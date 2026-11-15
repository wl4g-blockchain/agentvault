# Common Audit Log

A unified audit logging library with multiple provider implementations.

## Features

- **Provider Trait**: Pluggable backend providers for audit log storage
- **File Provider**: Default implementation, writes to rotating log files
- **MQTT Provider**: For sigbot, publishes audit logs to EMQX MQTT broker
- **Macros**: `audit_info!()`, `audit_warn!()`, `audit_error!()`, `audit_debug!()` for manual logging
- **Attribute Macro**: `#[audit_log]` for automatic function execution logging
- **Optional Service Name**: Service name can be omitted, defaults to env `SIGBOT_SERVICE_NAME` or `SERVICE_NAME`

## Usage

### Manual Logging (手动挡)

```rust
use common_audit_log::{audit_info, audit_warn, audit_error, AuditLogLevel, audit_log};

// Initialize the audit logger
let config = AuditLogConfig {
    service_name: "my-service".to_string(),  // Can be empty to use env default
    workflow_id: Some("wf-001".to_string()),
    node_id: Some("node-1".to_string()),
    provider_config: ProviderConfig::File(FileProviderConfig::default()),
};

let provider = FileAuditLogProvider::new();
init_audit_logger(provider, config).unwrap();

// Log with explicit service name
audit_info!("my-service", "User {} logged in", user_id).await;
audit_warn!("my-service", "High latency detected: {}ms", latency).await;
audit_error!("my-service", "Failed to process request: {}", error).await;

// Log with default service name (from env/config)
audit_info!("User {} logged in", user_id).await;  // Uses SIGBOT_SERVICE_NAME or SERVICE_NAME
audit_warn!("High latency detected: {}ms", latency).await;

// Log with metadata
audit_with_meta!(
    "my-service",
    AuditLogLevel::Info,
    "Order processed",
    r#"{"order_id": "123", "amount": 100}"#
).await;
```

### Automatic Logging (自动挡) with `#[audit_log]` Macro

```rust
use common_macro::audit_log;

/// Automatically log function entry/exit with arguments (explicit service)
#[audit_log(service = "order-service")]
async fn process_order(order_id: String, quantity: u32) -> Result<Order, Error> {
    // Function body
    Ok(order)
}

/// Log with default service name (from env/config) - service is optional!
#[audit_log]
async fn process_payment(amount: f64) -> Result<(), Error> {
    // Function body - service name defaults to SIGBOT_SERVICE_NAME env var
    Ok(())
}

/// Log with custom level and skip sensitive arguments
#[audit_log(service = "wallet-service", level = "warn", skip_args = "1")]
async fn transfer_funds(wallet_id: i64, _secret_key: &str, amount: f64) -> Result<(), Error> {
    // Function body
    Ok(())
}

/// Log with additional metadata
#[audit_log(service = "strategy-service", metadata = "{\"strategy_id\": \"momentum_001\"}")]
fn calculate_signal(price: f64, rsi: f64) -> Signal {
    // Function body
    Signal::Buy
}
```

## Provider Configuration

### File Provider (Default)

```rust
use common_audit_log::{AuditLogConfig, ProviderConfig, FileProviderConfig, FileAuditLogProvider, init_audit_logger};

let config = AuditLogConfig {
    service_name: "my-service".to_string(),  // Optional: defaults to env var if empty
    workflow_id: None,
    node_id: None,
    provider_config: ProviderConfig::File(FileProviderConfig {
        log_dir: "./audit-logs".to_string(),
        max_files: 100,
        rotation_size_mb: 100,
    }),
};

init_audit_logger(FileAuditLogProvider::new(), config).unwrap();
```

### MQTT Provider (for sigbot)

```rust
use common_audit_log::{AuditLogConfig, ProviderConfig, MqttProviderConfig, MqttAuditLogProvider, init_audit_logger};

let config = AuditLogConfig {
    service_name: "sigbot-order-service".to_string(),  // Optional: defaults to env var if empty
    workflow_id: Some("trading-001".to_string()),
    node_id: Some("node-1".to_string()),
    provider_config: ProviderConfig::Mqtt(MqttProviderConfig {
        host: "localhost".to_string(),
        port: 1883,
        client_id: "order-service-audit".to_string(),
        topic_prefix: Some("/internal/v1".to_string()),
        tenant_id: "sigbot".to_string(),
        username: None,
        password: None,
    }),
};

init_audit_logger(MqttAuditLogProvider::new(), config).unwrap();
```

## Feature Flags

- `default`: Enables file provider
- `file-provider`: File-based audit logging
- `mqtt-provider`: MQTT-based audit logging
- `sigbot`: Enables both providers for sigbot integration

## Environment Variables

- `SIGBOT_SERVICE_NAME`: Primary default for service name
- `SERVICE_NAME`: Fallback default for service name

## Architecture

```
┌─────────────────────────────────────┐
│     Application Code                │
│  ┌─────────────┐  ┌──────────────┐ │
│  │ audit_info! │  │ #[audit_log] │ │
│  └──────┬──────┘  └──────┬───────┘ │
│         │                │          │
│         └───────┬────────┘          │
│                 │                   │
│         ┌───────▼────────┐          │
│         │  AuditLogger   │          │
│         │  (global)      │          │
│         └───────┬────────┘          │
│                 │                   │
│         ┌───────▼────────┐          │
│         │ AuditLogProvider│         │
│         │    (trait)      │         │
│         └───────┬────────┘          │
└─────────────────┼───────────────────┘
                  │
    ┌─────────────┴─────────────┐
    │                           │
┌───▼────────┐         ┌────────▼──────┐
│FileProvider│         │MqttProvider   │
│(default)   │         │(sigbot/EMQX)  │
└────────────┘         └───────────────┘
```
