# rustfs-operator-crds Helm chart

The [rustfs-operator](https://github.com/OpenProjectX/rustfs-operator) CRDs
(`Bucket`, `User`, `Policy`, `ClusterConnection`) as **Helm templates**, so
they are upgradeable via `helm upgrade` and controllable through values —
unlike the copy shipped in the main chart's `crds/` directory, which Helm
only ever installs once.

```sh
helm repo add rustfs-operator https://openprojectx.github.io/rustfs-operator

# install CRDs first, then the operator (it skips already-existing CRDs)
helm install rustfs-operator-crds rustfs-operator/rustfs-operator-crds
helm install rustfs-operator rustfs-operator/rustfs-operator \
  --namespace rustfs-operator --create-namespace
```

## Values

| Key | Default | Description |
|-----|---------|-------------|
| `keep` | `true` | add `helm.sh/resource-policy: keep` so `helm uninstall` leaves the CRDs (deleting a CRD cascades to **all** CRs of that kind) |
| `annotations` | `{}` | extra annotations on every CRD |
| `labels` | `{}` | extra labels on every CRD |
| `crds.bucket` | `true` | install `buckets.rustfs.com` |
| `crds.user` | `true` | install `users.rustfs.com` |
| `crds.policy` | `true` | install `policies.rustfs.com` |
| `crds.clusterConnection` | `true` | install `clusterconnections.rustfs.com` |

## Adopting CRDs installed by the main chart

If the CRDs already exist on the cluster (installed from the main chart's
`crds/` directory), Helm refuses to manage them until they carry release
ownership metadata. Adopt them once, then install this chart:

```sh
for crd in buckets users policies clusterconnections; do
  kubectl annotate crd ${crd}.rustfs.com \
    meta.helm.sh/release-name=rustfs-operator-crds \
    meta.helm.sh/release-namespace=default --overwrite
  kubectl label crd ${crd}.rustfs.com \
    app.kubernetes.io/managed-by=Helm --overwrite
done
helm install rustfs-operator-crds rustfs-operator/rustfs-operator-crds
```

(Adjust the release name/namespace to whatever you install with.)

## Development

Templates are generated from the Rust CRD types — do not edit them by hand.
Regenerate after changing `src/crd.rs`:

```sh
python3 scripts/generate-crds.py
```
