use std::{env, net::SocketAddr, path::PathBuf};

use crate::pipeline::EncodeSettings;

#[derive(Clone, Debug)]
pub struct Config {
    pub listen: SocketAddr,
    pub public_base_url: String,
    pub iiif_prefix: String,
    pub route_prefix: String,
    pub source_bucket: String,
    pub cache_bucket: String,
    pub s3_endpoint: Option<String>,
    pub cache_prefix: String,
    pub local_cache_dir: PathBuf,
    pub local_cache_limit: u64,
    pub derivative_cache_limit: u64,
    pub prune_interval_seconds: u64,
    pub max_output_pixels: u64,
    pub max_decode_pixels: u64,
    pub max_native_decode_pixels: u64,
    pub max_source_bytes: u64,
    pub max_temp_bitmap_bytes: u64,
    pub max_parallel_decodes: usize,
    pub max_parallel_downloads: usize,
    pub min_size: u32,
    pub min_tile_size: u32,
    pub encoding: EncodeSettings,
    pub kakadu_expand: PathBuf,
    pub kakadu_native: Option<PathBuf>,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    pub admin_user: String,
    pub admin_password: String,
}

pub struct PruneConfig {
    pub cache_bucket: String,
    pub s3_endpoint: Option<String>,
    pub cache_prefix: String,
    pub derivative_cache_limit: u64,
    pub prune_interval_seconds: u64,
}

fn var(name: &str) -> anyhow::Result<String> {
    env::var(name).map_err(|_| anyhow::anyhow!("missing required environment variable {name}"))
}

fn value<T: std::str::FromStr>(name: &str, default: T) -> anyhow::Result<T>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    match env::var(name) {
        Ok(v) => Ok(v.parse()?),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(e) => Err(e.into()),
    }
}

fn normalize_iiif_prefix(value: &str) -> anyhow::Result<String> {
    let prefix = value.strip_suffix('/').unwrap_or(value);
    if prefix.is_empty() {
        return Ok(String::new());
    }
    if !prefix.starts_with('/')
        || prefix.split('/').skip(1).any(|segment| {
            segment.is_empty() || segment == "." || segment == ".." || segment.contains('%')
        })
        || prefix.contains('?')
        || prefix.contains('#')
    {
        anyhow::bail!(
            "IIIF prefix must be an absolute path without empty, dot, or encoded segments"
        );
    }
    Ok(prefix.to_owned())
}

fn cache_prefix() -> anyhow::Result<String> {
    let prefix = env::var("PERSIMMON_CACHE_PREFIX").unwrap_or_else(|_| "persimmon/".into());
    if prefix.is_empty() || !prefix.ends_with('/') {
        anyhow::bail!("cache prefix must be nonempty and end with /");
    }
    Ok(prefix)
}

