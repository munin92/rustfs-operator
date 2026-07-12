#!/usr/bin/env bash
# Real-cluster chart e2e: k3d cluster + RustFS container, operator installed
# from the Helm charts (CRDs chart first), resources declared via the
# rustfs-resources chart, convergence and finalizer cleanup asserted.
#
# Required: IMAGE=<operator image present in the local docker daemon>
# Uses: docker, k3d, helm, kubectl
set -euo pipefail
cd "$(dirname "$0")/.."

IMAGE="${IMAGE:?set IMAGE to the operator image to test (must exist locally)}"
K3S_IMAGE="${K3S_IMAGE:-ghcr.io/openprojectx/dockerhub/rancher/k3s:v1.34.9-k3s1}"
RUSTFS_IMAGE="${RUSTFS_IMAGE:-ghcr.io/openprojectx/dockerhub/rustfs/rustfs:1.0.0-beta.8}"
# k3s system images (exact tags k3s v1.34.9 wants). Imported into the
# cluster nodes so nothing is pulled from inside the cluster at runtime —
# in-cluster docker.io pulls don't work on proxied/firewalled hosts.
K3S_SYSTEM_IMAGES=(
  docker.io/rancher/mirrored-pause:3.6
  docker.io/rancher/mirrored-coredns-coredns:1.14.4
  docker.io/rancher/local-path-provisioner:v0.0.36
)
CLUSTER=rfo-chart-e2e
RUSTFS_NAME=${CLUSTER}-rustfs

WORK=$(mktemp -d)
export KUBECONFIG=$WORK/kubeconfig

cleanup() {
  if [ "${KEEP:-}" = "1" ]; then
    echo "KEEP=1: leaving cluster '$CLUSTER' and container '$RUSTFS_NAME' running (kubeconfig: $KUBECONFIG)"
    return
  fi
  k3d cluster delete $CLUSTER >/dev/null 2>&1 || true
  docker rm -f $RUSTFS_NAME >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
trap cleanup EXIT

# Relaxed kubelet eviction thresholds so the cluster survives nearly-full
# host disks (k3s ignores --kubelet-arg for these; a config drop-in works).
cat > "$WORK/99-eviction.conf" <<EOF
apiVersion: kubelet.config.k8s.io/v1beta1
kind: KubeletConfiguration
evictionHard:
  nodefs.available: 100Mi
  imagefs.available: 100Mi
  nodefs.inodesFree: 1%
EOF

echo "== create k3d cluster"
k3d cluster create $CLUSTER \
  --image "$K3S_IMAGE" \
  --no-lb \
  --k3s-arg '--disable=traefik@server:0' \
  --k3s-arg '--disable=metrics-server@server:0' \
  --volume "$WORK/99-eviction.conf:/var/lib/rancher/k3s/agent/etc/kubelet.conf.d/99-eviction.conf@server:0" \
  --kubeconfig-update-default=false \
  --kubeconfig-switch-context=false \
  --wait --timeout 180s
k3d kubeconfig get $CLUSTER > "$KUBECONFIG"

echo "== start RustFS on the cluster network"
docker run -d --rm --name $RUSTFS_NAME --network k3d-$CLUSTER \
  -e RUSTFS_ACCESS_KEY=rustfsadmin -e RUSTFS_SECRET_KEY=rustfsadmin \
  "$RUSTFS_IMAGE" >/dev/null

echo "== import operator + system images (no in-cluster registry pulls)"
# `k3d image import` (and plain `docker save`) trips over multi-arch
# attestation manifests when docker uses the containerd image store, so
# save each image single-platform and stream it into the node's containerd.
# The --platform flag only exists (and is only needed) on the containerd
# store; the classic overlay2 store (GitHub runners) saves single-platform.
save_args=()
if [ "$(docker info -f '{{.Driver}}')" = "overlayfs" ]; then
  save_args=(--platform "linux/$(docker version --format '{{.Server.Arch}}')")
fi
for img in "$IMAGE" "${K3S_SYSTEM_IMAGES[@]}"; do
  docker image inspect "$img" >/dev/null 2>&1 || docker pull -q "$img"
  docker save "${save_args[@]}" "$img" \
    | docker exec -i k3d-$CLUSTER-server-0 ctr --namespace k8s.io images import - >/dev/null
done

echo "== install CRDs chart"
helm install rustfs-crds charts/rustfs-operator-crds --wait

echo "== install operator chart (skips existing CRDs) with bootstrapped ClusterConnection"
repo=${IMAGE%:*}; tag=${IMAGE##*:}
helm install rustfs-operator charts/rustfs-operator \
  -n rustfs-operator --create-namespace \
  --set image.repository="$repo" --set image.tag="$tag" --set image.pullPolicy=Never \
  --set-json "clusterConnections=[{\"name\":\"prod\",\"endpoint\":\"http://$RUSTFS_NAME:9000\",\"credentials\":{\"accessKey\":\"rustfsadmin\",\"secretKey\":\"rustfsadmin\"}}]" \
  --wait --timeout 300s

echo "== install resources chart"
kubectl create namespace team-a
helm install app-storage charts/rustfs-resources -n team-a \
  -f scripts/testdata/resources-values.yaml

wait_ready() { # <kind> <name>
  for _ in $(seq 1 60); do
    [ "$(kubectl -n team-a get "$1" "$2" -o jsonpath='{.status.ready}' 2>/dev/null)" = "true" ] && {
      echo "   $1/$2 ready"; return 0; }
    sleep 3
  done
  echo "FAIL: timeout waiting for $1/$2:" >&2
  kubectl -n team-a get "$1" "$2" -o yaml 2>/dev/null | tail -15 >&2
  return 1
}
wait_ready bucket app-data
wait_ready policy app-data-rw
wait_ready user ci-user
wait_ready accesskey ci-key
kubectl -n team-a get secret ci-key-credentials -o jsonpath='{.data.accessKey}' | grep -q . \
  || { echo "FAIL: ci-key credentials secret missing accessKey" >&2; exit 1; }

echo "== uninstall resources release; finalizers must clean up remote state"
helm uninstall app-storage -n team-a --wait
for _ in $(seq 1 60); do
  [ -z "$(kubectl -n team-a get buckets,users,policies,accesskeys -o name 2>/dev/null)" ] && break
  sleep 3
done
remaining=$(kubectl -n team-a get buckets,users,policies,accesskeys -o name 2>/dev/null)
[ -z "$remaining" ] || { echo "FAIL: CRs not cleaned up: $remaining" >&2; exit 1; }

echo "chart-e2e OK"
