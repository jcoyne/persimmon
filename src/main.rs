use std::{sync::Arc, time::Duration};

use aws_sdk_s3::Client;
use persimmon::{
    config::{Config, PruneConfig},
    server::{self, AppState},
    storage::{Storage, prune_derivatives},
};
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .json()
        .init();
    let command = std::env::args().nth(1);
    if matches!(command.as_deref(), Some("prune-cache" | "prune-cache-loop")) {
        let config = PruneConfig::from_env()?;
        let aws = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
        let mut s3_config = aws_sdk_s3::config::Builder::from(&aws);
        if let Some(endpoint) = &config.s3_endpoint {
            s3_config = s3_config.endpoint_url(endpoint).force_path_style(true);
        }
        let s3 = Client::from_conf(s3_config.build());
        loop {
            let (bytes, deleted) = prune_derivatives(
                &s3,
                &config.cache_bucket,
                &config.cache_prefix,
                config.derivative_cache_limit,
                None,
            )
            .await?;
            info!(bytes, deleted, "derivative cleanup complete");
            if command.as_deref() == Some("prune-cache") {
                break;
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(config.prune_interval_seconds)) => {},
                _ = tokio::signal::ctrl_c() => break,
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
    let state = Arc::new(AppState::new(config.clone(), storage));
    let app = server::router(state);
    info!(listen = %config.listen, "persimmon starting");
    if let (Some(cert), Some(key)) = (&config.tls_cert, &config.tls_key) {
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key).await?;
        axum_server::bind_rustls(config.listen, tls)
            .serve(app.into_make_service())
            .await?;
    } else {
        axum_server::bind(config.listen)
            .serve(app.into_make_service())
            .await?;
    }
    Ok(())
}
