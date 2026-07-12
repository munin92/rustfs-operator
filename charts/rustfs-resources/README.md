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
    policies: ["app-data-rw"]    # must include admin:*ServiceAccount actions
    passwordRef:                 # existing Secret with key `password`
      name: app-user-creds

accessKeys:
  - name: app-key                # operator writes AK/SK to Secret
    user: app-user               # "app-key-credentials" in this namespace
    passwordRef:
      name: app-user-creds
```

## Values

| Key | Description |
|-----|-------------|
| `connection.clusterRef` / `connection.secretRef` | default connection (exactly one) |
| `buckets[]` | `name` (required), `bucketName`, `versioning`, `quotaBytes`, `deletionPolicy`, `connection` |
| `policies[]` | `name` (required), `document` (required), `policyName`, `deletionPolicy`, `connection` |
| `users[]` | `name` (required), `username`, `passwordRef` **or** inline `password`, `policies`, `enabled`, `deletionPolicy`, `connection` |
| `accessKeys[]` | `name`, `user` (required), `passwordRef` **or** `passwordFromUser`, `accessKey`, `description`, `policy`, `targetSecretName`, `deletionPolicy`, `connection` |

Fields you omit stay unmanaged (e.g. no `versioning` key means the operator
never touches versioning). `deletionPolicy` defaults to `Delete` — the
remote resource is removed when the CR is deleted; use `Retain` to keep it.

**User passwords**: `passwordRef` points at an existing Secret in the
release namespace. Alternatively set `password` inline and the chart
creates `<release>-user-<name>` — but it then lives in the Helm release
values; prefer `passwordRef` in production. Passwords are only applied when
the user is first created (RustFS cannot update them in place).

**Access keys**: each `accessKeys[]` entry issues an AK/SK pair for `user`;
the operator writes the generated credentials to a Secret (default
`<name>-credentials`). `passwordFromUser: <users[] entry>` reuses the
password Secret this chart created for that user. The user's policies must
allow `admin:CreateServiceAccount`, `admin:ListServiceAccounts` and
`admin:RemoveServiceAccount`.

## Prerequisites

The operator and CRDs must be installed (main `rustfs-operator` chart), and
the referenced `ClusterConnection` or connection Secret must exist.
