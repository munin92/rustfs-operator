#!/usr/bin/env bash
# Static chart tests: lint, render assertions, validation failure paths.
# No cluster or docker needed.
set -euo pipefail
cd "$(dirname "$0")/.."

fail() { echo "FAIL: $*" >&2; exit 1; }

echo "== helm lint (all charts)"
helm lint charts/* >/dev/null

echo "== main chart: default render has no ClusterConnection/Secret"
render=$(helm template rel charts/rustfs-operator --namespace op-ns)
grep -q 'kind: ClusterConnection' <<<"$render" && fail "unexpected ClusterConnection in default render"
grep -q 'kind: Secret' <<<"$render" && fail "unexpected Secret in default render"

echo "== main chart: clusterConnections rendering"
render=$(helm template rel charts/rustfs-operator --namespace op-ns --set-json 'clusterConnections=[
  {"name":"prod","endpoint":"http://e:9000","allowedNamespaces":["a"],"credentials":{"accessKey":"k","secretKey":"s"}},
  {"name":"stg","endpoint":"http://e2:9000","insecure":true,"credentials":{"existingSecret":"ext"}}]')
grep -q 'credentialsSecretRef: "rel-rustfs-operator-prod-admin"' <<<"$render" || fail "inline creds secret not referenced"
grep -q 'credentialsSecretRef: "ext"' <<<"$render" || fail "existingSecret not referenced"
[ "$(grep -c 'kind: ClusterConnection' <<<"$render")" = 2 ] || fail "expected 2 ClusterConnections"
[ "$(grep -c 'kind: Secret' <<<"$render")" = 1 ] || fail "expected exactly 1 Secret"
grep -q 'insecure: true' <<<"$render" || fail "insecure not rendered"
grep -q 'allowedNamespaces:' <<<"$render" || fail "allowedNamespaces not rendered"

echo "== crds chart: defaults, keep policy, toggles"
render=$(helm template crds charts/rustfs-operator-crds)
[ "$(grep -c 'kind: CustomResourceDefinition' <<<"$render")" = 4 ] || fail "expected 4 CRDs"
[ "$(grep -c 'helm.sh/resource-policy: keep' <<<"$render")" = 4 ] || fail "expected keep policy on all CRDs"
render=$(helm template crds charts/rustfs-operator-crds --set keep=false --set crds.user=false)
[ "$(grep -c 'kind: CustomResourceDefinition' <<<"$render")" = 3 ] || fail "crds.user=false should drop one CRD"
grep -q 'resource-policy' <<<"$render" && fail "keep=false should drop the resource-policy annotation"

echo "== resources chart: rendering"
render=$(helm template rel charts/rustfs-resources -n team-a -f scripts/testdata/resources-values.yaml)
grep -q 'kind: Bucket' <<<"$render" || fail "no Bucket rendered"
grep -q 'quotaBytes: 10485760' <<<"$render" || fail "quotaBytes wrong or missing"
grep -q 'clusterRef: "prod"' <<<"$render" || fail "default connection not applied"
grep -q 'name: rel-user-ci-user' <<<"$render" || fail "chart-created user secret not rendered"
render=$(helm template rel charts/rustfs-resources --set-json 'buckets=[{"name":"b","connection":{"secretRef":"local"}}]')
grep -q 'secretRef: "local"' <<<"$render" || fail "per-entry connection override not applied"
grep -q 'versioning' <<<"$render" && fail "omitted versioning must not render"

echo "== validation failures"
expect_fail() { # <chart> <set-json> <message fragment>
  local out
  if out=$(helm template rel "$1" --set-json "$2" 2>&1); then
    fail "expected template failure for: $2"
  fi
  grep -q "$3" <<<"$out" || fail "wrong error for [$2]: $out"
}
C=charts/rustfs-operator
expect_fail $C 'clusterConnections=[{"endpoint":"x"}]' "non-empty 'name'"
expect_fail $C 'clusterConnections=[{"name":"a"}]' "'endpoint' is required"
expect_fail $C 'clusterConnections=[{"name":"a","endpoint":"x","credentials":{"existingSecret":"s","accessKey":"k"}}]' "not both"
expect_fail $C 'clusterConnections=[{"name":"a","endpoint":"x"}]' "must both be set"
expect_fail $C 'clusterConnections=[{"name":"a","endpoint":"x","credentials":{"create":false}}]' "no credentials source"
expect_fail $C 'clusterConnections=[{"name":"a","endpoint":"x","credentials":{"accessKey":"k","secretKey":"s"}},{"name":"a","endpoint":"y","credentials":{"existingSecret":"e"}}]' "duplicate name"
R=charts/rustfs-resources
expect_fail $R 'buckets=[{"name":"a"}]' "no connection"
expect_fail $R 'buckets=[{"name":"a","connection":{"clusterRef":"p","secretRef":"s"}}]' "mutually exclusive"
expect_fail $R 'buckets=[{"name":"a","connection":{"clusterRef":"p"},"deletionPolicy":"Del"}]' "deletionPolicy must be"
expect_fail $R 'policies=[{"name":"a","connection":{"clusterRef":"p"}}]' "'document' is required"
expect_fail $R 'users=[{"name":"a","connection":{"clusterRef":"p"}}]' "secret key source is required"
expect_fail $R 'users=[{"name":"a","connection":{"clusterRef":"p"},"secretKey":"x","secretKeyRef":{"name":"y"}}]' "not both"

echo "chart-tests OK"
