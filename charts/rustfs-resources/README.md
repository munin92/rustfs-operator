# rustfs-resources Helm chart

Declare RustFS resources — `Bucket`, `Policy`, `User` custom resources —
from Helm values. One release per team/app/namespace; the
[rustfs-operator](https://github.com/OpenProjectX/rustfs-operator)
reconciles them against the RustFS server. Resources are created in the
release namespace.

```sh
helm repo add rustfs-operator https://openprojectx.github.io/rustfs-operator
helm install app-storage rustfs-operator/rustfs-resources \
  --namespace team-a -f my-resources.yaml
```

## Example

```yaml
# my-resources.yaml
connection:            # default for every entry; per-entry override possible
  clusterRef: prod     # or secretRef: <connection Secret in this namespace>

buckets:
  - name: app-data
    versioning: true
    quotaBytes: 10737418240      # 10 GiB
    deletionPolicy: Retain       # keep the bucket if the CR is deleted

policies:
  - name: app-data-rw
    document:
      Version: "2012-10-17"
      Statement:
        - Effect: Allow
          Action: ["s3:GetObject", "s3:PutObject", "s3:DeleteObject", "s3:ListBucket"]
          Resource: ["arn:aws:s3:::app-data", "arn:aws:s3:::app-data/*"]

users:
  - name: app-user
    policies: ["app-data-rw"]
    secretKeyRef:                # existing Secret with key `secretKey`
      name: app-user-creds
```

## Values

| Key | Description |
|-----|-------------|
| `connection.clusterRef` / `connection.secretRef` | default connection (exactly one) |
| `buckets[]` | `name` (required), `bucketName`, `versioning`, `quotaBytes`, `deletionPolicy`, `connection` |
| `policies[]` | `name` (required), `document` (required), `policyName`, `deletionPolicy`, `connection` |
| `users[]` | `name` (required), `accessKey`, `secretKeyRef` **or** inline `secretKey`, `policies`, `enabled`, `deletionPolicy`, `connection` |

Fields you omit stay unmanaged (e.g. no `versioning` key means the operator
never touches versioning). `deletionPolicy` defaults to `Delete` — the
remote resource is removed when the CR is deleted; use `Retain` to keep it.

**User secret keys**: `secretKeyRef` points at an existing Secret in the
release namespace. Alternatively set `secretKey` inline and the chart
creates `<release>-user-<name>` — but the key then lives in the Helm
release values; prefer `secretKeyRef` in production. The secret key is only
applied when the user is first created (RustFS cannot update it in place).

## Prerequisites

The operator and CRDs must be installed (main `rustfs-operator` chart), and
the referenced `ClusterConnection` or connection Secret must exist.
