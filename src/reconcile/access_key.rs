//! AccessKey (service account) reconciliation.
//!
//! RustFS mints service accounts for the calling identity only, so the
//! operator authenticates as the owning user (username + password from the
//! spec) to issue keys. The generated AK/SK pair is written to a Secret in
//! the CR's namespace, owner-referenced for garbage collection. Secret keys
//! are only obtainable at creation time: if the target Secret disappears,
//! the key is revoked and reissued.

use std::sync::Arc;

use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::api::{Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::finalizer::{Event, finalizer};
use kube::{Api, Resource, ResourceExt};
use rand::Rng;

use super::{Context, FINALIZER, REQUEUE_OK, namespace_of, patch_status};
use crate::connection::{provider_for, secret_key_value};
use crate::crd::{AccessKey, AccessKeySpec, AccessKeyStatus, DeletionPolicy};
use crate::error::{Error, Result};
use crate::provider::RustFs;

fn random_key(len: usize, charset: &[u8]) -> String {
    let mut rng = rand::rng();
    (0..len)
        .map(|_| charset[rng.random_range(0..charset.len())] as char)
        .collect()
}

fn generate_access_key() -> String {
    random_key(20, b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567")
}

fn generate_secret_key() -> String {
    random_key(
        40,
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
    )
}

/// Outcome of [`ensure_access_key`].
#[derive(Debug, PartialEq)]
pub enum KeyOutcome {
    /// Key exists and the credentials Secret is intact.
    Kept { access_key: String },
    /// A new credential pair was issued and must be written to the Secret.
    Issued {
        access_key: String,
        secret_key: String,
    },
}

/// Converge the service account in RustFS. `known_ak` is the key recorded in
/// status (or pinned in the spec); `secret_intact` says whether the target
/// Secret still holds credentials for `known_ak`.
pub async fn ensure_access_key(
    fs: &dyn RustFs,
    username: &str,
    password: &str,
    known_ak: Option<&str>,
    secret_intact: bool,
    spec: &AccessKeySpec,
) -> Result<KeyOutcome> {
    let policy = spec
        .policy
        .as_ref()
        .map(|doc| {
            serde_json::to_string(doc)
                .map_err(|e| Error::Spec(format!("policy document is not valid JSON: {e}")))
        })
        .transpose()?;

    if let Some(ak) = known_ak {
        let exists = fs.get_access_key(username, password, ak).await?.is_some();
        if exists && secret_intact {
            return Ok(KeyOutcome::Kept {
                access_key: ak.to_string(),
            });
        }
        if exists {
            // Secret lost; the SK is unrecoverable — revoke and reissue.
            fs.delete_access_key(username, password, ak).await?;
        }
        let secret_key = generate_secret_key();
        fs.create_access_key(
            username,
            password,
            ak,
            &secret_key,
            spec.description.clone(),
            policy,
        )
        .await?;
        return Ok(KeyOutcome::Issued {
            access_key: ak.to_string(),
            secret_key,
        });
    }

    let access_key = generate_access_key();
    let secret_key = generate_secret_key();
    fs.create_access_key(
        username,
        password,
        &access_key,
        &secret_key,
        spec.description.clone(),
        policy,
    )
    .await?;
    Ok(KeyOutcome::Issued {
        access_key,
        secret_key,
    })
}

pub async fn cleanup_access_key(
    fs: &dyn RustFs,
    username: &str,
    password: &str,
    access_key: Option<&str>,
    spec: &AccessKeySpec,
) -> Result<()> {
    match (spec.deletion_policy, access_key) {
        (DeletionPolicy::Delete, Some(ak)) => fs.delete_access_key(username, password, ak).await,
        _ => Ok(()),
    }
}

pub async fn reconcile(obj: Arc<AccessKey>, ctx: Arc<Context>) -> Result<Action> {
    let ns = namespace_of(obj.as_ref())?;
    let api: Api<AccessKey> = Api::namespaced(ctx.client.clone(), &ns);
    finalizer(&api, FINALIZER, obj, |event| async {
        match event {
            Event::Apply(obj) => apply(obj, &ctx).await,
            Event::Cleanup(obj) => cleanup(obj, &ctx).await,
        }
    })
    .await
    .map_err(|e| Error::Finalizer(e.to_string()))
}

/// Whether the target Secret currently holds credentials for `ak`.
async fn secret_intact(secrets: &Api<Secret>, name: &str, ak: &str) -> bool {
    match secrets.get(name).await {
        Ok(secret) => {
            let stored = secret
                .data
                .as_ref()
                .and_then(|d| d.get("accessKey"))
                .and_then(|v| String::from_utf8(v.0.clone()).ok());
            stored.as_deref() == Some(ak)
                && secret
                    .data
                    .as_ref()
                    .is_some_and(|d| d.contains_key("secretKey"))
        }
        Err(_) => false,
    }
}

async fn write_credentials_secret(
    secrets: &Api<Secret>,
    obj: &AccessKey,
    name: &str,
    access_key: &str,
    secret_key: &str,
    endpoint: &str,
) -> Result<()> {
    let owner = OwnerReference {
        api_version: AccessKey::api_version(&()).to_string(),
        kind: AccessKey::kind(&()).to_string(),
        name: obj.name_any(),
        uid: obj.metadata.uid.clone().unwrap_or_default(),
        controller: Some(true),
        ..Default::default()
    };
    let secret = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": { "name": name, "ownerReferences": [owner] },
        "type": "Opaque",
        "stringData": {
            "accessKey": access_key,
            "secretKey": secret_key,
            "endpoint": endpoint,
        }
    });
    secrets
        .patch(
            name,
            &PatchParams::apply("rustfs-operator").force(),
            &Patch::Apply(&secret),
        )
        .await?;
    Ok(())
}

