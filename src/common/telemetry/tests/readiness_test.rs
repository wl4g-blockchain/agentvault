use common_telemetry::readiness::RuntimeReadiness;

#[tokio::test]
async fn startup_disconnect_task_failure_and_successful_finite_completion_have_distinct_health() {
    assert!(!RuntimeReadiness::snapshot().0);
    assert!(RuntimeReadiness::liveness_snapshot().0);
    let session = RuntimeReadiness::task("mqtt");
    session.set(true);
    assert!(
        !RuntimeReadiness::snapshot().0,
        "a connected broker is not an initialized consumer"
    );
    RuntimeReadiness::business("consumer", false);
    assert!(!RuntimeReadiness::snapshot().0);
    assert!(RuntimeReadiness::liveness_snapshot().0);
    RuntimeReadiness::business("consumer", true);
    assert!(RuntimeReadiness::snapshot().0);
    session.set(false);
    assert!(!RuntimeReadiness::snapshot().0);
    assert!(
        RuntimeReadiness::liveness_snapshot().0,
        "disconnect must not trigger a restart"
    );
    session.set(true);
    assert!(RuntimeReadiness::snapshot().0);
    drop(session);
    assert!(!RuntimeReadiness::liveness_snapshot().0);
    assert!(
        !RuntimeReadiness::snapshot().0,
        "a failed or aborted critical task must revoke readiness"
    );
    let replacement = RuntimeReadiness::task("mqtt");
    replacement.set(true);
    assert!(RuntimeReadiness::snapshot().0);
    assert!(RuntimeReadiness::liveness_snapshot().0);
    let finite = RuntimeReadiness::task("replay");
    assert!(!RuntimeReadiness::snapshot().0);
    finite.complete();
    assert!(
        RuntimeReadiness::snapshot().0,
        "completed replay must not cause a restart loop"
    );
    assert!(RuntimeReadiness::liveness_snapshot().0);

    // Explicit permanent failure cannot be cleared by a later dependency recovery
    // or successful shutdown of the same failed task incarnation.
    let failed = RuntimeReadiness::task("capture");
    failed.set(true);
    failed.fail();
    failed.set(true);
    failed.finish();
    assert!(!RuntimeReadiness::snapshot().0);
    assert!(!RuntimeReadiness::liveness_snapshot().0);
    let current = RuntimeReadiness::task("capture");
    current.set(true);
    drop(failed);
    assert!(
        RuntimeReadiness::liveness_snapshot().0,
        "old guard must not poison its replacement"
    );

    // A guard moved into the future detects abort even before its first poll.
    let aborted = RuntimeReadiness::task("aborted");
    let task = tokio::spawn(async move {
        let _guard = aborted;
        std::future::pending::<()>().await;
    });
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(!RuntimeReadiness::liveness_snapshot().0);
    RuntimeReadiness::task("aborted").complete();
    assert!(RuntimeReadiness::liveness_snapshot().0);

    let panicked = RuntimeReadiness::task("panicked");
    let task = tokio::spawn(async move {
        let _guard = panicked;
        panic!("intentional critical task failure");
    });
    assert!(task.await.unwrap_err().is_panic());
    assert!(!RuntimeReadiness::liveness_snapshot().0);
    RuntimeReadiness::task("panicked").complete();
    assert!(RuntimeReadiness::liveness_snapshot().0);

    let gracefully_stopped = RuntimeReadiness::task("graceful");
    gracefully_stopped.finish();
    drop(gracefully_stopped);
    assert!(RuntimeReadiness::liveness_snapshot().0);
    RuntimeReadiness::business("consumer", false);
    assert!(!RuntimeReadiness::snapshot().0);
    assert!(RuntimeReadiness::liveness_snapshot().0);
}
