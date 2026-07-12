//! Controllers for Bucket, User and Policy resources.

pub mod access_key;
pub mod bucket;
pub mod policy;
pub mod user;

use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use kube::api::{Patch, PatchParams};
use kube::runtime::Controller;
use kube::runtime::watcher;
use kube::{Api, Client, Resource, ResourceExt};
use serde::de::DeserializeOwned;
use tracing::{info, warn};

use crate::crd::{AccessKey, Bucket, Policy, User};
use crate::error::{Error, Result};

/// How often a successfully reconciled resource is re-checked for drift.
pub const REQUEUE_OK: Duration = Duration::from_secs(300);
/// Retry delay after a failed reconciliation.
pub const REQUEUE_ERR: Duration = Duration::from_secs(15);
/// Finalizer added to every managed resource.
pub const FINALIZER: &str = "rustfs.com/cleanup";

/// Shared state passed to every reconciler.
pub struct Context {
    pub client: Client,
}

/// Patch `.status` on a resource (best effort; reconcile result wins).
pub async fn patch_status<K, S>(api: &Api<K>, name: &str, status: &S)
where
    K: Resource + Clone + DeserializeOwned + Debug,
    S: serde::Serialize,
{
    let patch = serde_json::json!({ "status": status });
    if let Err(e) = api
        .patch_status(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
    {
        warn!(name, error = %e, "failed to patch status");
    }
}

fn error_policy<K>(
    obj: Arc<K>,
    err: &Error,
    _ctx: Arc<Context>,
) -> kube::runtime::controller::Action
where
    K: Resource<DynamicType = ()>,
{
    warn!(
        kind = %K::kind(&()),
        name = %obj.meta().name.as_deref().unwrap_or_default(),
        error = %err,
        "reconciliation failed"
    );
    kube::runtime::controller::Action::requeue(REQUEUE_ERR)
}

/// Run the Bucket, User and Policy controllers until shutdown.
pub async fn run_all(client: Client) -> Result<()> {
    let ctx = Arc::new(Context {
        client: client.clone(),
    });

    let buckets = Controller::new(
        Api::<Bucket>::all(client.clone()),
        watcher::Config::default(),
    )
    .shutdown_on_signal()
    .run(bucket::reconcile, error_policy, ctx.clone())
    .for_each(|res| async move {
        if let Ok((obj, _)) = res {
            info!(kind = "Bucket", name = %obj.name, "reconciled");
        }
    });

    let users = Controller::new(Api::<User>::all(client.clone()), watcher::Config::default())
        .shutdown_on_signal()
        .run(user::reconcile, error_policy, ctx.clone())
        .for_each(|res| async move {
            if let Ok((obj, _)) = res {
                info!(kind = "User", name = %obj.name, "reconciled");
            }
        });

    let policies = Controller::new(
        Api::<Policy>::all(client.clone()),
        watcher::Config::default(),
    )
    .shutdown_on_signal()
    .run(policy::reconcile, error_policy, ctx.clone())
    .for_each(|res| async move {
        if let Ok((obj, _)) = res {
            info!(kind = "Policy", name = %obj.name, "reconciled");
        }
    });

    let access_keys = Controller::new(
        Api::<AccessKey>::all(client.clone()),
        watcher::Config::default(),
    )
    .shutdown_on_signal()
    .run(access_key::reconcile, error_policy, ctx)
    .for_each(|res| async move {
        if let Ok((obj, _)) = res {
            info!(kind = "AccessKey", name = %obj.name, "reconciled");
        }
    });

    tokio::join!(buckets, users, policies, access_keys);
    Ok(())
}

/// Namespace of an object, erroring on the (impossible) cluster-scoped case.
fn namespace_of<K: ResourceExt>(obj: &K) -> Result<String> {
    obj.namespace()
        .ok_or_else(|| Error::Spec("resource has no namespace".into()))
}
