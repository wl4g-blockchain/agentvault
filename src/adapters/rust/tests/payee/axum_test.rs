use axum::http::Method;

use crate::AxumPayeeRoute;

use super::is_mcp_tool_call;

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
    assert!(is_mcp_tool_call(
        br#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"marketpulse.realtime"}}"#,
        "marketpulse.realtime",
    ));
    assert!(!is_mcp_tool_call(
        br#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"marketpulse.yesterday"}}"#,
        "marketpulse.realtime",
    ));
}
