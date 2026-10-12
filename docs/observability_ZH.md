# 可观测性

管理面沿用 AuthGuard 的通用 `mgmt:` 结构：独立 listener、context path、健康
路径、metrics 路径和 OTLP 设置。

Prometheus 指标只使用低基数 label：

- `agentvault_sign_requests_total{transport,outcome}`
- `agentvault_sign_duration_seconds{transport}`
- `agentvault_signs_in_flight`
- `agentvault_wallet_operations_total{operation,outcome}`
- `agentvault_http_requests_total{route,method,status}`
- `agentvault_history_records`
- `agentvault_history_persistence_failures_total`
- `agentvault_mqtt_reconnects_total`

wallet ID、client ID、request ID、digest 与签名都不会成为 metric label。
readiness 会校验加密钱包元数据和历史文件；liveness 只表示进程事件循环可响应。

设置 `mgmt.otel.enabled: true` 后通过 OTLP/gRPC 导出 trace。HTTP span 支持 W3C
Trace Context；`OTEL_RESOURCE_ATTRIBUTES` 可设置
`deployment.environment.name`。`logging.mode` 支持 `HUMAN` 与 `JSON`。
