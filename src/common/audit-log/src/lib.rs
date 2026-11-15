// SPDX-License-Identifier: GNU GENERAL PUBLIC LICENSE Version 3
//
// Copyright (c) 2027 James Wong. This file is part of James Wong.
// is free software: you can redistribute it and/or modify it under
// the terms of the GNU General Public License as published by the
// Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// James Wong is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with James Wong.  If not, see <https://www.gnu.org/licenses/>.

//! Audit Log Runtime Library
//!
//! This library provides a unified audit log interface with multiple provider implementations:
//! - File provider (default, cross-project)
//! - MQTT provider (for sigbot, publishes to EMQX)
//!
//! Usage:
//! 1. Initialize the audit log system with a provider
//! 2. Use `audit_info!()`, `audit_warn!()`, `audit_error!()`, `audit_debug!()` macros
//! 3. Or use `AuditLogger::log()` for manual logging

use chrono::DateTime;

use chrono::Utc;
use once_cell::sync::OnceCell;
use parking_lot::RwLock;
use serde::Deserialize;
use serde::Serialize;
use std::fmt;
use std::sync::Arc;

/// Audit log level
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum AuditLogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl fmt::Display for AuditLogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuditLogLevel::Debug => write!(f, "DEBUG"),
            AuditLogLevel::Info => write!(f, "INFO"),
            AuditLogLevel::Warn => write!(f, "WARN"),
            AuditLogLevel::Error => write!(f, "ERROR"),
        }
    }
}

/// Audit log entry structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditLogEntry {
    /// Timestamp of the log entry
    pub timestamp: DateTime<Utc>,
    /// Service name that generated the log
    pub service_name: String,
    /// Log level
    pub level: AuditLogLevel,
    /// Log message
    pub message: String,
    /// Optional metadata in JSON format
    pub metadata: Option<String>,
    /// Optional workflow ID for tracing
    pub workflow_id: Option<String>,
    /// Optional node ID
    pub node_id: Option<String>,
}

impl AuditLogEntry {
    #[must_use]
    pub fn new(service_name: String, level: AuditLogLevel, message: String, metadata: Option<String>) -> Self {
        Self {
            timestamp: Utc::now(),
            service_name,
            level,
            message,
            metadata,
            workflow_id: None,
            node_id: None,
        }
    }

    #[must_use]
    pub fn with_workflow_id(mut self, workflow_id: String) -> Self {
        self.workflow_id = Some(workflow_id);
        self
    }

    #[must_use]
    pub fn with_node_id(mut self, node_id: String) -> Self {
        self.node_id = Some(node_id);
        self
    }
}

/// Error type for audit log operations
#[derive(Debug, thiserror::Error)]
pub enum AuditLogError {
    #[error("Provider not initialized")]
    ProviderNotInitialized,
    #[error("Failed to publish log: {0}")]
    PublishFailed(String),
    #[error("Failed to write log: {0}")]
    WriteFailed(String),
    #[error("Configuration error: {0}")]
    ConfigurationError(String),
}

/// Result type for audit log operations
pub type AuditLogResult<T> = Result<T, AuditLogError>;

/// Provider trait for audit log backends
#[async_trait::async_trait]
pub trait AuditLogProvider: Send + Sync {
    /// Initialize the provider
    ///
    /// # Errors
    /// Configuration, provider resource or output initialization failures.
    fn init(&mut self, config: AuditLogConfig) -> AuditLogResult<()>;

    /// Write a single audit log entry
    async fn log(&self, entry: AuditLogEntry) -> AuditLogResult<()>;

    /// Flush any buffered logs
    async fn flush(&self) -> AuditLogResult<()>;

    /// Shutdown the provider
    async fn shutdown(&self) -> AuditLogResult<()>;

    /// Get provider name
    fn name(&self) -> &'static str;
}

/// Audit log configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditLogConfig {
    /// Service name for this instance
    pub service_name: String,
    /// Workflow ID (optional)
    pub workflow_id: Option<String>,
    /// Node ID (optional)
    pub node_id: Option<String>,
    /// Provider-specific configuration
    pub provider_config: ProviderConfig,
}

/// Provider-specific configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "config")]
pub enum ProviderConfig {
    /// File provider configuration
    File(FileProviderConfig),
    /// MQTT provider configuration
    Mqtt(MqttProviderConfig),
}

/// File provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileProviderConfig {
    /// Directory to store log files
    pub log_dir: String,
    /// Maximum number of log files to retain
    pub max_files: usize,
    /// Rotation size in MB
    pub rotation_size_mb: u64,
}

