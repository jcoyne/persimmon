use std::{future::Future, sync::Arc, time::Duration};

use aws_sdk_s3::Client;
use persimmon::{
    config::{Config, PruneConfig},
    server::{self, AppState},
    storage::{Storage, prune_derivatives},
};
use tracing::{info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .json()
        .init();
    // Dropping the guard at the end of main delivers any queued notices.
    #[cfg(feature = "honeybadger")]
    let _honeybadger = init_honeybadger()?;
    let command = std::env::args().nth(1);
    if matches!(command.as_deref(), Some("prune-cache" | "prune-cache-loop")) {
        let config = PruneConfig::from_env()?;
        let aws = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
        let mut s3_config = aws_sdk_s3::config::Builder::from(&aws);
        if let Some(endpoint) = &config.s3_endpoint {
            s3_config = s3_config.endpoint_url(endpoint).force_path_style(true);
        }
        let s3 = Client::from_conf(s3_config.build());
        let shutdown = shutdown_signal()?;
        tokio::pin!(shutdown);
        loop {
            // A pass stopped partway leaves the cache over its target until the next pass.
            let (bytes, deleted) = tokio::select! {
                result = prune_derivatives(
                    &s3,
                    &config.cache_bucket,
                    &config.cache_prefix,
                    config.derivative_cache_limit,
                    None,
                ) => result?,
                _ = &mut shutdown => {
                    info!("shutdown signal received; stopping derivative cleanup");
                    break;
                }
            };
            info!(bytes, deleted, "derivative cleanup complete");
            if command.as_deref() == Some("prune-cache") {
                break;
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(config.prune_interval_seconds)) => {},
                _ = &mut shutdown => break,
            }
        }
        return Ok(());
    }
    let config = Arc::new(Config::from_env()?);
    let aws = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let mut s3_config = aws_sdk_s3::config::Builder::from(&aws);
    if let Some(endpoint) = &config.s3_endpoint {
        s3_config = s3_config.endpoint_url(endpoint).force_path_style(true);
    }
    let s3 = Client::from_conf(s3_config.build());
    let storage = Arc::new(Storage::new(s3, config.clone()).await?);
    let state = Arc::new(AppState::new(config.clone(), storage.clone()));
    let app = server::router(state);
    let handle = axum_server::Handle::new();
    let shutdown = shutdown_signal()?;
    tokio::spawn({
        let handle = handle.clone();
        async move {
            shutdown.await;
            info!("shutdown signal received; finishing in-flight requests");
            // No deadline here: the orchestrator's stop timeout bounds the wait.
            handle.graceful_shutdown(None);
        }
    });
    info!(listen = %config.listen, "persimmon starting");
    if let (Some(cert), Some(key)) = (&config.tls_cert, &config.tls_key) {
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key).await?;
        axum_server::bind_rustls(config.listen, tls)
            .handle(handle)
            .serve(app.into_make_service())
            .await?;
    } else {
        axum_server::bind(config.listen)
            .handle(handle)
            .serve(app.into_make_service())
            .await?;
    }
    if let Err(e) = storage.clear_local_cache().await {
        warn!(error = %e, "could not clear local source cache");
    }
    info!("persimmon stopped");
    Ok(())
}

/// Starts error reporting only when an API key is configured, so the same image runs without it.
/// The SDK reads the key and its other settings from `HONEYBADGER_*` environment variables.
#[cfg(feature = "honeybadger")]
fn init_honeybadger() -> anyhow::Result<Option<honeybadger::Guard>> {
    if std::env::var_os("HONEYBADGER_API_KEY").is_none_or(|key| key.is_empty()) {
        return Ok(None);
    }
    let config = honeybadger::Config::builder().build()?;
    let guard = honeybadger::init(config)?;
    info!("honeybadger error reporting enabled");
    Ok(Some(guard))
}

/// Resolves on SIGTERM or SIGINT. The handlers are installed before this returns, so a signal
/// that arrives before the future is first polled is not lost. Installing a SIGTERM handler
/// matters in containers: as PID 1 the process would otherwise ignore it and wait to be killed.
fn shutdown_signal() -> std::io::Result<impl Future<Output = ()>> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate())?;
        let mut interrupt = signal(SignalKind::interrupt())?;
        Ok(async move {
            tokio::select! {
                _ = terminate.recv() => {},
                _ = interrupt.recv() => {},
            }
        })
    }
    #[cfg(not(unix))]
    {
        Ok(async {
            let _ = tokio::signal::ctrl_c().await;
        })
    }
}
