use common_telemetry::logging::{init_global_logging, LogFormat, LoggingOptions, TracingOptions};
#[test]
fn rolling_json_error_and_slow_streams_keep_their_original_filtering() {
    let directory = tempfile::tempdir().unwrap();
    let options = LoggingOptions {
        dir: directory.path().display().to_string(),
        append_stdout: false,
        enable_otlp_tracing: false,
        log_format: LogFormat::Json,
        slow_query: common_telemetry::logging::SlowQueryOptions {
            enable: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let guards = init_global_logging("framework-test", &options, &TracingOptions::default(), None);
    tracing::info!("ordinary-event");
    tracing::error!("failure-event");
    tracing::info!(slow_operation = true, "slow-event");
    drop(guards);
    let mut main = String::new();
    let mut errors = String::new();
    let mut slow = String::new();
    for entry in std::fs::read_dir(directory.path()).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(entry.path()).unwrap();
        if name.starts_with("application-err") {
            errors = text;
        } else if name.starts_with("application-slow-queries") {
            slow = text;
        } else if name.starts_with("application.") {
            main = text;
        }
    }
    assert!(main.contains("ordinary-event") && main.contains("failure-event") && main.contains("slow-event"));
    assert!(errors.contains("failure-event") && !errors.contains("ordinary-event"));
    assert!(slow.contains("slow-event") && !slow.contains("ordinary-event"));
    for line in main.lines() {
        serde_json::from_str::<serde_json::Value>(line).unwrap();
    }
}