impl Default for FileProviderConfig {
    fn default() -> Self {
        Self {
            log_dir: "./audit-logs".to_string(),
            max_files: 100,
            rotation_size_mb: 100,
        }
    }
}

/// MQTT provider configuration (for sigbot)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MqttProviderConfig {
    /// MQTT broker host
    pub host: String,
    /// MQTT broker port
    pub port: u16,
    /// Client ID
    pub client_id: String,
    /// Topic prefix for audit logs
    pub topic_prefix: Option<String>,
    /// Tenant ID
    pub tenant_id: String,
    /// Optional username
    pub username: Option<String>,
    /// Optional password
    pub password: Option<String>,
}

/// Global audit logger instance
static AUDIT_LOGGER: OnceCell<Arc<RwLock<AuditLogger>>> = OnceCell::new();

/// Return the neutral fallback used when the application did not inject its
/// centrally configured service name.
#[must_use]
pub fn get_default_service_name() -> String {
    "unknown-service".to_owned()
}

/// Audit logger that holds the provider
pub struct AuditLogger {
    provider: Option<Arc<dyn AuditLogProvider>>,
    config: Option<AuditLogConfig>,
    default_service: String,
}

impl AuditLogger {
    /// Create a new audit logger
    #[must_use]
    pub fn new(default_service: String) -> Self {
        Self {
            provider: None,
            config: None,
            default_service,
        }
    }

    /// Set the provider
    pub fn set_provider(&mut self, provider: Arc<dyn AuditLogProvider>) {
        self.provider = Some(provider);
    }

    /// Set the configuration
    pub fn set_config(&mut self, config: AuditLogConfig) {
        self.config = Some(config);
    }

    /// Get the service name from config or default
    #[must_use]
    pub fn service_name(&self) -> String {
        self.config
            .as_ref()
            .map(|c| c.service_name.clone())
            .unwrap_or(self.default_service.clone())
    }

    /// Log an audit entry
    ///
    /// # Errors
    /// Returns an error when no provider is configured or the provider cannot write the entry.
    pub async fn log(&self, level: AuditLogLevel, message: String, metadata: Option<String>) -> AuditLogResult<()> {
        let provider = self.provider.as_ref().ok_or(AuditLogError::ProviderNotInitialized)?;

        let service_name = self.service_name();
        let mut entry = AuditLogEntry::new(service_name, level, message, metadata);

        if let Some(config) = &self.config {
            if let Some(ref wf_id) = config.workflow_id {
                entry = entry.with_workflow_id(wf_id.clone());
            }
            if let Some(ref node_id) = config.node_id {
                entry = entry.with_node_id(node_id.clone());
            }
        }

        provider.log(entry).await
    }

    /// Shutdown the logger
    ///
    /// # Errors
    /// Returns the configured provider shutdown failure.
    pub async fn shutdown(&self) -> AuditLogResult<()> {
        if let Some(ref provider) = self.provider {
            provider.shutdown().await
        } else {
            Ok(())
        }
    }
}

/// Initialize the global audit logger with a provider
///
/// # Errors
/// Returns the provider configuration or initialization failure.
pub fn init_audit_logger<P: AuditLogProvider + 'static>(mut provider: P, config: AuditLogConfig) -> AuditLogResult<()> {
    let service_name = if config.service_name.is_empty() {
        get_default_service_name()
    } else {
        config.service_name.clone()
    };

    // Initialize provider first
    provider.init(config.clone())?;

    AUDIT_LOGGER.get_or_init(|| {
        let mut logger = AuditLogger::new(service_name);
        logger.set_provider(Arc::new(provider));
        logger.set_config(config);
        Arc::new(RwLock::new(logger))
    });

    Ok(())
}

/// Get the global audit logger
pub fn get_audit_logger() -> Option<Arc<RwLock<AuditLogger>>> {
    AUDIT_LOGGER.get().cloned()
}

/// Shutdown the global audit logger
///
/// # Errors
/// Returns the configured provider shutdown failure.
pub async fn shutdown_audit_logger() -> AuditLogResult<()> {
    if let Some(logger) = AUDIT_LOGGER.get() {
        let provider = logger.read().provider.clone();
        if let Some(provider) = provider {
            provider.shutdown().await
        } else {
            Ok(())
        }
    } else {
        Ok(())
    }
}

