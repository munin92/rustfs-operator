//! Custom resource definitions: Bucket, User, Policy and ClusterConnection
//! (group `rustfs.com/v1alpha1`).
//!
//! Every namespaced resource points at a RustFS server in one of two ways:
//!
//! - `connection.secretRef`: a Secret in the resource's own namespace holding
//!   `endpoint`, `accessKey` and `secretKey` (self-service; the namespace owns
//!   its credentials).
//! - `connection.clusterRef`: the name of a cluster-scoped
//!   [`ClusterConnection`], whose admin credentials Secret lives only in the
//!   operator's namespace (centrally managed).

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Reference to a RustFS server. Exactly one of `secretRef` / `clusterRef`
/// must be set (validated at reconcile time).
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionRef {
    /// Name of a Secret in the resource's namespace holding `endpoint`,
    /// `accessKey` and `secretKey` (optional: `region`, `insecure`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<String>,
    /// Name of a cluster-scoped ClusterConnection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_ref: Option<String>,
}

impl ConnectionRef {
    /// Connection via a Secret in the resource's own namespace.
    pub fn local(secret: impl Into<String>) -> Self {
        Self {
            secret_ref: Some(secret.into()),
            cluster_ref: None,
        }
    }

    /// Connection via a cluster-scoped ClusterConnection.
    pub fn cluster(name: impl Into<String>) -> Self {
        Self {
            secret_ref: None,
            cluster_ref: Some(name.into()),
        }
    }
}

/// A centrally managed connection to a RustFS server. Cluster-scoped; the
/// referenced credentials Secret lives in the operator's own namespace, so
/// application namespaces never see admin credentials.
#[derive(CustomResource, Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "rustfs.com",
    version = "v1alpha1",
    kind = "ClusterConnection",
    shortname = "rfcc",
    printcolumn = r#"{"name":"Endpoint","type":"string","jsonPath":".spec.endpoint"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct ClusterConnectionSpec {
    /// RustFS endpoint URL, e.g. `http://rustfs.storage.svc:9000`.
    pub endpoint: String,
    /// Name of the Secret in the operator's namespace holding `accessKey`
    /// and `secretKey`.
    pub credentials_secret_ref: String,
    /// AWS region; defaults to `us-east-1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Skip TLS verification.
    #[serde(default)]
    pub insecure: bool,
    /// Namespaces whose resources may use this connection.
    /// Absent means all namespaces are allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_namespaces: Option<Vec<String>>,
}

impl ClusterConnectionSpec {
    /// Whether resources in `namespace` may use this connection.
    pub fn allows_namespace(&self, namespace: &str) -> bool {
        match &self.allowed_namespaces {
            None => true,
            Some(allowed) => allowed.iter().any(|n| n == namespace),
        }
    }
}

/// What happens to the remote resource when the CR is deleted.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
pub enum DeletionPolicy {
    /// Delete the resource in RustFS.
    #[default]
    Delete,
    /// Keep the resource in RustFS, only remove the CR.
    Retain,
}

/// Whether a lifecycle rule is active.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
pub enum LifecycleStatus {
    /// The rule is evaluated by the scanner.
    #[default]
    Enabled,
    /// The rule is kept but not evaluated.
    Disabled,
}

/// A single S3 lifecycle rule.
///
/// Deliberately a *subset* of the S3 lifecycle spec: expiration by age and
/// aborting stale multipart uploads. Transitions and non-current-version rules
/// are omitted — they only make sense with storage tiers / versioning, which
/// RustFS does not offer, and a CRD field that silently does nothing is worse
/// than no field.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleRuleSpec {
    /// Rule ID, unique within the bucket.
    pub id: String,
    /// Enabled (default) or Disabled.
    #[serde(default)]
    pub status: LifecycleStatus,
    /// Object-key prefix the rule applies to. Unset = the whole bucket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// Expire objects this many days after creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiration_days: Option<i32>,
    /// Abort multipart uploads left incomplete for this many days. Worth setting
    /// on any bucket that takes large writes: aborted parts are invisible to a
    /// normal LIST and still consume the disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abort_incomplete_multipart_upload_days: Option<i32>,
}