async fn apply(obj: Arc<AccessKey>, ctx: &Context) -> Result<Action> {
    let ns = namespace_of(obj.as_ref())?;
    let api: Api<AccessKey> = Api::namespaced(ctx.client.clone(), &ns);
    let secrets: Api<Secret> = Api::namespaced(ctx.client.clone(), &ns);
    let secret_name = obj.target_secret_name();

    let result: Result<String> = async {
        let password =
            secret_key_value(&ctx.client, &ns, &obj.spec.password_ref, "password").await?;
        let fs = provider_for(&ctx.client, &ns, &obj.spec.connection).await?;

        let known_ak = obj
            .spec
            .access_key
            .clone()
            .or_else(|| obj.status.as_ref().and_then(|s| s.access_key.clone()));
        let intact = match &known_ak {
            Some(ak) => secret_intact(&secrets, &secret_name, ak).await,
            None => false,
        };

        let outcome = ensure_access_key(
            &fs,
            &obj.spec.user,
            &password,
            known_ak.as_deref(),
            intact,
            &obj.spec,
        )
        .await?;
        if let KeyOutcome::Issued {
            access_key,
            secret_key,
        } = &outcome
        {
            write_credentials_secret(
                &secrets,
                &obj,
                &secret_name,
                access_key,
                secret_key,
                &fs.endpoint(),
            )
            .await?;
        }
        Ok(match outcome {
            KeyOutcome::Kept { access_key } | KeyOutcome::Issued { access_key, .. } => access_key,
        })
    }
    .await;

    let status = match &result {
        Ok(ak) => AccessKeyStatus {
            ready: true,
            message: Some("reconciled".into()),
            observed_generation: obj.metadata.generation,
            access_key: Some(ak.clone()),
        },
        Err(e) => AccessKeyStatus {
            ready: false,
            message: Some(e.to_string()),
            observed_generation: obj.metadata.generation,
            access_key: obj.status.as_ref().and_then(|s| s.access_key.clone()),
        },
    };
    patch_status(&api, &obj.name_any(), &status).await;
    result.map(|_| Action::requeue(REQUEUE_OK))
}

