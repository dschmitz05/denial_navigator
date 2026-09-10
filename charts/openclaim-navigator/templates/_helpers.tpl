{{- define "openclaim.name" -}}openclaim{{- end }}
{{- define "openclaim.labels" -}}
app.kubernetes.io/name: {{ include "openclaim.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}
{{- define "openclaim.image" -}}
{{ .repository }}:{{ .tag }}
{{- end }}
