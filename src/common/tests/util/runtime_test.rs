use super::*;
#[tokio::test]
async fn supervisor_rejects_an_empty_runtime() {
    let error = ServerSupervisor::new(std::time::Duration::from_secs(1))
        .run()
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no runtime component"));
}
