use common_telemetry::tracing_context::MessageTrace;

#[test]
fn a_noop_global_tracer_still_creates_distinct_real_message_children() {
    opentelemetry::global::set_tracer_provider(opentelemetry::trace::noop::NoopTracerProvider::new());
    let parent = "00-11111111111111111111111111111111-2222222222222222-01";
    let first = MessageTrace::child("message.consume", parent).traceparent();
    let second = MessageTrace::child("message.publish", &first).traceparent();
    let first: Vec<_> = first.split('-').collect();
    let second: Vec<_> = second.split('-').collect();
    assert_eq!(first[1], "11111111111111111111111111111111");
    assert_eq!(first[1], second[1]);
    assert_ne!(first[2], "2222222222222222");
    assert_ne!(first[2], second[2]);
}
