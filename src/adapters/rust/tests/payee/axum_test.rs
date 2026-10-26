use super::AxumRequest;
use crate::AxumPayeeRoute;
use axum::http::Method;

#[test]
fn axum_adapter_binds_an_exact_mcp_tool() {
    let route = AxumPayeeRoute::mcp_tool(
        "/mcp",
        "marketpulse.realtime",
        "scenario",
        "offer",
        "mcp://marketpulse/realtime",
        "https://marketpulse.example",
    );
    assert_eq!(route.method, Method::POST);
    assert_eq!(route.mcp_tool.as_deref(), Some("marketpulse.realtime"));
    assert!(AxumRequest::is_mcp_tool_call(
        br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"marketpulse.realtime"}}"#,
        "marketpulse.realtime",
    ));
    assert!(!AxumRequest::is_mcp_tool_call(
        br#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"marketpulse.yesterday"}}"#,
        "marketpulse.realtime",
    ));
    assert!(!AxumRequest::is_mcp_tool_call(
        br#"{"jsonrpc":"1.0","id":1,"method":"tools/call","params":{"name":"marketpulse.realtime"}}"#,
        "marketpulse.realtime",
    ));
}
