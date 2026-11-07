use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

use anyhow::Context;
use aws_sdk_s3::{Client, error::ProvideErrorMetadata, primitives::ByteStream};
use bytes::Bytes;
use sha2::{Digest, Sha256};
use tokio::{
    io::AsyncWriteExt,
    sync::{Mutex, Semaphore},
};
use tracing::warn;

use crate::config::Config;
use crate::metrics::Metrics;

#[derive(Debug)]
pub struct SourceNotFound;

impl std::fmt::Display for SourceNotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("source image not found")
    }
}

impl std::error::Error for SourceNotFound {}

/// The source object is larger than `PERSIMMON_MAX_SOURCE_BYTES`. Retrying will not help.
#[derive(Debug)]
pub struct SourceTooLarge {
    /// The object's size, or the bytes read before the limit was crossed when S3 omits it.
    pub size: u64,
    pub limit: u64,
}

impl std::fmt::Display for SourceTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "source exceeds configured byte limit ({} > {} bytes)",
            self.size, self.limit
        )
    }
}

impl std::error::Error for SourceTooLarge {}

/// The local source cache has no room for this source because requests are using
/// the sources it already holds. Retrying once they finish will help.
#[derive(Debug)]
pub struct LocalCacheFull;

impl std::fmt::Display for LocalCacheFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("local source cache is full of sources in use")
    }
}

impl std::error::Error for LocalCacheFull {}

pub fn hash(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

struct LocalEntry {
    path: PathBuf,
    size: u64,
    accessed: Instant,
    active: Arc<AtomicUsize>,
}

pub struct SourceLease {
    pub path: PathBuf,
    active: Arc<AtomicUsize>,
    local: Arc<StdMutex<LocalState>>,
    local_limit: u64,
    metrics: Arc<Metrics>,
}

pub struct Derivative {
    pub body: ByteStream,
    pub size: Option<u64>,
}

struct RemoveOnDrop(Option<PathBuf>);

impl RemoveOnDrop {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl Drop for SourceLease {
    fn drop(&mut self) {
        if self.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            let mut local = self.local.lock().unwrap_or_else(|e| e.into_inner());
            evict_local(&mut local, self.local_limit, &self.metrics);
        }
    }
}

#[derive(Default)]
struct LocalState {
    entries: HashMap<String, LocalEntry>,
    /// Bytes of the files in `entries`.
    bytes: u64,
    /// Bytes set aside for downloads in progress, so `bytes + reserved` never
    /// exceeds the local cache limit.
    reserved: u64,
}

/// Disk space set aside for one download, returned when dropped unless the
/// download settles it into a cache entry.
struct Reservation {
    local: Arc<StdMutex<LocalState>>,
    bytes: u64,
}

impl Reservation {
    fn settle(&mut self, local: &mut LocalState) {
        local.reserved -= self.bytes;
        self.bytes = 0;
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.bytes > 0 {
            let mut local = self.local.lock().unwrap_or_else(|e| e.into_inner());
            local.reserved -= self.bytes;
        }
    }
}

/// Evicts unused sources, least recently used first, until cached and reserved
/// bytes together fit within `limit`, or only sources in use remain.
fn evict_local(local: &mut LocalState, limit: u64, metrics: &Metrics) {
    while local.bytes + local.reserved > limit {
        let victim = local
            .entries
            .iter()
            .filter(|(_, e)| e.active.load(Ordering::Acquire) == 0)
            .min_by_key(|(_, e)| e.accessed)
            .map(|(key, _)| key.clone());
        let Some(victim) = victim else {
            break;
        };
        let entry = local.entries.get(&victim).expect("victim exists");
        if let Err(e) = std::fs::remove_file(&entry.path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            warn!(error = %e, "could not evict local source file");
            break;
        }
        let entry = local.entries.remove(&victim).expect("victim exists");
        local.bytes -= entry.size;
        Metrics::increment(&metrics.source_evictions);
        metrics
            .source_cache_bytes
            .store(local.bytes, Ordering::Relaxed);
    }
}

/// Sets aside `bytes` of local cache space, evicting unused sources to make room.
/// Fails when sources in use and other downloads already fill the limit.
fn reserve_local(
    local: &Arc<StdMutex<LocalState>>,
    bytes: u64,
    limit: u64,
    metrics: &Metrics,
) -> anyhow::Result<Reservation> {
    let mut state = local.lock().unwrap_or_else(|e| e.into_inner());
    evict_local(&mut state, limit.saturating_sub(bytes), metrics);
    if state.bytes + state.reserved + bytes > limit {
        Metrics::increment(&metrics.source_cache_rejections);
        return Err(LocalCacheFull.into());
    }
    state.reserved += bytes;
    Ok(Reservation {
        local: local.clone(),
        bytes,
    })
}

async fn clear_local_cache_dir(dir: &std::path::Path) -> std::io::Result<()> {
    let mut entries = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.file_type().await?.is_file()
            && (name.ends_with(".jp2") || name.ends_with(".tmp") || name.starts_with("render-"))
        {
            tokio::fs::remove_file(entry.path()).await?;
        }
    }
    Ok(())
}

