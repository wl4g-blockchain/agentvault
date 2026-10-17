use super::*;

#[test]
fn renders_bounded_payment_metrics() {
    let metrics = AgentVaultMetrics::default();
    metrics.payment_started();
    metrics.payment_finished("authorize", "outbound", true, 0.01);
    let rendered = metrics.render().unwrap();
    assert!(rendered.contains(
        "agentvault_payment_operations_total{operation=\"authorize\",direction=\"outbound\",outcome=\"success\"} 1"
    ));
}

#[test]
fn payment_operation_guard_records_success_and_early_failure() {
    let metrics = AgentVaultMetrics::default();
    {
        let mut operation = metrics.payment_operation("authorize", "outbound");
        operation.succeed();
    }
    drop(metrics.payment_operation("sign", "outbound"));
    let rendered = metrics.render().unwrap();
    assert!(rendered.contains(
        "agentvault_payment_operations_total{operation=\"authorize\",direction=\"outbound\",outcome=\"success\"} 1"
    ));
    assert!(rendered.contains(
        "agentvault_payment_operations_total{operation=\"sign\",direction=\"outbound\",outcome=\"failure\"} 1"
    ));
    assert!(rendered.contains("agentvault_payments_in_flight 0"));
}