impl PruneConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let config = Self {
            cache_bucket: var("PERSIMMON_CACHE_BUCKET")?,
            s3_endpoint: env::var("PERSIMMON_S3_ENDPOINT").ok(),
            cache_prefix: cache_prefix()?,
            derivative_cache_limit: value("PERSIMMON_DERIVATIVE_CACHE_BYTES", 10_000_000_000)?,
            prune_interval_seconds: value("PERSIMMON_PRUNE_INTERVAL_SECONDS", 3_600)?,
        };
        if config.prune_interval_seconds == 0 {
            anyhow::bail!("prune interval must be positive");
        }
        Ok(config)
    }
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let tls_cert = env::var_os("PERSIMMON_TLS_CERT").map(PathBuf::from);
        let tls_key = env::var_os("PERSIMMON_TLS_KEY").map(PathBuf::from);
        if tls_cert.is_some() != tls_key.is_some() {
            anyhow::bail!("TLS certificate and key must be set together");
        }
        let public_base_url = var("PERSIMMON_PUBLIC_BASE_URL")?
            .trim_end_matches('/')
            .to_owned();
        if !public_base_url.starts_with("https://") && !public_base_url.starts_with("http://") {
            anyhow::bail!("public base URL must be http or https");
        }
        if tls_cert.is_none() && !public_base_url.starts_with("https://") {
            anyhow::bail!(
                "admin Basic authentication requires TLS or an HTTPS public base URL behind a trusted TLS proxy"
            );
        }
        let uri: http::Uri = public_base_url.parse()?;
        if uri.authority().is_none() || uri.query().is_some() {
            anyhow::bail!("public base URL must have a host and no query string");
        }
        let iiif_prefix = normalize_iiif_prefix(
            &env::var("PERSIMMON_IIIF_V3_PREFIX").unwrap_or_else(|_| "/v3".into()),
        )?;
        let route_prefix = format!("{}{}", uri.path().trim_end_matches('/'), iiif_prefix);
        let cache_prefix = cache_prefix()?;
        let defaults = EncodeSettings::default();
        let config = Self {
            listen: value("PERSIMMON_LISTEN", "0.0.0.0:3000".parse()?)?,
            public_base_url,
            iiif_prefix,
            route_prefix,
            source_bucket: var("PERSIMMON_SOURCE_BUCKET")?,
            cache_bucket: var("PERSIMMON_CACHE_BUCKET")?,
            s3_endpoint: env::var("PERSIMMON_S3_ENDPOINT").ok(),
            cache_prefix,
            local_cache_dir: PathBuf::from(
                env::var("PERSIMMON_LOCAL_CACHE_DIR")
                    .unwrap_or_else(|_| "/var/cache/persimmon".into()),
            ),
            local_cache_limit: value("PERSIMMON_LOCAL_CACHE_BYTES", 2_000_000_000)?,
            derivative_cache_limit: value("PERSIMMON_DERIVATIVE_CACHE_BYTES", 10_000_000_000)?,
            prune_interval_seconds: value("PERSIMMON_PRUNE_INTERVAL_SECONDS", 3_600)?,
            max_output_pixels: value("PERSIMMON_MAX_OUTPUT_PIXELS", 100_000_000)?,
            max_decode_pixels: value("PERSIMMON_MAX_DECODE_PIXELS", 100_000_000)?,
            max_native_decode_pixels: value("PERSIMMON_MAX_NATIVE_DECODE_PIXELS", 10_000_000)?,
            max_source_bytes: value("PERSIMMON_MAX_SOURCE_BYTES", 250_000_000)?,
            max_temp_bitmap_bytes: value("PERSIMMON_MAX_TEMP_BITMAP_BYTES", 2_000_000_000)?,
            max_parallel_decodes: value("PERSIMMON_MAX_PARALLEL_DECODES", 8)?,
            max_parallel_downloads: value("PERSIMMON_MAX_PARALLEL_DOWNLOADS", 8)?,
            min_size: value("PERSIMMON_MIN_SIZE", 64)?,
            min_tile_size: value("PERSIMMON_MIN_TILE_SIZE", 1024)?,
            encoding: EncodeSettings {
                jpeg_quality: value("PERSIMMON_JPEG_QUALITY", defaults.jpeg_quality)?,
                avif_quality: value("PERSIMMON_AVIF_QUALITY", defaults.avif_quality)?,
                avif_speed: value("PERSIMMON_AVIF_SPEED", defaults.avif_speed)?,
            },
            kakadu_expand: PathBuf::from(
                env::var("PERSIMMON_KDU_EXPAND").unwrap_or_else(|_| "kdu_expand".into()),
            ),
            kakadu_native: env::var_os("PERSIMMON_KAKADU_NATIVE").map(PathBuf::from),
            tls_cert,
            tls_key,
            admin_user: var("PERSIMMON_ADMIN_USER")?,
            admin_password: var("PERSIMMON_ADMIN_PASSWORD")?,
        };
        if config.max_output_pixels == 0
            || config.prune_interval_seconds == 0
            || config.max_source_bytes == 0
            || config.max_temp_bitmap_bytes < 1_048_576
            || config.max_temp_bitmap_bytes / 1_048_576 > u64::from(u32::MAX)
            || config.max_decode_pixels < config.max_output_pixels
            || config.max_native_decode_pixels == 0
            || config.max_native_decode_pixels > config.max_decode_pixels
            || config.max_parallel_decodes == 0
            || config.max_parallel_downloads == 0
            || config.min_tile_size == 0
        {
            anyhow::bail!("invalid resource limits");
        }
        let encoding = config.encoding;
        if !(1..=100).contains(&encoding.jpeg_quality)
            || !(1..=100).contains(&encoding.avif_quality)
            || !(1..=10).contains(&encoding.avif_speed)
        {
            anyhow::bail!("JPEG and AVIF quality must be 1 to 100, and AVIF speed must be 1 to 10");
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_iiif_prefix;

    #[test]
    fn iiif_prefix_is_optional_and_canonical() {
        assert_eq!(normalize_iiif_prefix("/v3/").unwrap(), "/v3");
        assert_eq!(normalize_iiif_prefix("/").unwrap(), "");
        assert_eq!(normalize_iiif_prefix("").unwrap(), "");
        for invalid in [
            "v3", "//", "/v3//", "/v3//x", "/../v3", "/v3/%2F", "/v3?x=1",
        ] {
            assert!(normalize_iiif_prefix(invalid).is_err(), "{invalid}");
        }
    }
}
