use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
pub struct Metrics {
    pub s3_requests: AtomicU64,
    pub source_hits: AtomicU64,
    pub source_misses: AtomicU64,
    pub source_downloads: AtomicU64,
    pub source_download_bytes: AtomicU64,
    pub source_evictions: AtomicU64,
    pub source_cache_bytes: AtomicU64,
    pub derivative_hits: AtomicU64,
    pub derivative_misses: AtomicU64,
    pub derivative_writes: AtomicU64,
    pub render_count: AtomicU64,
    pub native_renders: AtomicU64,
    pub command_renders: AtomicU64,
    pub command_fallback_renders: AtomicU64,
    pub render_duration_ns: AtomicU64,
    pub purges: AtomicU64,
    pub errors: AtomicU64,
}

impl Metrics {
    pub fn increment(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> String {
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        format!(
            concat!(
                "# TYPE persimmon_s3_requests_total counter\n",
                "persimmon_s3_requests_total {}\n",
                "# TYPE persimmon_source_cache_hits_total counter\n",
                "persimmon_source_cache_hits_total {}\n",
                "# TYPE persimmon_source_cache_misses_total counter\n",
                "persimmon_source_cache_misses_total {}\n",
                "# TYPE persimmon_source_downloads_total counter\n",
                "persimmon_source_downloads_total {}\n",
                "# TYPE persimmon_source_download_bytes_total counter\n",
                "persimmon_source_download_bytes_total {}\n",
                "# TYPE persimmon_source_evictions_total counter\n",
                "persimmon_source_evictions_total {}\n",
                "# TYPE persimmon_source_cache_bytes gauge\n",
                "persimmon_source_cache_bytes {}\n",
                "# TYPE persimmon_derivative_cache_hits_total counter\n",
                "persimmon_derivative_cache_hits_total {}\n",
                "# TYPE persimmon_derivative_cache_misses_total counter\n",
                "persimmon_derivative_cache_misses_total {}\n",
                "# TYPE persimmon_derivative_writes_total counter\n",
                "persimmon_derivative_writes_total {}\n",
                "# TYPE persimmon_renders_total counter\n",
                "persimmon_renders_total {}\n",
                "# TYPE persimmon_native_renders_total counter\n",
                "persimmon_native_renders_total {}\n",
                "# TYPE persimmon_command_renders_total counter\n",
                "persimmon_command_renders_total {}\n",
                "# TYPE persimmon_command_fallback_renders_total counter\n",
                "persimmon_command_fallback_renders_total {}\n",
                "# TYPE persimmon_render_duration_seconds_total counter\n",
                "persimmon_render_duration_seconds_total {:.9}\n",
                "# TYPE persimmon_purges_total counter\n",
                "persimmon_purges_total {}\n",
                "# TYPE persimmon_errors_total counter\n",
                "persimmon_errors_total {}\n",
            ),
            read(&self.s3_requests),
            read(&self.source_hits),
            read(&self.source_misses),
            read(&self.source_downloads),
            read(&self.source_download_bytes),
            read(&self.source_evictions),
            read(&self.source_cache_bytes),
            read(&self.derivative_hits),
            read(&self.derivative_misses),
            read(&self.derivative_writes),
            read(&self.render_count),
            read(&self.native_renders),
            read(&self.command_renders),
            read(&self.command_fallback_renders),
            read(&self.render_duration_ns) as f64 / 1_000_000_000.0,
            read(&self.purges),
            read(&self.errors),
        )
    }
}