pub struct Storage {
    pub s3: Client,
    pub config: Arc<Config>,
    pub metrics: Arc<Metrics>,
    local: Arc<StdMutex<LocalState>>,
    source_locks: Vec<Mutex<()>>,
    downloads: Semaphore,
}

impl Storage {
    pub async fn new(s3: Client, config: Arc<Config>) -> anyhow::Result<Self> {
        tokio::fs::create_dir_all(&config.local_cache_dir).await?;
        // Old files have no in-memory access record. Start each server
        // process with a clean local cache for trustworthy accounting.
        clear_local_cache_dir(&config.local_cache_dir).await?;
        Ok(Self {
            s3,
            metrics: Arc::new(Metrics::default()),
            downloads: Semaphore::new(config.max_parallel_downloads),
            config,
            local: Arc::new(StdMutex::new(LocalState::default())),
            source_locks: (0..256).map(|_| Mutex::new(())).collect(),
        })
    }

    /// Deletes the downloaded sources once the server has stopped serving. A stopped
    /// container kept for rollback would otherwise hold them on the host's disk.
    pub async fn clear_local_cache(&self) -> std::io::Result<()> {
        clear_local_cache_dir(&self.config.local_cache_dir).await
    }

    fn marker_key(&self, identifier: &str) -> String {
        format!("{}markers/{}", self.config.cache_prefix, hash(identifier))
    }

    fn derivative_prefix(&self, identifier: &str) -> String {
        format!(
            "{}derivatives/{}/",
            self.config.cache_prefix,
            hash(identifier)
        )
    }

    pub fn derivative_key(&self, identifier: &str, generation: &str, raw_path: &str) -> String {
        let extension = raw_path.rsplit('.').next().unwrap_or("jpg");
        format!(
            "{}{}/{:}.{}",
            self.derivative_prefix(identifier),
            generation,
            hash(raw_path),
            extension
        )
    }

    pub async fn generation(&self, identifier: &str) -> anyhow::Result<String> {
        Metrics::increment(&self.metrics.s3_requests);
        match self
            .s3
            .head_object()
            .bucket(&self.config.cache_bucket)
            .key(self.marker_key(identifier))
            .send()
            .await
        {
            Ok(output) => Ok(output
                .metadata()
                .and_then(|m| m.get("generation"))
                .cloned()
                .unwrap_or_else(|| "0".into())),
            Err(e) if e.as_service_error().and_then(|e| e.code()) == Some("NotFound") => {
                Ok("0".into())
            }
            Err(e) => Err(e.into()),
        }
    }

    pub async fn purge(&self, identifier: &str) -> anyhow::Result<String> {
        let generation = uuid::Uuid::new_v4().simple().to_string();
        Metrics::increment(&self.metrics.s3_requests);
        self.s3
            .put_object()
            .bucket(&self.config.cache_bucket)
            .key(self.marker_key(identifier))
            .metadata("generation", &generation)
            .body(Bytes::new().into())
            .send()
            .await?;
        Metrics::increment(&self.metrics.purges);
        // The new marker invalidates all old keys immediately. Physical
        // deletion is best effort and can proceed after the response.
        Ok(generation)
    }