async fn cleanup(obj: Arc<AccessKey>, ctx: &Context) -> Result<Action> {
    if obj.spec.deletion_policy == DeletionPolicy::Retain {
        return Ok(Action::await_change());
    }
    let ns = namespace_of(obj.as_ref())?;
    let known_ak = obj
        .spec
        .access_key
        .clone()
        .or_else(|| obj.status.as_ref().and_then(|s| s.access_key.clone()));
    if known_ak.is_none() {
        return Ok(Action::await_change());
    }
    let password = secret_key_value(&ctx.client, &ns, &obj.spec.password_ref, "password").await?;
    let fs = provider_for(&ctx.client, &ns, &obj.spec.connection).await?;
    cleanup_access_key(
        &fs,
        &obj.spec.user,
        &password,
        known_ak.as_deref(),
        &obj.spec,
    )
    .await?;
    Ok(Action::await_change())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::{ConnectionRef, SecretKeyRef};
    use crate::provider::MockRustFs;
    use rc_core::admin::ServiceAccount;

    fn spec(access_key: Option<&str>) -> AccessKeySpec {
        AccessKeySpec {
            connection: ConnectionRef::cluster("prod"),
            user: "spark".into(),
            password_ref: SecretKeyRef {
                name: "spark-password".into(),
                key: None,
            },
            access_key: access_key.map(str::to_string),
            description: None,
            policy: None,
            target_secret_name: None,
            deletion_policy: DeletionPolicy::default(),
        }
    }

    #[tokio::test]
    async fn issues_generated_key_when_none_known() {
        let mut fs = MockRustFs::new();
        fs.expect_create_access_key()
            .withf(|user, pwd, ak, sk, _, _| {
                user == "spark" && pwd == "pw" && ak.len() == 20 && sk.len() == 40
            })
            .return_once(|_, _, _, _, _, _| Ok(()));

        match ensure_access_key(&fs, "spark", "pw", None, false, &spec(None))
            .await
            .unwrap()
        {
            KeyOutcome::Issued { access_key, .. } => assert_eq!(access_key.len(), 20),
            other => panic!("expected Issued, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn existing_key_with_intact_secret_is_kept() {
        let mut fs = MockRustFs::new();
        fs.expect_get_access_key()
            .withf(|_, _, ak| ak == "AK1")
            .return_once(|_, _, _| Ok(Some(ServiceAccount::new("AK1"))));

        let outcome = ensure_access_key(&fs, "spark", "pw", Some("AK1"), true, &spec(Some("AK1")))
            .await
            .unwrap();
        assert_eq!(
            outcome,
            KeyOutcome::Kept {
                access_key: "AK1".into()
            }
        );
    }

    #[tokio::test]
    async fn lost_secret_revokes_and_reissues() {
        let mut fs = MockRustFs::new();
        fs.expect_get_access_key()
            .return_once(|_, _, _| Ok(Some(ServiceAccount::new("AK1"))));
        fs.expect_delete_access_key()
            .withf(|_, _, ak| ak == "AK1")
            .return_once(|_, _, _| Ok(()));
        fs.expect_create_access_key()
            .withf(|_, _, ak, _, _, _| ak == "AK1")
            .return_once(|_, _, _, _, _, _| Ok(()));

        match ensure_access_key(&fs, "spark", "pw", Some("AK1"), false, &spec(None))
            .await
            .unwrap()
        {
            KeyOutcome::Issued { access_key, .. } => assert_eq!(access_key, "AK1"),
            other => panic!("expected Issued, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn key_deleted_serverside_is_recreated() {
        let mut fs = MockRustFs::new();
        fs.expect_get_access_key().return_once(|_, _, _| Ok(None));
        fs.expect_create_access_key()
            .withf(|_, _, ak, _, _, _| ak == "AK1")
            .return_once(|_, _, _, _, _, _| Ok(()));

        match ensure_access_key(&fs, "spark", "pw", Some("AK1"), true, &spec(Some("AK1")))
            .await
            .unwrap()
        {
            KeyOutcome::Issued { access_key, .. } => assert_eq!(access_key, "AK1"),
            other => panic!("expected Issued, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn retain_policy_keeps_remote_key() {
        let fs = MockRustFs::new(); // any call panics
        let mut s = spec(Some("AK1"));
        s.deletion_policy = DeletionPolicy::Retain;
        cleanup_access_key(&fs, "spark", "pw", Some("AK1"), &s)
            .await
            .unwrap();
    }
}
