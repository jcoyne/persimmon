use std::{
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
};

use aws_sdk_s3::{
    Client,
    config::{BehaviorVersion, Credentials, Region},
};
use bytes::Bytes;
use persimmon::{config::Config, storage::Storage};

fn test_config(
    source_bucket: String,
    cache_bucket: String,
    local_cache_dir: PathBuf,
) -> Arc<Config> {
    Arc::new(Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        public_base_url: "https://example.test".into(),
        iiif_prefix: "/v3".into(),
        route_prefix: "/v3".into(),
        source_bucket,
        cache_bucket,
        s3_endpoint: None,
        cache_prefix: "persimmon/".into(),
        local_cache_dir,
        local_cache_limit: 20,
        derivative_cache_limit: 5,
        prune_interval_seconds: 3_600,
        max_output_pixels: 100,
        max_decode_pixels: 100,
        max_native_decode_pixels: 100,
        max_source_bytes: 1_000,
        max_temp_bitmap_bytes: 1_048_576,
        max_parallel_decodes: 8,
        max_parallel_downloads: 8,
        min_size: 64,
        min_tile_size: 1024,
        kakadu_expand: "kdu_expand".into(),
        kakadu_native: None,
        tls_cert: None,
        tls_key: None,
        admin_user: "test".into(),
        admin_password: "test".into(),
    })
}