    pub async fn delete_old_derivatives(
        &self,
        identifier: &str,
        keep_generation: &str,
    ) -> anyhow::Result<()> {
        let prefix = self.derivative_prefix(identifier);
        let keep_prefix = format!("{prefix}{keep_generation}/");
        let mut continuation: Option<String> = None;
        let mut stale_keys = Vec::new();
        loop {
            Metrics::increment(&self.metrics.s3_requests);
            let page = self
                .s3
                .list_objects_v2()
                .bucket(&self.config.cache_bucket)
                .prefix(&prefix)
                .set_continuation_token(continuation.clone())
                .send()
                .await?;
            for object in page.contents() {
                if let Some(key) = object.key()
                    && !key.starts_with(&keep_prefix)
                {
                    stale_keys.push(key.to_owned());
                }
            }
            continuation = page.next_continuation_token().map(str::to_owned);
            if continuation.is_none() {
                break;
            }
        }
        for key in stale_keys {
            Metrics::increment(&self.metrics.s3_requests);
            self.s3
                .delete_object()
                .bucket(&self.config.cache_bucket)
                .key(key)
                .send()
                .await?;
        }
        Ok(())
    }

    pub async fn get_derivative(&self, key: &str) -> anyhow::Result<Option<Derivative>> {
        Metrics::increment(&self.metrics.s3_requests);
        match self
            .s3
            .get_object()
            .bucket(&self.config.cache_bucket)
            .key(key)
            .send()
            .await
        {
            Ok(output) => {
                Metrics::increment(&self.metrics.derivative_hits);
                Ok(Some(Derivative {
                    size: output.content_length().and_then(|n| u64::try_from(n).ok()),
                    body: output.body,
                }))
            }
            Err(e) if e.as_service_error().and_then(|e| e.code()) == Some("NoSuchKey") => {
                Metrics::increment(&self.metrics.derivative_misses);
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }

    pub async fn put_derivative(&self, key: &str, body: Bytes, mime: &str) -> anyhow::Result<()> {
        Metrics::increment(&self.metrics.s3_requests);
        let result = self
            .s3
            .put_object()
            .bucket(&self.config.cache_bucket)
            .key(key)
            .content_type(mime)
            .if_none_match("*")
            .body(body.into())
            .send()
            .await;
        match result {
            Ok(_) => {
                Metrics::increment(&self.metrics.derivative_writes);
                Ok(())
            }
            Err(e) if e.as_service_error().and_then(|e| e.code()) == Some("PreconditionFailed") => {
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Fetches up to the first `len` bytes of a source with a ranged GET and
    /// returns them with the object's total size, without downloading the rest.
    pub async fn source_prefix(&self, identifier: &str, len: u64) -> anyhow::Result<(Bytes, u64)> {
        Metrics::increment(&self.metrics.s3_requests);
        let output = match self
            .s3
            .get_object()
            .bucket(&self.config.source_bucket)
            .key(identifier)
            .range(format!("bytes=0-{}", len.saturating_sub(1)))
            .send()
            .await
        {
            Ok(output) => output,
            Err(e) if e.as_service_error().and_then(|e| e.code()) == Some("NoSuchKey") => {
                return Err(SourceNotFound.into());
            }
            // S3 cannot satisfy any byte range of an empty object.
            Err(e) if e.as_service_error().and_then(|e| e.code()) == Some("InvalidRange") => {
                return Ok((Bytes::new(), 0));
            }
            Err(e) => {
                return Err(e).with_context(|| format!("source image unavailable: {identifier}"));
            }
        };
        // A partial response reports the full size after the slash; a server that
        // ignores the range returns the whole (small) object instead.
        let total = match output.content_range().and_then(|r| r.rsplit_once('/')) {
            Some((_, total)) => total
                .parse::<u64>()
                .with_context(|| format!("unexpected Content-Range for {identifier}"))?,
            None => u64::try_from(output.content_length().unwrap_or(0))
                .context("negative source content length")?,
        };
        let limit = self.config.max_source_bytes;
        if total > limit {
            return Err(SourceTooLarge { size: total, limit }.into());
        }
        let mut body = output.body;
        let mut prefix = Vec::new();
        while let Some(chunk) = body.next().await {
            prefix.extend_from_slice(&chunk?);
            if prefix.len() as u64 >= len {
                prefix.truncate(usize::try_from(len).unwrap_or(usize::MAX));
                break;
            }
        }
        Ok((Bytes::from(prefix), total))
    }

    pub async fn source_path(
        &self,
        identifier: &str,
        generation: &str,
    ) -> anyhow::Result<SourceLease> {
        let cache_id = hash(&format!("{identifier}\0{generation}"));
        let path = self.config.local_cache_dir.join(format!("{cache_id}.jp2"));
        let lock_index = usize::from(u8::from_str_radix(&cache_id[..2], 16).expect("hash prefix"));
        let _guard = self.source_locks[lock_index].lock().await;
        {
            let mut local = self.local.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = local.entries.get_mut(&cache_id) {
                Metrics::increment(&self.metrics.source_hits);
                entry.accessed = Instant::now();
                entry.active.fetch_add(1, Ordering::AcqRel);
                let lease = SourceLease {
                    path: entry.path.clone(),
                    active: entry.active.clone(),
                    local: self.local.clone(),
                    local_limit: self.config.local_cache_limit,
                    metrics: self.metrics.clone(),
                };
                evict_local(&mut local, self.config.local_cache_limit, &self.metrics);
                return Ok(lease);
            }
        }
        Metrics::increment(&self.metrics.source_misses);
        let key = identifier.to_owned();
        let _download_permit = self.downloads.acquire().await?;
        Metrics::increment(&self.metrics.s3_requests);
        let output = match self
            .s3
            .get_object()
            .bucket(&self.config.source_bucket)
            .key(&key)
            .send()
            .await
        {
            Ok(output) => output,
            Err(e) if e.as_service_error().and_then(|e| e.code()) == Some("NoSuchKey") => {
                return Err(SourceNotFound.into());
            }
            Err(e) => return Err(e).with_context(|| format!("source image unavailable: {key}")),
        };
        // A source that could never fit in the local cache is as unservable as one
        // over the source limit. Configuration keeps the two consistent.
        let limit = self
            .config
            .max_source_bytes
            .min(self.config.local_cache_limit);
        let expected = match output.content_length() {
            Some(n) => {
                let size = u64::try_from(n).context("negative source content length")?;
                if size > limit {
                    return Err(SourceTooLarge { size, limit }.into());
                }
                Some(size)
            }
            None => None,
        };
        // Reserve the disk space before writing anything, so sources held by
        // requests waiting to render cannot push the cache past its limit.
        let mut reservation = reserve_local(
            &self.local,
            expected.unwrap_or(limit),
            self.config.local_cache_limit,
            &self.metrics,
        )?;
        let temp_path = self
            .config
            .local_cache_dir
            .join(format!("{cache_id}.{}.tmp", uuid::Uuid::new_v4()));
        let _temp_cleanup = RemoveOnDrop(Some(temp_path.clone()));
        let mut file = tokio::fs::File::create(&temp_path).await?;
        let mut body = output.body;
        let mut size = 0u64;
        async {
            while let Some(chunk) = body.next().await {
                let chunk = chunk?;
                size = size
                    .checked_add(chunk.len() as u64)
                    .context("source size overflow")?;
                if size > limit {
                    return Err(SourceTooLarge { size, limit }.into());
                }
                if size > reservation.bytes {
                    anyhow::bail!("source body is longer than its Content-Length");
                }
                file.write_all(&chunk).await?;
            }
            file.flush().await?;
            Ok::<(), anyhow::Error>(())
        }
        .await?;
        tokio::fs::rename(&temp_path, &path).await?;
        let mut final_cleanup = RemoveOnDrop(Some(path.clone()));
        Metrics::increment(&self.metrics.source_downloads);
        self.metrics
            .source_download_bytes
            .fetch_add(size, Ordering::Relaxed);
        let mut local = self.local.lock().unwrap_or_else(|e| e.into_inner());
        reservation.settle(&mut local);
        local.bytes += size;
        self.metrics
            .source_cache_bytes
            .store(local.bytes, Ordering::Relaxed);
        let active = Arc::new(AtomicUsize::new(1));
        local.entries.insert(
            cache_id,
            LocalEntry {
                path: path.clone(),
                size,
                accessed: Instant::now(),
                active: active.clone(),
            },
        );
        final_cleanup.disarm();
        evict_local(&mut local, self.config.local_cache_limit, &self.metrics);
        Ok(SourceLease {
            path,
            active,
            local: self.local.clone(),
            local_limit: self.config.local_cache_limit,
            metrics: self.metrics.clone(),
        })
    }

    pub async fn prune_derivatives(&self) -> anyhow::Result<(u64, usize)> {
        prune_derivatives(
            &self.s3,
            &self.config.cache_bucket,
            &self.config.cache_prefix,
            self.config.derivative_cache_limit,
            Some(&self.metrics),
        )
        .await
    }
}

pub async fn prune_derivatives(
    s3: &Client,
    cache_bucket: &str,
    cache_prefix: &str,
    limit: u64,
    metrics: Option<&Metrics>,
) -> anyhow::Result<(u64, usize)> {
    let prefix = format!("{cache_prefix}derivatives/");
    let mut entries = Vec::new();
    let mut continuation: Option<String> = None;
    let mut bytes = 0u64;
    loop {
        if let Some(metrics) = metrics {
            Metrics::increment(&metrics.s3_requests);
        }
        let page = s3
            .list_objects_v2()
            .bucket(cache_bucket)
            .prefix(&prefix)
            .set_continuation_token(continuation.clone())
            .send()
            .await?;
        for item in page.contents() {
            if let (Some(key), Some(modified), Some(size)) =
                (item.key(), item.last_modified(), item.size())
            {
                bytes = bytes.saturating_add(size.max(0) as u64);
                entries.push((*modified, key.to_owned(), size.max(0) as u64));
            }
        }
        continuation = page.next_continuation_token().map(str::to_owned);
        if continuation.is_none() {
            break;
        }
    }
    entries.sort_unstable_by_key(|a| a.0);
    let mut deleted = 0usize;
    for (_, key, size) in entries {
        if bytes <= limit {
            break;
        }
        if let Some(metrics) = metrics {
            Metrics::increment(&metrics.s3_requests);
        }
        s3.delete_object()
            .bucket(cache_bucket)
            .key(&key)
            .send()
            .await?;
        bytes = bytes.saturating_sub(size);
        deleted += 1;
    }
    Ok((bytes, deleted))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert(
        local: &Arc<StdMutex<LocalState>>,
        dir: &std::path::Path,
        key: &str,
        size: u64,
        active: usize,
    ) -> PathBuf {
        let path = dir.join(format!("{key}.jp2"));
        std::fs::write(&path, vec![0; size as usize]).unwrap();
        let mut state = local.lock().unwrap();
        state.bytes += size;
        state.entries.insert(
            key.into(),
            LocalEntry {
                path: path.clone(),
                size,
                accessed: Instant::now(),
                active: Arc::new(AtomicUsize::new(active)),
            },
        );
        path
    }

    #[test]
    fn reservation_evicts_unused_sources_and_refuses_when_sources_in_use_fill_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let metrics = Metrics::default();
        let local = Arc::new(StdMutex::new(LocalState::default()));
        let in_use = insert(&local, dir.path(), "in-use", 10, 1);
        let idle = insert(&local, dir.path(), "idle", 8, 0);

        // Evicting the idle source makes room.
        let reservation = reserve_local(&local, 8, 20, &metrics).unwrap();
        assert!(!idle.exists());
        assert!(in_use.exists());
        assert_eq!(local.lock().unwrap().reserved, 8);

        // The source in use and the outstanding reservation leave no room.
        let full = reserve_local(&local, 8, 20, &metrics).err().unwrap();
        assert!(full.is::<LocalCacheFull>());
        assert_eq!(metrics.source_cache_rejections.load(Ordering::Relaxed), 1);

        // An abandoned download returns its space.
        drop(reservation);
        assert_eq!(local.lock().unwrap().reserved, 0);
        let _reservation = reserve_local(&local, 8, 20, &metrics).unwrap();
        assert_eq!(metrics.source_evictions.load(Ordering::Relaxed), 1);
    }
}
