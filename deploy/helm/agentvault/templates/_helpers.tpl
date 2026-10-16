{{- define "agentvault.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "agentvault.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name (include "agentvault.name" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{- define "agentvault.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" }}
app.kubernetes.io/name: {{ include "agentvault.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{- define "agentvault.selectorLabels" -}}
app.kubernetes.io/name: {{ include "agentvault.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "agentvault.serviceAccountName" -}}
{{- default (include "agentvault.fullname" .) .Values.serviceAccount.name -}}
{{- end -}}

{{- define "agentvault.authguardEnabled" -}}
{{- $middleware := index .Values "authguard-middleware" -}}
{{- if $middleware.enabled }}true{{ else }}false{{ end -}}
{{- end -}}

{{- define "agentvault.secretsProvider" -}}
{{- lower (default "kubernetes" .Values.secrets.provider) -}}
{{- end -}}

{{- define "agentvault.csiSecretsMountPath" -}}
/etc/agentvault/secrets
{{- end -}}

{{- define "agentvault.vaultAnnotations" -}}
vault.hashicorp.com/agent-inject: "true"
vault.hashicorp.com/role: {{ required "secrets.vault.vaultRole is required when secrets.provider=vault" .Values.secrets.vault.vaultRole | quote }}
vault.hashicorp.com/agent-init-first: "true"
vault.hashicorp.com/agent-inject-secret-env: {{ required "secrets.vault.secretPath is required when secrets.provider=vault" .Values.secrets.vault.secretPath | quote }}
vault.hashicorp.com/agent-inject-template-env: |
  {{ print "{{" }}- with secret {{ .Values.secrets.vault.secretPath | quote }} -{{ print "}}" }}
  {{ print "{{" }} .Data.data.env {{ print "}}" }}
  {{ print "{{" }}- end -{{ print "}}" }}
{{- end -}}