#[tokio::test]
#[ignore = "requires a local S3-compatible endpoint in PERSIMMON_TEST_S3_ENDPOINT"]
async fn concurrent_source_reuse_and_cross_instance_purge() {
    let endpoint = std::env::var("PERSIMMON_TEST_S3_ENDPOINT").unwrap();
    let client = Client::from_conf(
        aws_sdk_s3::config::Builder::new()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new("us-east-1"))
            .credentials_provider(Credentials::new("test", "test", None, None, "test"))
            .endpoint_url(endpoint)
            .force_path_style(true)
            .build(),
    );
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let source_bucket = format!("persimmon-source-{nonce}");
    let cache_bucket = format!("persimmon-cache-{nonce}");
    for bucket in [&source_bucket, &cache_bucket] {
        client.create_bucket().bucket(bucket).send().await.unwrap();
    }
    client
        .put_object()
        .bucket(&source_bucket)
        .key("a/b.jp2")
        .body(Bytes::from_static(b"old source").into())
        .send()
        .await
        .unwrap();

    let first_dir = tempfile::tempdir().unwrap();
    let second_dir = tempfile::tempdir().unwrap();
    let first = Arc::new(
        Storage::new(
            client.clone(),
            test_config(
                source_bucket.clone(),
                cache_bucket.clone(),
                first_dir.path().into(),
            ),
        )
        .await
        .unwrap(),
    );
    let second = Storage::new(
        client.clone(),
        test_config(
            source_bucket.clone(),
            cache_bucket,
            second_dir.path().into(),
        ),
    )
    .await
    .unwrap();
    let initial_generation = first.generation("a/b").await.unwrap();
    assert_eq!(initial_generation, "0");

    let mut requests = Vec::new();
    for _ in 0..16 {
        let first = first.clone();
        requests.push(tokio::spawn(async move {
            let lease = first.source_path("a/b", "0").await.unwrap();
            tokio::fs::read(&lease.path).await.unwrap()
        }));
    }
    for request in requests {
        assert_eq!(request.await.unwrap(), b"old source");
    }
    assert_eq!(first.metrics.source_downloads.load(Ordering::Relaxed), 1);
    let second_source = second.source_path("a/b", "0").await.unwrap();
    assert_eq!(
        tokio::fs::read(&second_source.path).await.unwrap(),
        b"old source"
    );

    let path = "/v3/a%2Fb/full/max/0/default.jpg";
    let old_key = first.derivative_key("a/b", "0", path);
    first
        .put_derivative(&old_key, Bytes::from_static(b"old image"), "image/jpeg")
        .await
        .unwrap();
    assert_eq!(
        second.get_derivative(&old_key).await.unwrap().unwrap(),
        "old image"
    );

    let new_generation = first.purge("a/b").await.unwrap();
    assert_ne!(new_generation, initial_generation);
    assert_eq!(second.generation("a/b").await.unwrap(), new_generation);
    let new_key = second.derivative_key("a/b", &new_generation, path);
    assert!(second.get_derivative(&new_key).await.unwrap().is_none());

    client
        .put_object()
        .bucket(&source_bucket)
        .key("a/b.jp2")
        .body(Bytes::from_static(b"new source").into())
        .send()
        .await
        .unwrap();
    for storage in [&*first, &second] {
        let lease = storage.source_path("a/b", &new_generation).await.unwrap();
        assert_eq!(tokio::fs::read(&lease.path).await.unwrap(), b"new source");
    }
    assert_eq!(first.metrics.source_downloads.load(Ordering::Relaxed), 2);
    assert_eq!(second.metrics.source_downloads.load(Ordering::Relaxed), 2);

    client
        .put_object()
        .bucket(&source_bucket)
        .key("c.jp2")
        .body(Bytes::from_static(b"cccccccc").into())
        .send()
        .await
        .unwrap();
    let third_source = first.source_path("c", "0").await.unwrap();
    assert_eq!(
        tokio::fs::read(&third_source.path).await.unwrap(),
        b"cccccccc"
    );
    assert_eq!(first.metrics.source_evictions.load(Ordering::Relaxed), 1);
    assert_eq!(first.metrics.source_cache_bytes.load(Ordering::Relaxed), 18);

    let held_source = first.source_path("a/b", &new_generation).await.unwrap();
    client
        .put_object()
        .bucket(&source_bucket)
        .key("d.jp2")
        .body(Bytes::from_static(b"dddddddd").into())
        .send()
        .await
        .unwrap();
    let newest_source = first.source_path("d", "0").await.unwrap();
    assert_eq!(first.metrics.source_cache_bytes.load(Ordering::Relaxed), 26);
    let third_path = third_source.path.clone();
    drop(third_source);
    assert_eq!(first.metrics.source_cache_bytes.load(Ordering::Relaxed), 18);
    assert_eq!(first.metrics.source_evictions.load(Ordering::Relaxed), 2);
    assert!(!third_path.exists());
    drop(held_source);
    drop(newest_source);

    first
        .delete_old_derivatives("a/b", &new_generation)
        .await
        .unwrap();
    assert!(second.get_derivative(&old_key).await.unwrap().is_none());

    for (tail, content) in [("default.jpg", "aaaa"), ("gray.jpg", "bbbb")] {
        let key = first.derivative_key(
            "a/b",
            &new_generation,
            &format!("/v3/a%2Fb/full/max/0/{tail}"),
        );
        first
            .put_derivative(
                &key,
                Bytes::copy_from_slice(content.as_bytes()),
                "image/jpeg",
            )
            .await
            .unwrap();
    }
    let (remaining_bytes, deleted) = first.prune_derivatives().await.unwrap();
    assert!(remaining_bytes <= 5);
    assert_eq!(deleted, 1);

    // A single oversized source may be used while leased, but must not
    // remain cached after its final request finishes.
    let oversized_dir = tempfile::tempdir().unwrap();
    let mut oversized_config = (*test_config(
        source_bucket,
        first.config.cache_bucket.clone(),
        oversized_dir.path().into(),
    ))
    .clone();
    oversized_config.local_cache_limit = 5;
    let oversized = Storage::new(client, Arc::new(oversized_config))
        .await
        .unwrap();
    let lease = oversized.source_path("c", "0").await.unwrap();
    let path = lease.path.clone();
    assert!(path.exists());
    assert_eq!(
        oversized.metrics.source_cache_bytes.load(Ordering::Relaxed),
        8
    );
    drop(lease);
    assert!(!path.exists());
    assert_eq!(
        oversized.metrics.source_cache_bytes.load(Ordering::Relaxed),
        0
    );
    assert_eq!(
        oversized.metrics.source_evictions.load(Ordering::Relaxed),
        1
    );
}
