//! Resolve connection references into RustFS clients.
//!
//! Two forms are supported (exactly one must be set on the CR):
//! - `secretRef`: Secret in the CR's namespace with `endpoint`/`accessKey`/
//!   `secretKey` (self-service).
//! - `clusterRef`: cluster-scoped `ClusterConnection`; its credentials
//!   Secret is read from the operator's own namespace (the kube client's
//!   default namespace), so app namespaces never hold admin credentials.

use k8s_openapi::api::core::v1::Secret;
use kube::{Api, Client};

use crate::crd::{ClusterConnection, ConnectionRef, SecretKeyRef};
use crate::error::{Error, Result};
use crate::provider::{ConnectionInfo, RustFsProvider};

fn secret_value(secret: &Secret, key: &str) -> Option<String> {
    secret
        .data
        .as_ref()
        .and_then(|d| d.get(key))
        .and_then(|v| String::from_utf8(v.0.clone()).ok())
}

fn required(secret: &Secret, key: &str, secret_name: &str) -> Result<String> {
    secret_value(secret, key)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| Error::Connection(format!("secret '{secret_name}' is missing key '{key}'")))
}

async fn get_secret(client: &Client, namespace: &str, name: &str) -> Result<Secret> {
    let secrets: Api<Secret> = Api::namespaced(client.clone(), namespace);
    secrets
        .get(name)
        .await
        .map_err(|e| Error::Connection(format!("cannot read secret '{namespace}/{name}': {e}")))
}

async fn from_local_secret(client: &Client, namespace: &str, name: &str) -> Result<ConnectionInfo> {
    let secret = get_secret(client, namespace, name).await?;
    Ok(ConnectionInfo {
        endpoint: required(&secret, "endpoint", name)?,
        access_key: required(&secret, "accessKey", name)?,
        secret_key: required(&secret, "secretKey", name)?,
        region: secret_value(&secret, "region").unwrap_or_else(|| "us-east-1".into()),
        insecure: secret_value(&secret, "insecure").as_deref() == Some("true"),
    })
}

async fn from_cluster_connection(
    client: &Client,
    cr_namespace: &str,
    name: &str,
) -> Result<ConnectionInfo> {
    let connections: Api<ClusterConnection> = Api::all(client.clone());
    let conn = connections
        .get(name)
        .await
        .map_err(|e| Error::Connection(format!("cannot read ClusterConnection '{name}': {e}")))?;
    if !conn.spec.allows_namespace(cr_namespace) {
        return Err(Error::Connection(format!(
            "namespace '{cr_namespace}' is not allowed to use ClusterConnection '{name}'"
        )));
    }

    // Credentials always come from the operator's own namespace.
    let operator_ns = client.default_namespace();
    let secret = get_secret(client, operator_ns, &conn.spec.credentials_secret_ref).await?;
    Ok(ConnectionInfo {
        endpoint: conn.spec.endpoint.clone(),
        access_key: required(&secret, "accessKey", &conn.spec.credentials_secret_ref)?,
        secret_key: required(&secret, "secretKey", &conn.spec.credentials_secret_ref)?,
        region: conn
            .spec
            .region
            .clone()
            .unwrap_or_else(|| "us-east-1".into()),
        insecure: conn.spec.insecure,
    })
}

/// Resolve the connection reference of a CR in `namespace` and connect.
pub async fn provider_for(
    client: &Client,
    namespace: &str,
    conn: &ConnectionRef,
) -> Result<RustFsProvider> {
    let info = match (&conn.secret_ref, &conn.cluster_ref) {
        (Some(_), Some(_)) => {
            return Err(Error::Spec(
                "connection.secretRef and connection.clusterRef are mutually exclusive".into(),
            ));
        }
        (None, None) => {
            return Err(Error::Spec(
                "connection must set either secretRef or clusterRef".into(),
            ));
        }
        (Some(secret), None) => from_local_secret(client, namespace, secret).await?,
        (None, Some(cluster)) => from_cluster_connection(client, namespace, cluster).await?,
    };
    RustFsProvider::connect(info).await
}

/// Read a single key from a Secret (e.g. a managed user's password).
pub async fn secret_key_value(
    client: &Client,
    namespace: &str,
    sref: &SecretKeyRef,
    default_key: &str,
) -> Result<String> {
    let secret = get_secret(client, namespace, &sref.name).await?;
    required(&secret, sref.key_or(default_key), &sref.name)
}
