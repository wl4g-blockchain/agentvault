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
