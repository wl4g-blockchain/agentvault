# Audit Log Usage Guide for Sigbot Microservices

This guide demonstrates how to use the new `common-audit-log` library in sigbot microservices.

## Overview

The `common-audit-log` library provides:

1. **Manual Logging** (`audit_info!()`, `audit_warn!()`, etc.) - "手动挡"
2. **Automatic Logging** (`#[audit_log]` macro) - "自动挡"
3. **Pluggable Providers** - File (default) or MQTT (for sigbot EMQX integration)
4. **Optional Service Name** - Defaults to `SIGBOT_SERVICE_NAME` or `SERVICE_NAME` environment variables

## Quick Start

### 1. Add Dependency

In your microservice's `Cargo.toml`:

```toml
[dependencies]
common-audit-log = { workspace = true, features = ["sigbot"] }  # For MQTT provider
common-audit-log = { workspace = true }  # For file provider (default)
common-macro = { workspace = true }  # For #[audit_log] macro
```

### 2. Initialize Audit Logger

In your service's `main.rs` or initialization code:

```rust
use common_audit_log::{
    init_audit_logger, AuditLogConfig, ProviderConfig,
    MqttProviderConfig, FileAuditLogProvider, MqttAuditLogProvider,
    shutdown_audit_logger,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Option 1: File provider (default, cross-project)
    let config = AuditLogConfig {
        service_name: "order-service".to_string(),  // Can be empty to use env default
        workflow_id: Some("trading-001".to_string()),
        node_id: Some("node-1".to_string()),
        provider_config: ProviderConfig::File(Default::default()),
    };
    init_audit_logger(FileAuditLogProvider::new(), config)?;

    // Option 2: MQTT provider (sigbot-specific, publishes to EMQX)
    let config = AuditLogConfig {
        service_name: "".to_string(),  // Empty = use SIGBOT_SERVICE_NAME env var
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
    init_audit_logger(MqttAuditLogProvider::new(), config)?;

    // ... rest of your service initialization

    // Shutdown on exit
    shutdown_audit_logger().await?;

    Ok(())
}
```

### 3. Set Environment Variable (Optional)

Set the service name via environment variable:

```bash
export SIGBOT_SERVICE_NAME=order-service
# or
export SERVICE_NAME=order-service
```

Or in your deployment configuration (Kubernetes, Docker, etc.):

```yaml
env:
  - name: SIGBOT_SERVICE_NAME
    value: "order-service"
```

## Manual Logging (手动挡)

Use the audit macros for explicit, manual audit logging:

```rust
use common_audit_log::{audit_info, audit_warn, audit_error, audit_debug, audit_with_meta, AuditLogLevel};

// With explicit service name
async fn process_order(order_id: &str, amount: f64) -> Result<(), Error> {
    audit_info!("order-service", "Processing order: {}", order_id).await?;

    if amount > 10000.0 {
        audit_warn!("order-service", "Large order detected: {} USDT", amount).await?;
    }

    // ... process order

    audit_info!("order-service", "Order completed: {}", order_id).await?;
    Ok(())
}

// With default service name (from env/config) - service parameter is optional!
async fn process_order_default(order_id: &str, amount: f64) -> Result<(), Error> {
    // Uses SIGBOT_SERVICE_NAME or SERVICE_NAME env var
    audit_info!("Processing order: {}", order_id).await?;
    audit_warn!("Large order detected: {} USDT", amount).await?;

    // ... process order

    Ok(())
}

// With metadata (JSON)
async fn execute_trade(trade: &Trade) -> Result<(), Error> {
    let metadata = serde_json::json!({
        "trade_id": trade.id,
        "symbol": trade.symbol,
        "side": trade.side,
        "price": trade.price,
        "quantity": trade.quantity
    });

    audit_with_meta!(
        "order-service",
        AuditLogLevel::Info,
        "Trade executed",
        &metadata.to_string()
    ).await?;

    Ok(())
}

// Error logging with default service
async fn handle_error(ctx: &Context) {
    if let Err(e) = some_operation().await {
        audit_error!("Operation failed: {}", e).await.unwrap_or(());
    }
}
```

## Automatic Logging (自动挡)

Use the `#[audit_log]` attribute macro for automatic function entry/exit logging:

