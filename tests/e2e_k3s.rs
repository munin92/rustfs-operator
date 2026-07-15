//! End-to-end test: real k3s cluster + real RustFS server, controllers
//! running in-process against the k3s API.
//!
//! Run with: `cargo test --features e2e --test e2e_k3s`

#![cfg(feature = "e2e")]

mod common;

use std::time::Duration;

use k8s_openapi::api::core::v1::{Namespace, Secret};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::api::{DeleteParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Api, Client, CustomResourceExt};
use serde_json::json;
use testcontainers::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::k3s::{K3s, KUBE_SECURE_PORT};

use rustfs_operator::crd::{
    AccessKey, AccessKeySpec, Bucket, BucketSpec, ClusterConnection, ClusterConnectionSpec,
    ConnectionRef, DeletionPolicy, Policy, PolicySpec, SecretKeyRef, User, UserSpec,
};
use rustfs_operator::provider::RustFs;
use rustfs_operator::reconcile;

const K3S_IMAGE: &str = "ghcr.io/openprojectx/dockerhub/rancher/k3s";
const K3S_TAG: &str = "v1.34.9-k3s1";
const NS: &str = "default";

async fn eventually<F, Fut>(what: &str, timeout_secs: u64, check: F)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        if check().await {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out after {timeout_secs}s waiting for: {what}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn k3s_client(conf_dir: &std::path::Path, port: u16, yaml: &str) -> Client {
    let _ = conf_dir; // kubeconfig already read from the mount
    let mut kubeconfig = Kubeconfig::from_yaml(yaml).expect("invalid kubeconfig from k3s");
    for named_cluster in &mut kubeconfig.clusters {
        if let Some(cluster) = &mut named_cluster.cluster {
            cluster.server = Some(format!("https://127.0.0.1:{port}"));
        }
    }
    let config = kube::Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default())
        .await
        .expect("failed to build kube config");
    Client::try_from(config).expect("failed to build kube client")
}

fn connection_secret(name: &str, endpoint: &str) -> Secret {
    let mut secret = Secret::default();
    secret.metadata.name = Some(name.into());
    secret.string_data = Some(
        [
            ("endpoint".to_string(), endpoint.to_string()),
            ("accessKey".to_string(), common::ADMIN_KEY.to_string()),
            ("secretKey".to_string(), common::ADMIN_SECRET.to_string()),
        ]
        .into(),
    );
    secret
}

