# rustfs-operator Helm chart

Deploys the [rustfs-operator](https://github.com/OpenProjectX/rustfs-operator):
a Kubernetes operator managing RustFS buckets, IAM users and policies via
`Bucket`, `User`, `Policy` and `ClusterConnection` CRDs (installed from this
chart's `crds/` directory).

```sh
helm repo add rustfs-operator https://openprojectx.github.io/rustfs-operator
helm install rustfs-operator rustfs-operator/rustfs-operator \
  --namespace rustfs-operator --create-namespace
```

## Values

| Key | Default | Description |
|-----|---------|-------------|
| `image.repository` | `ghcr.io/openprojectx/rustfs-operator` | operator image |
| `image.tag` | chart `appVersion` | image tag override |
| `replicaCount` | `1` | operator replicas |
| `logLevel` | `info` | `RUST_LOG` value |
| `serviceAccount.create` | `true` | create the ServiceAccount |
| `rbac.create` | `true` | create ClusterRole/Role and bindings |
| `rbac.clusterWideSecrets` | `true` | see [RBAC](#rbac) |
| `rbac.secretNamespaces` | `[]` | namespaces granted Secret access via Roles when `clusterWideSecrets` is off |
| `clusterConnections` | `[]` | bootstrap ClusterConnection resources, see below |

## Bootstrapping ClusterConnections

`clusterConnections` declares cluster-scoped `ClusterConnection` resources
(and optionally their admin credentials Secrets) directly from values, so
GitOps setups don't need a second mechanism. Off by default.

**Inline credentials** — the chart creates the Secret in the release
namespace (named `<fullname>-<name>-admin` unless `secretName` is set):

```yaml
clusterConnections:
  - name: prod
    endpoint: http://rustfs.storage.svc:9000
    allowedNamespaces: ["team-a", "team-b"]   # omit = all namespaces
    credentials:
      accessKey: rustfsadmin
      secretKey: rustfsadmin
```

**Existing Secret** (recommended for production) — reference a Secret in the
release namespace holding `accessKey`/`secretKey`; the chart creates none:

```yaml
clusterConnections:
  - name: prod
    endpoint: http://rustfs.storage.svc:9000
    credentials:
      existingSecret: rustfs-admin
```

> **Security note:** inline `accessKey`/`secretKey` are stored in the Helm
> release values (a Secret in the release namespace, readable by anyone who
> can read Secrets there or run `helm get values`). Prefer `existingSecret`
> combined with sealed-secrets / external-secrets in production.

## RBAC

`rbac.clusterWideSecrets: true` (default) lets the operator read and write
Secrets in all namespaces (read: `connection.secretRef` and
`User`/`AccessKey` `passwordRef`; write: `AccessKey` credential Secrets).
For least privilege set it to `false` and list the application namespaces
that hold RustFS resources in `rbac.secretNamespaces` — the operator then
gets a namespaced Role in exactly those namespaces (plus its own).