```rust
use common_macro::audit_log;

// Basic usage - logs function entry with arguments and exit
#[audit_log(service = "order-service")]
async fn process_order(order_id: String, quantity: u32) -> Result<Order, Error> {
    // Function body - entry and exit are automatically logged
    Ok(order)
}

// Service is optional - defaults to env var if not specified
#[audit_log]
async fn process_payment(amount: f64) -> Result<(), Error> {
    // Automatically uses SIGBOT_SERVICE_NAME env var
    Ok(())
}

// Custom log level
#[audit_log(service = "wallet-service", level = "warn")]
async fn transfer_funds(wallet_id: i64, amount: f64) -> Result<(), Error> {
    // Logged at WARN level
    Ok(())
}

// Skip sensitive arguments (e.g., API keys, passwords)
#[audit_log(service = "exchange-service", skip_args = "1,2")]
async fn place_order(api_key: &str, api_secret: &str, order: &Order) -> Result<(), Error> {
    // api_secret (index 1) and order (index 2) won't be logged
    Ok(())
}

// With additional metadata
#[audit_log(service = "strategy-service", metadata = "{\"strategy_id\": \"momentum_001\"}")]
fn calculate_signal(price: f64, rsi: f64) -> Signal {
    // Logs include the static metadata
    Signal::Buy
}

// Combined options
#[audit_log(service = "order-service", level = "info", skip_args = "1", metadata = "{\"module\": \"trading\"}")]
async fn execute_trade(trade_id: String, _credentials: &Credentials, amount: f64) -> Result<(), Error> {
    // - Logs at INFO level
    // - Skips logging 'credentials' argument (index 1)
    // - Includes static metadata
    Ok(())
}
```

## What Gets Logged

### Manual Logging
- Exactly what you specify with the macros
- Full control over message content and metadata

### Automatic Logging (`#[audit_log]`)
- **Entry**: `ENTER function_name(arg1=value1, arg2=value2)`
- **Exit**: `EXIT function_name (duration: Xms)`
- **Abort**: `ABORT function_name (panic or early return)` if function panics

## Provider Options

### File Provider (Default)
- Writes to `./audit-logs/audit-YYYY-MM-DD.log`
- Automatic daily rotation
- Configurable retention

### MQTT Provider (Sigbot)
- Publishes to EMQX MQTT broker
- Topic format: `/internal/v1/{tenant_id}/audit/log`
- Integrates with sigbot's log service for automatic archiving to PostgreSQL

## Best Practices

1. **Set Service Name via Env**: Use `SIGBOT_SERVICE_NAME` environment variable for consistent service identification

2. **Initialize Early**: Call `init_audit_logger()` in your service's main function before any business logic

3. **Use Appropriate Levels**:
   - `INFO`: Normal operations
   - `WARN`: Unusual but handled situations
   - `ERROR`: Errors that need attention
   - `DEBUG`: Detailed troubleshooting info

4. **Skip Sensitive Data**: Use `skip_args` to avoid logging passwords, API keys, etc.

5. **Include Context**: Use metadata to add structured context (trade IDs, order IDs, etc.)

6. **Shutdown Gracefully**: Call `shutdown_audit_logger().await` on service shutdown

## Example: Complete Microservice

```rust
// src/main.rs
use common_audit_log::{
    init_audit_logger, AuditLogConfig, ProviderConfig,
    MqttProviderConfig, MqttAuditLogProvider,
    shutdown_audit_logger, audit_info,
};
use common_macro::audit_log;

#[audit_log(service = "order-service")]  // Or just #[audit_log] to use env default
async fn handle_order(order_id: String, quantity: u32) -> Result<(), Error> {
    // Business logic here - automatically logged!
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Set service name from env (or use empty string to auto-detect)
    let service_name = std::env::var("SIGBOT_SERVICE_NAME")
        .unwrap_or_else(|_| "order-service".to_string());

    // Initialize audit logger with MQTT provider
    let config = AuditLogConfig {
        service_name,
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
    init_audit_logger(MqttAuditLogProvider::new(), config)?;

    audit_info!("order-service", "Service starting up").await?;
    // Or with default service: audit_info!("Service starting up").await?;

    // Run service...
    handle_order("order-123".to_string(), 100).await?;

    // Shutdown
    shutdown_audit_logger().await?;

    Ok(())
}
```

## Migration from Direct insert_log

If you were previously using direct `insert_log()` calls in tests:

**Before:**
```rust
insert_log(&pool, "order-service", "INFO", "Order processed", None).await?;
```

**After (Manual with explicit service):**
```rust
audit_info!("order-service", "Order processed").await?;
```

**After (Manual with default service):**
```rust
audit_info!("Order processed").await?;  // Uses SIGBOT_SERVICE_NAME
```

**After (Automatic):**
```rust
#[audit_log(service = "order-service")]
async fn process_order(order_id: String) -> Result<(), Error> {
    // Automatically logged!
}

// Or with default service:
#[audit_log]
async fn process_order(order_id: String) -> Result<(), Error> {
    // Automatically logged with service from env!
}
```