/// Macro-friendly audit log function with explicit service
///
/// # Errors
/// Returns a configured provider write failure; the console fallback succeeds.
pub async fn audit_log(
    service: &str,
    level: AuditLogLevel,
    message: &str,
    metadata: Option<&str>,
) -> AuditLogResult<()> {
    let effective_service = if service.is_empty() {
        get_default_service_name()
    } else {
        service.to_string()
    };

    if let Some(logger) = get_audit_logger() {
        let provider = logger.read().provider.clone();
        let entry = AuditLogEntry::new(
            effective_service.clone(),
            level,
            message.to_string(),
            metadata.map(std::string::ToString::to_string),
        );

        if let Some(provider) = provider {
            return provider.log(entry).await;
        }
    }

    // Fallback: print to console if no provider is set
    println!("[AUDIT {level}] [{effective_service}] {message}");
    Ok(())
}

/// Macro-friendly audit log function using default service from config/env
///
/// # Errors
/// Returns a configured provider write failure; the console fallback succeeds.
pub async fn audit_log_default(level: AuditLogLevel, message: &str, metadata: Option<&str>) -> AuditLogResult<()> {
    let service_name = get_default_service_name();

    if let Some(logger) = get_audit_logger() {
        let provider = logger.read().provider.clone();
        let entry = AuditLogEntry::new(
            service_name.clone(),
            level,
            message.to_string(),
            metadata.map(std::string::ToString::to_string),
        );

        if let Some(provider) = provider {
            return provider.log(entry).await;
        }
    }

    // Fallback: print to console if no provider is set
    println!("[AUDIT {level}] [{service_name}] {message}");
    Ok(())
}

/// Audit guard for ordered best-effort RAII logging (used by #[`audit_log`] macro).
/// Guard messages are queued on the current Tokio runtime. Call the awaited
/// audit functions directly when delivery errors or durability must be observed.
pub struct AuditGuard {
    service: String,
    level: AuditLogLevel,
    entry_message: String,
    metadata: String,
    start_time: std::time::Instant,
    exited: bool,
    pending: Option<tokio::task::JoinHandle<()>>,
}

impl AuditGuard {
    #[must_use]
    pub fn new(service: Option<&str>, level: AuditLogLevel, entry_message: String, metadata: &str) -> Self {
        let service_name = service
            .filter(|s| !s.is_empty())
            .unwrap_or(&get_default_service_name())
            .to_string();

        let mut guard = Self {
            service: service_name,
            level,
            entry_message,
            metadata: metadata.to_owned(),
            start_time: std::time::Instant::now(),
            exited: false,
            pending: None,
        };
        guard.emit(level, format!("ENTER {}", guard.entry_message));
        guard
    }

    // Synchronous RAII callers cannot await. Queue owned messages in order on
    // the current runtime; without a runtime, retain console best-effort output.
    fn emit(&mut self, level: AuditLogLevel, message: String) {
        let service = self.service.clone();
        let metadata = self.metadata.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let previous = self.pending.take();
            self.pending = Some(runtime.spawn(async move {
                if let Some(previous) = previous {
                    let _ = previous.await;
                }
                if let Err(error) = audit_log(&service, level, &message, Some(&metadata)).await {
                    eprintln!("audit delivery failed: {error}");
                }
            }));
        } else {
            println!("[AUDIT {level}] [{service}] {message}");
        }
    }

    pub fn exit(mut self) {
        self.exited = true;
        let duration = self.start_time.elapsed();
        self.emit(
            self.level,
            format!("EXIT {} (duration: {:?})", self.entry_message, duration),
        );
    }
}

impl Drop for AuditGuard {
    fn drop(&mut self) {
        if !self.exited {
            // Function panicked or returned early
            self.emit(
                AuditLogLevel::Error,
                format!("ABORT {} (panic or early return)", self.entry_message),
            );
        }
    }
}

// Re-export for convenience

/// Log an audit message at INFO level
/// Usage: audit_info!("my-service", "message {}", arg)
///    or: `audit_info!("message` {}", arg)  // uses default service from env/config
#[macro_export]
macro_rules! audit_info {
    ($service:expr, $($arg:tt)*) => {{
        let msg = format!($($arg)*);
        $crate::audit_log($service, $crate::AuditLogLevel::Info, &msg, None).await
    }};
    ($($arg:tt)*) => {{
        let msg = format!($($arg)*);
        $crate::audit_log_default($crate::AuditLogLevel::Info, &msg, None).await
    }};
}

/// Log an audit message at WARN level
/// Usage: audit_warn!("my-service", "message {}", arg)
///    or: `audit_warn!("message` {}", arg)  // uses default service from env/config
#[macro_export]
macro_rules! audit_warn {
    ($service:expr, $($arg:tt)*) => {{
        let msg = format!($($arg)*);
        $crate::audit_log($service, $crate::AuditLogLevel::Warn, &msg, None).await
    }};
    ($($arg:tt)*) => {{
        let msg = format!($($arg)*);
        $crate::audit_log_default($crate::AuditLogLevel::Warn, &msg, None).await
    }};
}

