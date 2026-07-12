# rustfs-operator

A Kubernetes operator that manages **RustFS** resources declaratively:
buckets, IAM users and IAM policies. Built on [kube-rs](https://kube.rs) and
the [`rc-core`](https://crates.io/crates/rc-core) /
[`rc-s3`](https://crates.io/crates/rc-s3) client crates from
[rustfs/cli](https://github.com/rustfs/cli).

## CRDs (`rustfs.com/v1alpha1`)

| Kind                | Short name | Scope      | Manages                                                |
|---------------------|------------|------------|--------------------------------------------------------|
| `Bucket`            | `rfb`      | namespaced | bucket existence, versioning, hard quota               |
| `User`              | `rfu`      | namespaced | IAM identity (username + password), attached policies  |
| `AccessKey`         | `rfak`     | namespaced | AK/SK credential pair for a User, written to a Secret  |
| `Policy`            | `rfp`      | namespaced | IAM policy document (inline YAML/JSON)                 |
| `ClusterConnection` | `rfcc`     | cluster    | centrally managed RustFS server connection             |

The IAM model mirrors RustFS: a **User** is an identity (username/password)
that policies attach to; applications authenticate with **AccessKeys**
(a user can have many). The operator issues each AccessKey while
authenticated *as the user* and writes the generated `accessKey`/
`secretKey`/`endpoint` into a Secret in the CR's namespace, owner-referenced
so it is garbage-collected with the CR. If that Secret is lost, the key is
revoked and reissued. For a user to manage its own keys, its policies must
allow `admin:CreateServiceAccount`, `admin:ListServiceAccounts` and
`admin:RemoveServiceAccount`.

Namespaced resources select a RustFS server via `spec.connection`, in one of
two mutually exclusive ways:

**Centrally managed (recommended for multi-tenant clusters)** — reference a
cluster-scoped `ClusterConnection`; the admin credentials Secret lives only
in the operator's namespace, and `allowedNamespaces` restricts who may use
it:

```yaml
spec:
  connection:
    clusterRef: prod
```

**Self-service** — reference a connection Secret in the resource's own
namespace (keys: `endpoint`, `accessKey`, `secretKey`; optional `region`,
`insecure`):

```yaml
spec:
  connection:
    secretRef: rustfs-conn
```

See `deploy/example.yaml` for complete examples of both. Each resource
supports `deletionPolicy: Delete` (default; the remote resource is removed
via finalizer when the CR is deleted) or `Retain`.

## Install

Via Helm (chart repo served from GitHub Pages, image from GHCR):

```sh
helm repo add rustfs-operator https://openprojectx.github.io/rustfs-operator
helm install rustfs-operator rustfs-operator/rustfs-operator \
  --namespace rustfs-operator --create-namespace
```

The chart can also bootstrap `ClusterConnection` resources (and their admin
credentials Secrets) from values — see
[`charts/rustfs-operator/README.md`](charts/rustfs-operator/README.md).

Teams can declare their RustFS resources (Buckets, Policies, Users) from
values with the [`rustfs-resources`](charts/rustfs-resources/README.md)
chart — one release per namespace, reconciled by the operator.

The main chart installs the CRDs from its `crds/` directory, which Helm
never upgrades. For Helm-managed, value-controlled CRDs (per-CRD toggles,
keep-on-uninstall policy, upgrades via `helm upgrade`), install the
[`rustfs-operator-crds`](charts/rustfs-operator-crds/README.md) chart first —
the main chart automatically skips CRDs that already exist. CRD manifests
and the CRDs chart templates are regenerated from the Rust types with
`python3 scripts/generate-crds.py`.

Or run from source against the current kubeconfig:

```sh
# CRDs (regenerate with: cargo run -- crd > deploy/crds.yaml)
kubectl apply -f deploy/crds.yaml
cargo run --release -- run
```

## Releasing

Push a `v*` tag. The release workflow builds and pushes
`ghcr.io/openprojectx/rustfs-operator:<version>`, packages the Helm chart to
the `gh-pages` chart repository, and creates a GitHub release with the CRD
manifest attached. Set the repository variable `RELEASE_BINARY=true` to also
build and attach a linux-amd64 binary.

## Behavior notes

- **Reconcile loop**: finalizer-based; drift is re-checked every 5 minutes,
  errors retry after 15s and are reported in `.status.message`.
- **User passwords** are only applied at user creation; RustFS cannot
  update them in place. Rotate credentials by rotating AccessKeys instead
  (delete/recreate the AccessKey CR).
- **Policies in use cannot be deleted**: RustFS rejects deleting a policy
  that is still attached; the Policy CR reports the error and retries.
- **Policy attachment** uses RustFS's `set-user-or-group-policy` endpoint,
  which *replaces* the whole attachment set — `spec.policies` is therefore
  fully declarative.
- **Policy drift detection** compares documents semantically: the server
  normalizes stored policies (adds empty `Sid`/`Condition`, reorders string
  arrays, wraps in metadata), so byte-comparison would never converge.

## Testing

| Layer | Command | Needs |
|-------|---------|-------|
| Unit (mocked provider) | `cargo test` | – |
| Integration (real RustFS) | `cargo test --features integration --test integration_rustfs` | Docker |
| E2E (real k3s + RustFS, controllers in-process) | `cargo test --features e2e --test e2e_k3s` | Docker |
| Chart tests (lint, render, validation) | `bash scripts/chart-tests.sh` | helm |
| Chart e2e (helm install on k3d, real operator image) | `IMAGE=<image> bash scripts/chart-e2e.sh` | Docker, k3d, helm, kubectl |

The e2e test boots a k3s cluster and a RustFS server in containers, installs
the CRDs, runs the controllers inside the test process, applies
Bucket/User/Policy CRs and asserts both convergence in RustFS and finalizer
cleanup on deletion.

The chart e2e goes one layer further: it creates a k3d cluster, installs the
CRDs chart, the operator chart (with a values-bootstrapped ClusterConnection)
running the given image, and the `rustfs-resources` chart, then asserts the
declared resources converge and that `helm uninstall` cleans up remote state
via finalizers. All required images are preloaded into the cluster, so
nothing is pulled from inside it. CI runs every layer; test images, the k3d
binary and the rust toolchain are cached in the GitHub Actions cache.
