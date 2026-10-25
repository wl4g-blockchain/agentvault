#[test]
fn agent_module_exports_the_mcp_helper() {
    assert!(std::any::type_name::<super::mcp::ToolResult>().ends_with("ToolResult"));
}