/// Shared status for all RustFS resources.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResourceStatus {
    /// True once the remote resource matches the spec.
    #[serde(default)]
    pub ready: bool,
    /// Human-readable state or last error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Generation last acted upon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}

impl ResourceStatus {
    pub fn ready(generation: Option<i64>) -> Self {
        Self {
            ready: true,
            message: Some("reconciled".into()),
            observed_generation: generation,
        }
    }

    pub fn error(generation: Option<i64>, message: impl std::fmt::Display) -> Self {
        Self {
            ready: false,
            message: Some(message.to_string()),
            observed_generation: generation,
        }
    }
}

/// An S3 bucket in RustFS.
#[derive(CustomResource, Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "rustfs.com",
    version = "v1alpha1",
    kind = "Bucket",
    namespaced,
    status = "ResourceStatus",
    shortname = "rfb",
    printcolumn = r#"{"name":"Ready","type":"boolean","jsonPath":".status.ready"}"#,
    printcolumn = r#"{"name":"Message","type":"string","jsonPath":".status.message"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct BucketSpec {
    pub connection: ConnectionRef,
    /// Bucket name in RustFS; defaults to the CR name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bucket_name: Option<String>,
    /// Desired versioning state; unset means unmanaged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub versioning: Option<bool>,
    /// Hard quota in bytes; unset means unmanaged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_bytes: Option<u64>,
    /// Lifecycle rules; unset means unmanaged (the operator never reads or writes
    /// the bucket's lifecycle configuration). An explicitly EMPTY list is not the
    /// same thing: it means "no rules", and removes any configuration present on
    /// the bucket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<Vec<LifecycleRuleSpec>>,
    #[serde(default)]
    pub deletion_policy: DeletionPolicy,
}

impl Bucket {
    /// Effective bucket name in RustFS.
    pub fn bucket_name(&self) -> &str {
        self.spec
            .bucket_name
            .as_deref()
            .unwrap_or_else(|| self.metadata.name.as_deref().unwrap_or_default())
    }
}

/// Reference to a key inside a Secret in the same namespace.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SecretKeyRef {
    /// Secret name.
    pub name: String,
    /// Key within the Secret; each consumer documents its default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

impl SecretKeyRef {
    pub fn key_or<'a>(&'a self, default: &'a str) -> &'a str {
        self.key.as_deref().unwrap_or(default)
    }
}

fn default_true() -> bool {
    true
}

/// An IAM user (identity) in RustFS: a username with a password. Policies
/// attach to the user; applications should authenticate with [`AccessKey`]s
/// issued for the user rather than the password itself.
#[derive(CustomResource, Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "rustfs.com",
    version = "v1alpha1",
    kind = "User",
    namespaced,
    status = "ResourceStatus",
    shortname = "rfu",
    printcolumn = r#"{"name":"Ready","type":"boolean","jsonPath":".status.ready"}"#,
    printcolumn = r#"{"name":"Message","type":"string","jsonPath":".status.message"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct UserSpec {
    pub connection: ConnectionRef,
    /// Username in RustFS; defaults to the CR name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Secret holding the user's password (key defaults to `password`).
    /// Only applied when the user is first created; RustFS cannot update it.
    pub password_ref: SecretKeyRef,
    /// Policies attached to the user; managed declaratively (extra
    /// attachments are detached).
    #[serde(default)]
    pub policies: Vec<String>,
    /// Whether the user is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub deletion_policy: DeletionPolicy,
}

impl User {
    /// Effective username in RustFS.
    pub fn username(&self) -> &str {
        self.spec
            .username
            .as_deref()
            .unwrap_or_else(|| self.metadata.name.as_deref().unwrap_or_default())
    }
}

