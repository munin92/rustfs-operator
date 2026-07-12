{{- define "rustfs-resources.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
app.kubernetes.io/name: {{ .Chart.Name }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Render the spec.connection block for an entry, validating that exactly one
of clusterRef/secretRef is set after applying the top-level default.
Context: dict "root" $ "entry" <entry> "id" "<kind>[<name>]"
*/}}
{{- define "rustfs-resources.connection" -}}
{{- $conn := .entry.connection | default .root.Values.connection | default dict -}}
{{- $secretRef := $conn.secretRef | default "" -}}
{{- $clusterRef := $conn.clusterRef | default "" -}}
{{- if and (eq $secretRef "") (eq $clusterRef "") -}}
{{- fail (printf "%s: no connection; set top-level 'connection' or a per-entry 'connection' with clusterRef or secretRef" .id) -}}
{{- end -}}
{{- if and (ne $secretRef "") (ne $clusterRef "") -}}
{{- fail (printf "%s: connection.clusterRef and connection.secretRef are mutually exclusive" .id) -}}
{{- end -}}
connection:
  {{- if ne $clusterRef "" }}
  clusterRef: {{ $clusterRef | quote }}
  {{- else }}
  secretRef: {{ $secretRef | quote }}
  {{- end }}
{{- end }}

{{/*
Validate and render deletionPolicy when set.
Context: dict "entry" <entry> "id" "<kind>[<name>]"
*/}}
{{- define "rustfs-resources.deletionPolicy" -}}
{{- with .entry.deletionPolicy -}}
{{- if not (has . (list "Delete" "Retain")) -}}
{{- fail (printf "%s: deletionPolicy must be Delete or Retain, got %q" $.id .) -}}
{{- end -}}
deletionPolicy: {{ . }}
{{- end -}}
{{- end }}

{{/*
Require a unique non-empty .name per entry.
Context: dict "entry" <entry> "seen" <dict> "kind" "<kind>"
*/}}
{{- define "rustfs-resources.entryName" -}}
{{- $name := .entry.name | default "" -}}
{{- if eq $name "" -}}
{{- fail (printf "%s: every entry needs a non-empty 'name'" .kind) -}}
{{- end -}}
{{- if hasKey .seen $name -}}
{{- fail (printf "%s: duplicate name %q" .kind $name) -}}
{{- end -}}
{{- $_ := set .seen $name true -}}
{{- $name -}}
{{- end }}
