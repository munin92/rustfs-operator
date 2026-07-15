//! Bucket reconciliation.

use std::sync::Arc;

use kube::runtime::controller::Action;
use kube::runtime::finalizer::{Event, finalizer};
use kube::{Api, ResourceExt};

use rc_core::lifecycle::{LifecycleExpiration, LifecycleRule, LifecycleRuleStatus};

use super::{Context, FINALIZER, REQUEUE_OK, namespace_of, patch_status};
use crate::connection::provider_for;
use crate::crd::{
    Bucket, BucketSpec, DeletionPolicy, LifecycleRuleSpec, LifecycleStatus, ResourceStatus,
};
use crate::error::{Error, Result};
use crate::provider::RustFs;

/// Make the RustFS bucket match the spec. Pure logic, unit-testable.
pub async fn ensure_bucket(fs: &dyn RustFs, name: &str, spec: &BucketSpec) -> Result<()> {
    if !fs.bucket_exists(name).await? {
        fs.create_bucket(name).await?;
    }
    if let Some(versioning) = spec.versioning {
        let current = fs.get_versioning(name).await?.unwrap_or(false);
        if current != versioning {
            fs.set_versioning(name, versioning).await?;
        }
    }
    if let Some(quota) = spec.quota_bytes
        && fs.get_bucket_quota(name).await? != Some(quota)
    {
        fs.set_bucket_quota(name, quota).await?;
    }
    if let Some(rules) = &spec.lifecycle {
        let desired: Vec<LifecycleRule> = rules.iter().map(to_rc_rule).collect();
        if !lifecycle_eq(&fs.get_bucket_lifecycle(name).await?, &desired) {
            // An explicitly empty list means "no rules" — which is a DELETE, not a
            // PUT of an empty configuration (S3 rejects the latter).
            if desired.is_empty() {
                fs.delete_bucket_lifecycle(name).await?;
            } else {
                fs.set_bucket_lifecycle(name, desired).await?;
            }
        }
    }
    Ok(())
}

/// Map a CRD rule onto `rc-core`'s wire type.
fn to_rc_rule(spec: &LifecycleRuleSpec) -> LifecycleRule {
    LifecycleRule {
        id: spec.id.clone(),
        status: match spec.status {
            LifecycleStatus::Enabled => LifecycleRuleStatus::Enabled,
            LifecycleStatus::Disabled => LifecycleRuleStatus::Disabled,
        },
        prefix: spec.prefix.clone(),
        tags: None,
        expiration: spec.expiration_days.map(|days| LifecycleExpiration {
            days: Some(days),
            date: None,
        }),
        transition: None,
        noncurrent_version_expiration: None,
        noncurrent_version_transition: None,
        expired_object_delete_marker: None,
        abort_incomplete_multipart_upload_days: spec.abort_incomplete_multipart_upload_days,
    }
}

/// `rc-core`'s `LifecycleRule` has no `PartialEq`, and comparing only the fields the
/// CRD models would call a bucket "in sync" while a rule we do not model still differs.
/// Compare the serialized form instead — it covers every field on the wire.
fn lifecycle_eq(current: &[LifecycleRule], desired: &[LifecycleRule]) -> bool {
    match (serde_json::to_value(current), serde_json::to_value(desired)) {
        (Ok(a), Ok(b)) => a == b,
        // Unserializable means we cannot prove they match — rewrite rather than
        // silently leave a bucket unmanaged.
        _ => false,
    }
}

/// Remove the bucket if the deletion policy asks for it.
pub async fn cleanup_bucket(fs: &dyn RustFs, name: &str, spec: &BucketSpec) -> Result<()> {
    match spec.deletion_policy {
        DeletionPolicy::Delete => fs.delete_bucket(name).await,
        DeletionPolicy::Retain => Ok(()),
    }
}

