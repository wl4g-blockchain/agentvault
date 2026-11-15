use common_audit_log::{
    init_audit_logger, AuditGuard, AuditLogConfig, AuditLogEntry, AuditLogLevel, AuditLogProvider, AuditLogResult,
    FileProviderConfig, ProviderConfig,
};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

struct Recording {
    messages: Arc<Mutex<Vec<String>>>,
    complete: Arc<Notify>,
}
#[async_trait::async_trait]
impl AuditLogProvider for Recording {
    fn init(&mut self, _config: AuditLogConfig) -> AuditLogResult<()> {
        Ok(())
    }
    async fn log(&self, entry: AuditLogEntry) -> AuditLogResult<()> {
        let mut messages = self.messages.lock().unwrap();
        messages.push(entry.message);
        if messages.len() >= 2 {
            self.complete.notify_one();
        }
        Ok(())
    }
    async fn flush(&self) -> AuditLogResult<()> {
        Ok(())
    }
    async fn shutdown(&self) -> AuditLogResult<()> {
        Ok(())
    }
    fn name(&self) -> &'static str {
        "recording"
    }
}
#[tokio::test]
async fn synchronous_guard_queues_enter_then_exit_instead_of_discarding_futures() {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let complete = Arc::new(Notify::new());
    init_audit_logger(
        Recording {
            messages: messages.clone(),
            complete: complete.clone(),
        },
        AuditLogConfig {
            service_name: "framework-test".to_owned(),
            workflow_id: None,
            node_id: None,
            provider_config: ProviderConfig::File(FileProviderConfig::default()),
        },
    )
    .unwrap();
    AuditGuard::new(Some("framework-test"), AuditLogLevel::Info, "work".to_owned(), "").exit();
    tokio::time::timeout(std::time::Duration::from_secs(2), complete.notified())
        .await
        .unwrap();
    let messages = messages.lock().unwrap();
    assert_eq!(messages.len(), 2);
    assert!(messages[0].starts_with("ENTER work"));
    assert!(messages[1].starts_with("EXIT work"));
}