/// Log an audit message at ERROR level
/// Usage: audit_error!("my-service", "message {}", arg)
///    or: `audit_error!("message` {}", arg)  // uses default service from env/config
#[macro_export]
macro_rules! audit_error {
    ($service:expr, $($arg:tt)*) => {{
        let msg = format!($($arg)*);
        $crate::audit_log($service, $crate::AuditLogLevel::Error, &msg, None).await
    }};
    ($($arg:tt)*) => {{
        let msg = format!($($arg)*);
        $crate::audit_log_default($crate::AuditLogLevel::Error, &msg, None).await
    }};
}

/// Log an audit message at DEBUG level
/// Usage: audit_debug!("my-service", "message {}", arg)
///    or: `audit_debug!("message` {}", arg)  // uses default service from env/config
#[macro_export]
macro_rules! audit_debug {
    ($service:expr, $($arg:tt)*) => {{
        let msg = format!($($arg)*);
        $crate::audit_log($service, $crate::AuditLogLevel::Debug, &msg, None).await
    }};
    ($($arg:tt)*) => {{
        let msg = format!($($arg)*);
        $crate::audit_log_default($crate::AuditLogLevel::Debug, &msg, None).await
    }};
}

/// Log an audit message with metadata
/// Usage: audit_with_meta!("my-service", level, "message", metadata)
///    or: `audit_with_meta!(level`, "message", metadata)  // uses default service from env/config
#[macro_export]
macro_rules! audit_with_meta {
    ($service:expr, $level:expr, $msg:expr, $meta:expr) => {{
        $crate::audit_log($service, $level, $msg, Some($meta)).await
    }};
    ($level:expr, $msg:expr, $meta:expr) => {{
        $crate::audit_log_default($level, $msg, Some($meta)).await
    }};
}

// Provider implementations
#[cfg(feature = "file-provider")]
pub mod providers {
    use super::{
        AuditLogConfig, AuditLogEntry, AuditLogError, AuditLogProvider, AuditLogResult, FileProviderConfig,
        ProviderConfig,
    };
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::path::Path;

    /// File-based audit log provider (default implementation)
    pub struct FileAuditLogProvider {
        config: Option<FileProviderConfig>,
    }

    impl Default for FileAuditLogProvider {
        fn default() -> Self {
            Self::new()
        }
    }

    impl FileAuditLogProvider {
        #[must_use]
        pub fn new() -> Self {
            Self { config: None }
        }
    }

    #[async_trait::async_trait]
    impl AuditLogProvider for FileAuditLogProvider {
        fn init(&mut self, config: AuditLogConfig) -> AuditLogResult<()> {
            if let ProviderConfig::File(file_config) = config.provider_config {
                // Create log directory if it doesn't exist
                if let Err(e) = std::fs::create_dir_all(&file_config.log_dir) {
                    return Err(AuditLogError::ConfigurationError(format!(
                        "Failed to create log directory: {e}"
                    )));
                }
                self.config = Some(file_config);
                Ok(())
            } else {
                Err(AuditLogError::ConfigurationError(
                    "Invalid provider config type".to_string(),
                ))
            }
        }

        async fn log(&self, entry: AuditLogEntry) -> AuditLogResult<()> {
            let config = self.config.as_ref().ok_or(AuditLogError::ProviderNotInitialized)?;

            let log_file =
                Path::new(&config.log_dir).join(format!("audit-{}.log", chrono::Utc::now().format("%Y-%m-%d")));

            let log_line = format!(
                "{} [{}] [{}] {}{}\n",
                entry.timestamp.to_rfc3339(),
                entry.level,
                entry.service_name,
                entry.message,
                entry.metadata.as_ref().map(|m| format!(" | {m}")).unwrap_or_default()
            );

            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_file)
                .map_err(|e| AuditLogError::WriteFailed(e.to_string()))?;

            file.write_all(log_line.as_bytes())
                .map_err(|e| AuditLogError::WriteFailed(e.to_string()))?;

            Ok(())
        }

        async fn flush(&self) -> AuditLogResult<()> {
            Ok(())
        }

        async fn shutdown(&self) -> AuditLogResult<()> {
            Ok(())
        }