pub async fn reconcile(obj: Arc<Bucket>, ctx: Arc<Context>) -> Result<Action> {
    let ns = namespace_of(obj.as_ref())?;
    let api: Api<Bucket> = Api::namespaced(ctx.client.clone(), &ns);
    finalizer(&api, FINALIZER, obj, |event| async {
        match event {
            Event::Apply(obj) => apply(obj, &ctx).await,
            Event::Cleanup(obj) => cleanup(obj, &ctx).await,
        }
    })
    .await
    .map_err(|e| Error::Finalizer(e.to_string()))
}

async fn apply(obj: Arc<Bucket>, ctx: &Context) -> Result<Action> {
    let ns = namespace_of(obj.as_ref())?;
    let api: Api<Bucket> = Api::namespaced(ctx.client.clone(), &ns);

    let result = async {
        let fs = provider_for(&ctx.client, &ns, &obj.spec.connection).await?;
        ensure_bucket(&fs, obj.bucket_name(), &obj.spec).await
    }
    .await;

    let status = match &result {
        Ok(()) => ResourceStatus::ready(obj.metadata.generation),
        Err(e) => ResourceStatus::error(obj.metadata.generation, e),
    };
    patch_status(&api, &obj.name_any(), &status).await;
    result.map(|()| Action::requeue(REQUEUE_OK))
}

