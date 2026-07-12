//! Shared helpers for container-backed tests.

use std::time::Duration;

use rustfs_operator::provider::{ConnectionInfo, RustFs, RustFsProvider};
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

pub const RUSTFS_IMAGE: (&str, &str) = (
    "ghcr.io/openprojectx/dockerhub/rustfs/rustfs",
    "1.0.0-beta.8",
);

/// Container tests talk to 127.0.0.1; a system proxy must not intercept
/// those connections (kube also refuses proxy env vars unless its proxy
/// feature is enabled). Call once at the start of each test.
pub fn setup_test_env() {
    rustfs_operator::install_crypto_provider();
    for var in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        // SAFETY: called at test start, before other threads read the env
        unsafe { std::env::remove_var(var) };
    }
}
pub const ADMIN_KEY: &str = "rustfsadmin";
pub const ADMIN_SECRET: &str = "rustfsadmin";

/// Start a RustFS container and return it with the S3 endpoint reachable
/// from the host (test process).
pub async fn start_rustfs() -> (ContainerAsync<GenericImage>, String) {
    let container = GenericImage::new(RUSTFS_IMAGE.0, RUSTFS_IMAGE.1)
        .with_exposed_port(9000.tcp())
        .with_wait_for(WaitFor::seconds(1))
        .with_env_var("RUSTFS_ACCESS_KEY", ADMIN_KEY)
        .with_env_var("RUSTFS_SECRET_KEY", ADMIN_SECRET)
        .start()
        .await
        .expect("failed to start rustfs container");
    let port = container
        .get_host_port_ipv4(9000)
        .await
        .expect("rustfs port not mapped");
    (container, format!("http://127.0.0.1:{port}"))
}

pub fn connection_info(endpoint: &str) -> ConnectionInfo {
    ConnectionInfo {
        endpoint: endpoint.to_string(),
        access_key: ADMIN_KEY.into(),
        secret_key: ADMIN_SECRET.into(),
        region: "us-east-1".into(),
        insecure: false,
    }
}

/// Connect to RustFS, retrying until the server answers S3 calls.
pub async fn connect_when_ready(endpoint: &str) -> RustFsProvider {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let provider = RustFsProvider::connect(connection_info(endpoint))
            .await
            .expect("failed to build provider");
        match provider.bucket_exists("readiness-probe").await {
            Ok(_) => return provider,
            Err(e) if std::time::Instant::now() > deadline => {
                panic!("rustfs not ready within 60s: {e}");
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
}