        fn name(&self) -> &'static str {
            "FileAuditLogProvider"
        }
    }

    /// MQTT-based audit log provider (for sigbot)
    #[cfg(feature = "mqtt-provider")]
    pub struct MqttAuditLogProvider {
        config: Option<MqttProviderConfig>,
        client: tokio::sync::OnceCell<rumqttc::AsyncClient>,
    }

    #[cfg(feature = "mqtt-provider")]
    impl MqttAuditLogProvider {
        pub fn new() -> Self {
            Self {
                config: None,
                client: tokio::sync::OnceCell::new(),
            }
        }
    }

    #[cfg(feature = "mqtt-provider")]
    #[async_trait::async_trait]
    impl AuditLogProvider for MqttAuditLogProvider {
        fn init(&mut self, config: AuditLogConfig) -> AuditLogResult<()> {
            if let ProviderConfig::Mqtt(mqtt_config) = config.provider_config {
                self.config = Some(mqtt_config.clone());

                // Initialize MQTT client asynchronously
                let mqtt_config_clone = mqtt_config.clone();
                let client_cell = self.client.clone();

                tokio::spawn(async move {
                    use rumqttc::AsyncClient;
                    use rumqttc::MqttOptions;
                    use rumqttc::QoS;

                    let mut mqtt_options = MqttOptions::new(
                        &mqtt_config_clone.client_id,
                        &mqtt_config_clone.host,
                        mqtt_config_clone.port,
                    );
                    mqtt_options.set_keep_alive(std::time::Duration::from_secs(30));

                    if let (Some(username), Some(password)) = (mqtt_config_clone.username, mqtt_config_clone.password) {
                        mqtt_options.set_credentials(username, password);
                    }

                    let (client, event_loop) = AsyncClient::new(mqtt_options, 10);

                    // Store client
                    let _ = client_cell.set(client);

                    // Run event loop in background
                    use rumqttc::Event;
                    tokio::spawn(async move {
                        loop {
                            match event_loop.poll().await {
                                Ok(Event::Incoming(_) | Event::Outgoing(_)) => {}
                                Err(_) => break,
                            }
                        }
                    });
                });

                Ok(())
            } else {
                Err(AuditLogError::ConfigurationError(
                    "Invalid provider config type".to_string(),
                ))
            }
        }

        async fn log(&self, entry: AuditLogEntry) -> AuditLogResult<()> {
            let config = self.config.as_ref().ok_or(AuditLogError::ProviderNotInitialized)?;

            let client = self.client.get().ok_or(AuditLogError::ProviderNotInitialized)?;

            // Build topic: /internal/v1/{tenant_id}/{workflow_id}/audit/log
            let topic = match &config.topic_prefix {
                Some(prefix) => format!("{}/{}/audit/log", prefix, config.tenant_id),
                None => format!("/internal/v1/{}/audit/log", config.tenant_id),
            };

            let payload = serde_json::to_string(&entry).map_err(|e| AuditLogError::PublishFailed(e.to_string()))?;

            client
                .publish(&topic, rumqttc::QoS::AtLeastOnce, false, payload.as_bytes())
                .await
                .map_err(|e| AuditLogError::PublishFailed(e.to_string()))?;

            tracing::info!("Published audit log to topic: {}", topic);

            Ok(())
        }

        async fn flush(&self) -> AuditLogResult<()> {
            Ok(())
        }

        async fn shutdown(&self) -> AuditLogResult<()> {
            Ok(())
        }

        fn name(&self) -> &'static str {
            "MqttAuditLogProvider"
        }
    }
}

// Default provider (no-op if no features enabled)
#[cfg(not(any(feature = "file-provider", feature = "mqtt-provider")))]
pub mod providers {
    use super::*;

    /// Default no-op provider
    pub struct FileAuditLogProvider;

    impl FileAuditLogProvider {
        pub fn new() -> Self {
            Self
        }
    }

    #[async_trait::async_trait]
    impl AuditLogProvider for FileAuditLogProvider {
        fn init(&mut self, _config: AuditLogConfig) -> AuditLogResult<()> {
            Ok(())
        }

        async fn log(&self, entry: AuditLogEntry) -> AuditLogResult<()> {
            // Fallback: print to console
            println!("[AUDIT {}] [{}] {}", entry.level, entry.service_name, entry.message);
            Ok(())
        }

        async fn flush(&self) -> AuditLogResult<()> {
            Ok(())
        }

        async fn shutdown(&self) -> AuditLogResult<()> {
            Ok(())
        }

        fn name(&self) -> &'static str {
            "FileAuditLogProvider"
        }
    }
}

// Re-export providers
pub use providers::FileAuditLogProvider;

#[cfg(feature = "mqtt-provider")]
pub use providers::MqttAuditLogProvider;

// Re-export audit_log attribute macro for convenience
// This allows users to import audit_log from common_audit_log directly
pub use common_macro::audit_log;
