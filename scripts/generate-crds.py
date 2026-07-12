#!/usr/bin/env python3
"""Regenerate all CRD artifacts from the Rust types.

Run `cargo run -- crd > deploy/crds.yaml` first (or let this script do it),
then this script syncs:
  - charts/rustfs-operator/crds/crds.yaml           (verbatim copy)
  - charts/rustfs-operator-crds/templates/*.yaml    (templated per CRD)
"""

import pathlib
import re
import shutil
import subprocess

ROOT = pathlib.Path(__file__).resolve().parent.parent
SRC = ROOT / "deploy" / "crds.yaml"
CRDS_CHART_TEMPLATES = ROOT / "charts" / "rustfs-operator-crds" / "templates"
MAIN_CHART_CRDS = ROOT / "charts" / "rustfs-operator" / "crds" / "crds.yaml"

# plural -> values key under .Values.crds
VALUES_KEYS = {
    "buckets": "bucket",
    "users": "user",
    "policies": "policy",
    "accesskeys": "accessKey",
    "clusterconnections": "clusterConnection",
}

METADATA_TEMPLATE = """metadata:
  name: {plural}.rustfs.com
  {{{{- if or .Values.keep .Values.annotations }}}}
  annotations:
    {{{{- if .Values.keep }}}}
    helm.sh/resource-policy: keep
    {{{{- end }}}}
    {{{{- with .Values.annotations }}}}
    {{{{- toYaml . | nindent 4 }}}}
    {{{{- end }}}}
  {{{{- end }}}}
  labels:
    app.kubernetes.io/name: rustfs-operator-crds
    app.kubernetes.io/managed-by: {{{{ .Release.Service }}}}
    helm.sh/chart: {{{{ printf "%s-%s" .Chart.Name .Chart.Version }}}}
    {{{{- with .Values.labels }}}}
    {{{{- toYaml . | nindent 4 }}}}
    {{{{- end }}}}
"""


def main() -> None:
    subprocess.run(
        ["cargo", "run", "--quiet", "--", "crd"],
        cwd=ROOT,
        stdout=SRC.open("w"),
        check=True,
    )
    shutil.copyfile(SRC, MAIN_CHART_CRDS)

    CRDS_CHART_TEMPLATES.mkdir(parents=True, exist_ok=True)
    docs = [d for d in SRC.read_text().split("---\n") if d.strip()]
    assert len(docs) == len(VALUES_KEYS), f"expected {len(VALUES_KEYS)} CRDs, got {len(docs)}"
    for doc in docs:
        match = re.search(r"^  name: (\w+)\.rustfs\.com$", doc, re.M)
        assert match, "CRD document without a rustfs.com name"
        plural = match.group(1)
        key = VALUES_KEYS[plural]
        plain = f"metadata:\n  name: {plural}.rustfs.com\n"
        assert plain in doc, f"unexpected metadata layout for {plural}"
        templated = doc.replace(plain, METADATA_TEMPLATE.format(plural=plural))
        out = CRDS_CHART_TEMPLATES / f"{plural}.yaml"
        out.write_text(
            f"{{{{- if .Values.crds.{key} }}}}\n{templated.rstrip()}\n{{{{- end }}}}\n"
        )
        print(f"wrote {out.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