async fn cleanup(obj: Arc<Bucket>, ctx: &Context) -> Result<Action> {
    if obj.spec.deletion_policy == DeletionPolicy::Retain {
        return Ok(Action::await_change());
    }
    let ns = namespace_of(obj.as_ref())?;
    let fs = provider_for(&ctx.client, &ns, &obj.spec.connection).await?;
    cleanup_bucket(&fs, obj.bucket_name(), &obj.spec).await?;
    Ok(Action::await_change())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::ConnectionRef;
    use crate::provider::MockRustFs;

    fn spec(versioning: Option<bool>, quota_bytes: Option<u64>) -> BucketSpec {
        BucketSpec {
            connection: ConnectionRef::local("conn"),
            bucket_name: None,
            versioning,
            quota_bytes,
            lifecycle: None,
            deletion_policy: DeletionPolicy::default(),
        }
    }

    fn spec_with_lifecycle(rules: Option<Vec<LifecycleRuleSpec>>) -> BucketSpec {
        BucketSpec {
            lifecycle: rules,
            ..spec(None, None)
        }
    }

    fn expire_after(id: &str, prefix: &str, days: i32) -> LifecycleRuleSpec {
        LifecycleRuleSpec {
            id: id.into(),
            status: LifecycleStatus::Enabled,
            prefix: Some(prefix.into()),
            expiration_days: Some(days),
            abort_incomplete_multipart_upload_days: None,
        }
    }

    #[tokio::test]
    async fn creates_missing_bucket() {
        let mut fs = MockRustFs::new();
        fs.expect_bucket_exists()
            .withf(|b| b == "demo")
            .return_once(|_| Ok(false));
        fs.expect_create_bucket()
            .withf(|b| b == "demo")
            .return_once(|_| Ok(()));

        ensure_bucket(&fs, "demo", &spec(None, None)).await.unwrap();
    }

    #[tokio::test]
    async fn existing_bucket_untouched_when_spec_matches() {
        let mut fs = MockRustFs::new();
        fs.expect_bucket_exists().return_once(|_| Ok(true));
        fs.expect_get_versioning().return_once(|_| Ok(Some(true)));
        fs.expect_get_bucket_quota().return_once(|_| Ok(Some(1024)));
        // no create/set expectations: any call would panic

        ensure_bucket(&fs, "demo", &spec(Some(true), Some(1024)))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn corrects_versioning_and_quota_drift() {
        let mut fs = MockRustFs::new();
        fs.expect_bucket_exists().return_once(|_| Ok(true));
        fs.expect_get_versioning().return_once(|_| Ok(None));
        fs.expect_set_versioning()
            .withf(|b, v| b == "demo" && *v)
            .return_once(|_, _| Ok(()));
        fs.expect_get_bucket_quota().return_once(|_| Ok(Some(5)));
        fs.expect_set_bucket_quota()
            .withf(|b, q| b == "demo" && *q == 2048)
            .return_once(|_, _| Ok(()));

        ensure_bucket(&fs, "demo", &spec(Some(true), Some(2048)))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn applies_lifecycle_when_bucket_has_none() {
        let mut fs = MockRustFs::new();
        fs.expect_bucket_exists().returning(|_| Ok(true));
        fs.expect_get_bucket_lifecycle().returning(|_| Ok(vec![]));
        fs.expect_set_bucket_lifecycle()
            .withf(|b, rules| {
                b == "cache"
                    && rules.len() == 1
                    && rules[0].id == "expire-proxy-cache"
                    && rules[0].prefix.as_deref() == Some("proxy-cache/")
                    && rules[0].expiration.as_ref().and_then(|e| e.days) == Some(1)
            })
            .times(1)
            .returning(|_, _| Ok(()));

        let spec = spec_with_lifecycle(Some(vec![expire_after(
            "expire-proxy-cache",
            "proxy-cache/",
            1,
        )]));
        ensure_bucket(&fs, "cache", &spec).await.unwrap();
    }

    #[tokio::test]
    async fn lifecycle_untouched_when_already_matching() {
        let rule = expire_after("expire-proxy-cache", "proxy-cache/", 1);
        let existing = to_rc_rule(&rule);

        let mut fs = MockRustFs::new();
        fs.expect_bucket_exists().returning(|_| Ok(true));
        fs.expect_get_bucket_lifecycle()
            .returning(move |_| Ok(vec![existing.clone()]));
        // No set/delete: a matching bucket must not be rewritten on every reconcile.
        fs.expect_set_bucket_lifecycle().never();
        fs.expect_delete_bucket_lifecycle().never();

        let spec = spec_with_lifecycle(Some(vec![rule]));
        ensure_bucket(&fs, "cache", &spec).await.unwrap();
    }

    #[tokio::test]
    async fn empty_lifecycle_list_deletes_the_configuration() {
        let mut fs = MockRustFs::new();
        fs.expect_bucket_exists().returning(|_| Ok(true));
        fs.expect_get_bucket_lifecycle()
            .returning(|_| Ok(vec![to_rc_rule(&expire_after("old", "tmp/", 7))]));
        // An empty list is a DELETE, not a PUT of an empty configuration.
        fs.expect_delete_bucket_lifecycle()
            .times(1)
            .returning(|_| Ok(()));
        fs.expect_set_bucket_lifecycle().never();

        let spec = spec_with_lifecycle(Some(vec![]));
        ensure_bucket(&fs, "cache", &spec).await.unwrap();
    }

    #[tokio::test]
    async fn unset_lifecycle_is_unmanaged() {
        let mut fs = MockRustFs::new();
        fs.expect_bucket_exists().returning(|_| Ok(true));
        // Unset means the operator must not even LOOK at the bucket's lifecycle —
        // otherwise adopting a bucket that already has rules would wipe them.
        fs.expect_get_bucket_lifecycle().never();
        fs.expect_set_bucket_lifecycle().never();
        fs.expect_delete_bucket_lifecycle().never();

        let spec = spec_with_lifecycle(None);
        ensure_bucket(&fs, "cache", &spec).await.unwrap();
    }

    #[tokio::test]
    async fn retain_policy_skips_remote_delete() {
        let fs = MockRustFs::new(); // any call panics
        let mut s = spec(None, None);
        s.deletion_policy = DeletionPolicy::Retain;
        cleanup_bucket(&fs, "demo", &s).await.unwrap();
    }

    #[tokio::test]
    async fn delete_policy_removes_bucket() {
        let mut fs = MockRustFs::new();
        fs.expect_delete_bucket()
            .withf(|b| b == "demo")
            .return_once(|_| Ok(()));
        cleanup_bucket(&fs, "demo", &spec(None, None))
            .await
            .unwrap();
    }
}