#[tokio::test]
async fn operator_reconciles_crs_against_rustfs() {
    common::setup_test_env();

    // --- infrastructure: RustFS + k3s side by side ---
    let (_rustfs, endpoint) = common::start_rustfs().await;
    let fs = common::connect_when_ready(&endpoint).await;

    let conf_dir = tempfile::tempdir().expect("tempdir");
    let k3s = K3s::default()
        .with_conf_mount(conf_dir.path())
        .with_name(K3S_IMAGE)
        .with_tag(K3S_TAG)
        .with_privileged(true)
        .with_userns_mode("host")
        .with_startup_timeout(Duration::from_secs(180))
        .start()
        .await
        .expect("failed to start k3s container");
    let kube_port = k3s
        .get_host_port_ipv4(KUBE_SECURE_PORT)
        .await
        .expect("k3s port not mapped");
    let kube_yaml = k3s.image().read_kube_config().expect("read kubeconfig");
    let client = k3s_client(conf_dir.path(), kube_port, &kube_yaml).await;

    // --- install CRDs and wait until they are served ---
    let crds: Api<CustomResourceDefinition> = Api::all(client.clone());
    for crd in [
        Bucket::crd(),
        User::crd(),
        Policy::crd(),
        AccessKey::crd(),
        ClusterConnection::crd(),
    ] {
        crds.create(&PostParams::default(), &crd)
            .await
            .expect("failed to create CRD");
    }
    let buckets: Api<Bucket> = Api::namespaced(client.clone(), NS);
    let users: Api<User> = Api::namespaced(client.clone(), NS);
    let policies: Api<Policy> = Api::namespaced(client.clone(), NS);
    eventually("CRDs are served", 60, || async {
        buckets.list(&Default::default()).await.is_ok()
            && users.list(&Default::default()).await.is_ok()
            && policies.list(&Default::default()).await.is_ok()
    })
    .await;

    // --- run the operator in-process ---
    let controller = tokio::spawn(reconcile::run_all(client.clone()));

    // --- connection + user credentials secrets ---
    let secrets: Api<Secret> = Api::namespaced(client.clone(), NS);
    secrets
        .create(
            &PostParams::default(),
            &connection_secret("rustfs-conn", &endpoint),
        )
        .await
        .expect("create connection secret");
    let mut user_creds = Secret::default();
    user_creds.metadata.name = Some("e2e-user-creds".into());
    user_creds.string_data =
        Some([("password".to_string(), "e2e-password-123".to_string())].into());
    secrets
        .create(&PostParams::default(), &user_creds)
        .await
        .expect("create user creds secret");

    let conn = ConnectionRef::local("rustfs-conn");

    // --- apply CRs: Policy, User (attached), Bucket ---
    policies
        .create(
            &PostParams::default(),
            &Policy::new(
                "e2e-policy",
                PolicySpec {
                    connection: conn.clone(),
                    policy_name: None,
                    document: json!({
                        "Version": "2012-10-17",
                        "Statement": [
                            {
                                "Effect": "Allow",
                                "Action": ["s3:GetObject"],
                                "Resource": ["arn:aws:s3:::e2e-bucket/*"]
                            },
                            // required for the user to manage its own access keys
                            {
                                "Effect": "Allow",
                                "Action": [
                                    "admin:CreateServiceAccount",
                                    "admin:ListServiceAccounts",
                                    "admin:RemoveServiceAccount"
                                ],
                                "Resource": ["arn:aws:s3:::*"]
                            }
                        ]
                    }),
                    deletion_policy: DeletionPolicy::Delete,
                },
            ),
        )
        .await
        .expect("create Policy CR");

    users
        .create(
            &PostParams::default(),
            &User::new(
                "e2e-user",
                UserSpec {
                    connection: conn.clone(),
                    username: None,
                    password_ref: SecretKeyRef {
                        name: "e2e-user-creds".into(),
                        key: None,
                    },
                    policies: vec!["e2e-policy".into()],
                    enabled: true,
                    deletion_policy: DeletionPolicy::Delete,
                },
            ),
        )
        .await
        .expect("create User CR");

    buckets
        .create(
            &PostParams::default(),
            &Bucket::new(
                "e2e-bucket",
                BucketSpec {
                    connection: conn.clone(),
                    bucket_name: None,
                    versioning: Some(true),
                    quota_bytes: Some(10 * 1024 * 1024),
                    lifecycle: None,
                    deletion_policy: DeletionPolicy::Delete,
                },
            ),
        )
        .await
        .expect("create Bucket CR");

    // --- observe convergence in RustFS ---
    eventually("bucket exists in RustFS", 120, || async {
        fs.bucket_exists("e2e-bucket").await.unwrap_or(false)
    })
    .await;
    eventually("bucket versioning enabled", 60, || async {
        fs.get_versioning("e2e-bucket").await.ok().flatten() == Some(true)
    })
    .await;
    eventually("bucket quota set", 60, || async {
        fs.get_bucket_quota("e2e-bucket").await.ok().flatten() == Some(10 * 1024 * 1024)
    })
    .await;
    eventually("policy exists in RustFS", 60, || async {
        fs.get_policy("e2e-policy").await.ok().flatten().is_some()
    })
    .await;
    eventually("user exists with policy attached", 60, || async {
        match fs.get_user("e2e-user").await {
            Ok(Some(u)) => u.policies().contains(&"e2e-policy".to_string()),
            _ => false,
        }
    })
    .await;

    // --- CR statuses report ready ---
    eventually("Bucket CR is ready", 60, || async {
        buckets
            .get("e2e-bucket")
            .await
            .ok()
            .and_then(|b| b.status)
            .map(|s| s.ready)
            .unwrap_or(false)
    })
    .await;
    eventually("User CR is ready", 60, || async {
        users
            .get("e2e-user")
            .await
            .ok()
            .and_then(|u| u.status)
            .map(|s| s.ready)
            .unwrap_or(false)
    })
    .await;
    eventually("Policy CR is ready", 60, || async {
        policies
            .get("e2e-policy")
            .await
            .ok()
            .and_then(|p| p.status)
            .map(|s| s.ready)
            .unwrap_or(false)
    })
    .await;

    // --- AccessKey: issue credentials for the user, written to a Secret ---
    let access_keys: Api<AccessKey> = Api::namespaced(client.clone(), NS);
    access_keys
        .create(
            &PostParams::default(),
            &AccessKey::new(
                "e2e-ak",
                AccessKeySpec {
                    connection: conn.clone(),
                    user: "e2e-user".into(),
                    password_ref: SecretKeyRef {
                        name: "e2e-user-creds".into(),
                        key: None,
                    },
                    access_key: None,
                    description: Some("e2e".into()),
                    policy: None,
                    target_secret_name: None,
                    deletion_policy: DeletionPolicy::Delete,
                },
            ),
        )
        .await
        .expect("create AccessKey CR");
    eventually("AccessKey CR is ready", 60, || async {
        access_keys
            .get("e2e-ak")
            .await
            .ok()
            .and_then(|k| k.status)
            .map(|s| s.ready)
            .unwrap_or(false)
    })
    .await;

    // credentials Secret exists and the key is registered in RustFS
    let creds = secrets
        .get("e2e-ak-credentials")
        .await
        .expect("credentials secret");
    let get_key = |k: &str| {
        String::from_utf8(creds.data.as_ref().unwrap().get(k).unwrap().0.clone()).unwrap()
    };
    let issued_ak = get_key("accessKey");
    assert_eq!(get_key("endpoint"), endpoint);
    assert!(!get_key("secretKey").is_empty());
    assert!(
        fs.get_access_key("e2e-user", "e2e-password-123", &issued_ak)
            .await
            .unwrap()
            .is_some(),
        "issued key must exist in RustFS"
    );

    // deleting the CR revokes the key; the Secret is GC'd via ownerReference
    access_keys
        .delete("e2e-ak", &DeleteParams::default())
        .await
        .expect("delete AccessKey CR");
    eventually("access key revoked in RustFS", 60, || async {
        matches!(
            fs.get_access_key("e2e-user", "e2e-password-123", &issued_ak)
                .await,
            Ok(None)
        )
    })
    .await;
    eventually("credentials secret garbage-collected", 60, || async {
        secrets.get("e2e-ak-credentials").await.is_err()
    })
    .await;

    // --- deletion: finalizers clean up the remote resources ---
    buckets
        .delete("e2e-bucket", &DeleteParams::default())
        .await
        .expect("delete Bucket CR");
    eventually("bucket removed from RustFS", 120, || async {
        !fs.bucket_exists("e2e-bucket").await.unwrap_or(true)
    })
    .await;

    users
        .delete("e2e-user", &DeleteParams::default())
        .await
        .expect("delete User CR");
    eventually("user removed from RustFS", 60, || async {
        matches!(fs.get_user("e2e-user").await, Ok(None))
    })
    .await;

    policies
        .delete("e2e-policy", &DeleteParams::default())
        .await
        .expect("delete Policy CR");
    eventually("policy removed from RustFS", 60, || async {
        matches!(fs.get_policy("e2e-policy").await, Ok(None))
    })
    .await;

    // ================= ClusterConnection =================
    // Admin credentials live only in the operator's namespace (the kube
    // client's default namespace, "default" in this test).
    let mut admin_creds = Secret::default();
    admin_creds.metadata.name = Some("rustfs-admin".into());
    admin_creds.string_data = Some(
        [
            ("accessKey".to_string(), common::ADMIN_KEY.to_string()),
            ("secretKey".to_string(), common::ADMIN_SECRET.to_string()),
        ]
        .into(),
    );
    secrets
        .create(&PostParams::default(), &admin_creds)
        .await
        .expect("create admin creds secret");

    let cluster_connections: Api<ClusterConnection> = Api::all(client.clone());
    cluster_connections
        .create(
            &PostParams::default(),
            &ClusterConnection::new(
                "prod",
                ClusterConnectionSpec {
                    endpoint: endpoint.clone(),
                    credentials_secret_ref: "rustfs-admin".into(),
                    region: None,
                    insecure: false,
                    allowed_namespaces: Some(vec![NS.into()]),
                },
            ),
        )
        .await
        .expect("create ClusterConnection");

    // happy path: bucket via clusterRef converges and cleans up
    buckets
        .create(
            &PostParams::default(),
            &Bucket::new(
                "cc-bucket",
                BucketSpec {
                    connection: ConnectionRef::cluster("prod"),
                    bucket_name: None,
                    versioning: None,
                    quota_bytes: None,
                    lifecycle: None,
                    deletion_policy: DeletionPolicy::Delete,
                },
            ),
        )
        .await
        .expect("create clusterRef Bucket CR");
    eventually("clusterRef bucket exists in RustFS", 120, || async {
        fs.bucket_exists("cc-bucket").await.unwrap_or(false)
    })
    .await;
    buckets
        .delete("cc-bucket", &DeleteParams::default())
        .await
        .expect("delete clusterRef Bucket CR");
    eventually("clusterRef bucket removed from RustFS", 120, || async {
        !fs.bucket_exists("cc-bucket").await.unwrap_or(true)
    })
    .await;

    // denied path: a namespace outside allowedNamespaces is rejected
    let namespaces: Api<Namespace> = Api::all(client.clone());
    let mut team_ns = Namespace::default();
    team_ns.metadata.name = Some("team-x".into());
    namespaces
        .create(&PostParams::default(), &team_ns)
        .await
        .expect("create team-x namespace");
    let denied_buckets: Api<Bucket> = Api::namespaced(client.clone(), "team-x");
    denied_buckets
        .create(
            &PostParams::default(),
            &Bucket::new(
                "denied-bucket",
                BucketSpec {
                    connection: ConnectionRef::cluster("prod"),
                    bucket_name: None,
                    versioning: None,
                    quota_bytes: None,
                    // Retain: cleanup must not need the (denied) connection
                    lifecycle: None,
                    deletion_policy: DeletionPolicy::Retain,
                },
            ),
        )
        .await
        .expect("create denied Bucket CR");
    eventually("denied bucket reports not allowed", 60, || async {
        denied_buckets
            .get("denied-bucket")
            .await
            .ok()
            .and_then(|b| b.status)
            .map(|s| !s.ready && s.message.unwrap_or_default().contains("not allowed"))
            .unwrap_or(false)
    })
    .await;
    assert!(
        !fs.bucket_exists("denied-bucket").await.unwrap(),
        "denied bucket must not be created in RustFS"
    );

    controller.abort();
}
