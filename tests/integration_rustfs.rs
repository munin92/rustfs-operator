//! Integration tests against a real RustFS container (no Kubernetes).
//!
//! Run with: `cargo test --features integration --test integration_rustfs`

#![cfg(feature = "integration")]

mod common;

use rc_core::lifecycle::{LifecycleExpiration, LifecycleRule, LifecycleRuleStatus};
use rustfs_operator::provider::RustFs;
use serde_json::json;

#[tokio::test]
async fn provider_manages_buckets_policies_and_users() {
    common::setup_test_env();
    let (_container, endpoint) = common::start_rustfs().await;
    let fs = common::connect_when_ready(&endpoint).await;

    // --- buckets ---
    assert!(!fs.bucket_exists("it-bucket").await.unwrap());
    fs.create_bucket("it-bucket").await.unwrap();
    assert!(fs.bucket_exists("it-bucket").await.unwrap());

    fs.set_versioning("it-bucket", true).await.unwrap();
    assert_eq!(fs.get_versioning("it-bucket").await.unwrap(), Some(true));

    fs.set_bucket_quota("it-bucket", 10 * 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(
        fs.get_bucket_quota("it-bucket").await.unwrap(),
        Some(10 * 1024 * 1024)
    );

    // --- bucket lifecycle ---
    assert!(
        fs.get_bucket_lifecycle("it-bucket")
            .await
            .unwrap()
            .is_empty(),
        "a fresh bucket must report no lifecycle rules, not an error"
    );

    let rule = LifecycleRule {
        id: "expire-cache".into(),
        status: LifecycleRuleStatus::Enabled,
        prefix: Some("cache/".into()),
        tags: None,
        expiration: Some(LifecycleExpiration {
            days: Some(1),
            date: None,
        }),
        transition: None,
        noncurrent_version_expiration: None,
        noncurrent_version_transition: None,
        expired_object_delete_marker: None,
        abort_incomplete_multipart_upload_days: Some(1),
    };
    fs.set_bucket_lifecycle("it-bucket", vec![rule])
        .await
        .unwrap();

    let got = fs.get_bucket_lifecycle("it-bucket").await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].id, "expire-cache");
    assert_eq!(got[0].prefix.as_deref(), Some("cache/"));
    assert_eq!(got[0].expiration.as_ref().and_then(|e| e.days), Some(1));
    assert_eq!(got[0].abort_incomplete_multipart_upload_days, Some(1));

    fs.delete_bucket_lifecycle("it-bucket").await.unwrap();
    assert!(
        fs.get_bucket_lifecycle("it-bucket")
            .await
            .unwrap()
            .is_empty(),
        "lifecycle configuration must be gone after delete"
    );

    // --- policies ---
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["s3:GetObject", "s3:PutObject"],
            "Resource": ["arn:aws:s3:::it-bucket/*"]
        }]
    });
    assert!(fs.get_policy("it-policy").await.unwrap().is_none());
    fs.put_policy("it-policy", &document.to_string())
        .await
        .unwrap();
    let fetched = fs.get_policy("it-policy").await.unwrap().unwrap();
    assert!(
        rustfs_operator::reconcile::policy::documents_equivalent(
            &fetched.parse_document().unwrap(),
            &document
        ),
        "stored policy should be semantically equal to the submitted one"
    );

    // --- users ---
    assert!(fs.get_user("it-user").await.unwrap().is_none());
    fs.create_user("it-user", "it-secret-key-123")
        .await
        .unwrap();
    let user = fs.get_user("it-user").await.unwrap().unwrap();
    assert_eq!(user.access_key, "it-user");

    fs.set_user_policies("it-user", &["it-policy".into()])
        .await
        .unwrap();
    let user = fs.get_user("it-user").await.unwrap().unwrap();
    assert!(user.policies().contains(&"it-policy".to_string()));

    // --- access keys (service accounts): issued as the user, usable for S3 ---
    let allow_all = json!({
        "Version": "2012-10-17",
        "Statement": [
            {"Effect": "Allow", "Action": ["s3:*"], "Resource": ["arn:aws:s3:::*"]},
            {"Effect": "Allow", "Action": ["admin:CreateServiceAccount", "admin:ListServiceAccounts", "admin:RemoveServiceAccount"], "Resource": ["arn:aws:s3:::*"]}
        ]
    });
    fs.put_policy("it-allow-all", &allow_all.to_string())
        .await
        .unwrap();
    fs.set_user_policies("it-user", &["it-allow-all".into()])
        .await
        .unwrap();
    let (sa_ak, sa_sk) = (
        "ITSAKEY1234567890ABC",
        "it-sa-secret-key-12345678901234567890",
    );
    assert!(
        fs.get_access_key("it-user", "it-secret-key-123", sa_ak)
            .await
            .unwrap()
            .is_none()
    );
    fs.create_access_key(
        "it-user",
        "it-secret-key-123",
        sa_ak,
        sa_sk,
        Some("integration".into()),
        None,
    )
    .await
    .unwrap();
    assert!(
        fs.get_access_key("it-user", "it-secret-key-123", sa_ak)
            .await
            .unwrap()
            .is_some()
    );
    // the issued credentials authenticate and authorize real S3 calls
    let sa_provider = rustfs_operator::provider::RustFsProvider::connect(
        rustfs_operator::provider::ConnectionInfo {
            endpoint: endpoint.clone(),
            access_key: sa_ak.into(),
            secret_key: sa_sk.into(),
            region: "us-east-1".into(),
            insecure: false,
        },
    )
    .await
    .unwrap();
    assert!(sa_provider.bucket_exists("it-bucket").await.unwrap());
    fs.delete_access_key("it-user", "it-secret-key-123", sa_ak)
        .await
        .unwrap();
    assert!(
        fs.get_access_key("it-user", "it-secret-key-123", sa_ak)
            .await
            .unwrap()
            .is_none()
    );
    fs.set_user_status("it-user", false).await.unwrap();

    // replace semantics: setting a different set drops the old attachment
    let document2 = json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": "Allow", "Action": ["s3:ListBucket"], "Resource": ["arn:aws:s3:::it-bucket"]}]
    });
    fs.put_policy("it-policy-2", &document2.to_string())
        .await
        .unwrap();
    fs.set_user_policies("it-user", &["it-policy-2".into()])
        .await
        .unwrap();
    let user = fs.get_user("it-user").await.unwrap().unwrap();
    assert_eq!(user.policies(), vec!["it-policy-2".to_string()]);
    // no longer attached to it-user, so deletable now
    fs.delete_policy("it-allow-all").await.unwrap();

    // --- cleanup, exercising delete paths ---
    fs.delete_user("it-user").await.unwrap();
    assert!(fs.get_user("it-user").await.unwrap().is_none());
    fs.delete_policy("it-policy").await.unwrap();
    assert!(fs.get_policy("it-policy").await.unwrap().is_none());
    fs.delete_policy("it-policy-2").await.unwrap();
    fs.delete_bucket("it-bucket").await.unwrap();
    assert!(!fs.bucket_exists("it-bucket").await.unwrap());

    // deletes are tolerant of already-absent resources
    fs.delete_user("it-user").await.unwrap();
    fs.delete_policy("it-policy").await.unwrap();
    fs.delete_bucket("it-bucket").await.unwrap();
}