/// An access key (AK/SK credential pair, a RustFS "service account") owned
/// by a [`User`]. A user can have many. The operator authenticates to
/// RustFS *as the user* to issue the key (the admin API only mints keys for
/// the calling identity), generates the credentials, and writes them to a
/// Secret in the CR's namespace, owner-referenced so it is garbage-collected
/// with the CR.
#[derive(CustomResource, Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "rustfs.com",
    version = "v1alpha1",
    kind = "AccessKey",
    namespaced,
    status = "AccessKeyStatus",
    shortname = "rfak",
    printcolumn = r#"{"name":"Ready","type":"boolean","jsonPath":".status.ready"}"#,
    printcolumn = r#"{"name":"AccessKey","type":"string","jsonPath":".status.accessKey"}"#,
    printcolumn = r#"{"name":"Message","type":"string","jsonPath":".status.message"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct AccessKeySpec {
    pub connection: ConnectionRef,
    /// Username of the owning RustFS user.
    pub user: String,
    /// Secret holding that user's password (key defaults to `password`).
    pub password_ref: SecretKeyRef,
    /// Explicit access key id; generated when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_key: Option<String>,
    /// Optional description stored on the service account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional inline policy restricting what this key may do (subset of
    /// the user's permissions), written as YAML/JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "policy_document_schema")]
    pub policy: Option<serde_json::Value>,
    /// Name of the Secret the operator writes the credentials to
    /// (keys `accessKey`, `secretKey`, `endpoint`); defaults to
    /// `<cr-name>-credentials`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_secret_name: Option<String>,
    #[serde(default)]
    pub deletion_policy: DeletionPolicy,
}

/// Status for AccessKey resources.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccessKeyStatus {
    #[serde(default)]
    pub ready: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    /// The issued access key id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_key: Option<String>,
}

impl AccessKey {
    /// Effective name of the Secret receiving the credentials.
    pub fn target_secret_name(&self) -> String {
        self.spec.target_secret_name.clone().unwrap_or_else(|| {
            format!(
                "{}-credentials",
                self.metadata.name.as_deref().unwrap_or_default()
            )
        })
    }
}

/// An IAM policy in RustFS.
#[derive(CustomResource, Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "rustfs.com",
    version = "v1alpha1",
    kind = "Policy",
    namespaced,
    status = "ResourceStatus",
    shortname = "rfp",
    printcolumn = r#"{"name":"Ready","type":"boolean","jsonPath":".status.ready"}"#,
    printcolumn = r#"{"name":"Message","type":"string","jsonPath":".status.message"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct PolicySpec {
    pub connection: ConnectionRef,
    /// Policy name in RustFS; defaults to the CR name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_name: Option<String>,
    /// IAM policy document, written inline as YAML/JSON.
    #[schemars(schema_with = "policy_document_schema")]
    pub document: serde_json::Value,
    #[serde(default)]
    pub deletion_policy: DeletionPolicy,
}

/// Arbitrary JSON object; Kubernetes requires an explicit type plus
/// `x-kubernetes-preserve-unknown-fields` for free-form fields.
fn policy_document_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "object",
        "x-kubernetes-preserve-unknown-fields": true
    })
}

impl Policy {
    /// Effective policy name in RustFS.
    pub fn policy_name(&self) -> &str {
        self.spec
            .policy_name
            .as_deref()
            .unwrap_or_else(|| self.metadata.name.as_deref().unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cc_spec(allowed: Option<Vec<&str>>) -> ClusterConnectionSpec {
        ClusterConnectionSpec {
            endpoint: "http://rustfs:9000".into(),
            credentials_secret_ref: "rustfs-admin".into(),
            region: None,
            insecure: false,
            allowed_namespaces: allowed.map(|v| v.iter().map(|s| s.to_string()).collect()),
        }
    }

    #[test]
    fn absent_allowed_namespaces_allows_all() {
        assert!(cc_spec(None).allows_namespace("anything"));
    }

    #[test]
    fn allowed_namespaces_is_an_exact_allowlist() {
        let spec = cc_spec(Some(vec!["team-a", "team-b"]));
        assert!(spec.allows_namespace("team-a"));
        assert!(!spec.allows_namespace("team-c"));
        // empty list denies everything
        assert!(!cc_spec(Some(vec![])).allows_namespace("team-a"));
    }

    #[test]
    fn connection_ref_yaml_forms_deserialize() {
        let local: ConnectionRef = serde_yaml::from_str("secretRef: conn").unwrap();
        assert_eq!(local, ConnectionRef::local("conn"));
        let cluster: ConnectionRef = serde_yaml::from_str("clusterRef: prod").unwrap();
        assert_eq!(cluster, ConnectionRef::cluster("prod"));
    }
}
