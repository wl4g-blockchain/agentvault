use std::env;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Once;

use common_error::mock::MockError;

use common_error::status_code::StatusCode;
use once_cell::sync::Lazy;
use tracing::Level;
use tracing_appender::non_blocking::WorkerGuard;

use crate::init_global_logging;

use crate::logging::LoggingOptions;

use crate::logging::TracingOptions;

macro_rules! all_log_macros {
    ($($arg:tt)*) => {
        trace!($($arg)*);
        debug!($($arg)*);
        info!($($arg)*);
        warn!($($arg)*);
        error!($($arg)*);
    };
}

static GLOBAL_UT_LOG_GUARD: Lazy<Arc<Mutex<Option<Vec<WorkerGuard>>>>> = Lazy::new(|| Arc::new(Mutex::new(None)));

/// Init tracing for unittest. Write logs to file `unittest`.
fn init_default_ut_logging() {
    static START: Once = Once::new();

    START.call_once(|| {
        let mut g = GLOBAL_UT_LOG_GUARD.as_ref().lock().unwrap();

        // When running in Github's actions, env "UNITTEST_LOG_DIR" is set to a directory other
        // than "/tmp".
        // This is to fix the problem that the "/tmp" disk space of action runner's is small,
        // if we write testing logs in it, actions would fail due to disk out of space error.
        let dir =
            env::var("UNITTEST_LOG_DIR").unwrap_or_else(|_| "/tmp/__unittest_logs".to_string());

        let level = env::var("UNITTEST_LOG_LEVEL").unwrap_or_else(|_|
            "debug,hyper=warn,tower=warn,datafusion=warn,reqwest=warn,sqlparser=warn,h2=info,opendal=info,rskafka=info".to_string()
        );
        let opts = LoggingOptions {
            dir: dir.clone(),
            level: Some(level),
            ..Default::default()
        };
        *g = Some(init_global_logging(
            "unittest",
            &opts,
            &TracingOptions::default(),
            None
        ));

        crate::info!("logs dir = {}", dir);
    });
}

#[test]
fn test_log_args() {
    log!(target: "my_target", Level::TRACE, "foo");
    log!(target: "my_target", Level::DEBUG, "foo",);

    log!(target: "my_target", Level::INFO, "foo: {}", 3);
    log!(target: "my_target", Level::WARN, "foo: {}", 3,);

    log!(target: "my_target", Level::ERROR, "hello {world}", world = "world");
    log!(target: "my_target", Level::DEBUG, "hello {world}", world = "world",);

    all_log_macros!(target: "my_target", "foo");
    all_log_macros!(target: "my_target", "foo",);

    all_log_macros!(target: "my_target", "foo: {}", 3);
    all_log_macros!(target: "my_target", "foo: {}", 3,);

    all_log_macros!(target: "my_target", "hello {world}", world = "world");
    all_log_macros!(target: "my_target", "hello {world}", world = "world",);
}

#[test]
fn test_log_no_target() {
    log!(Level::DEBUG, "foo");
    log!(Level::DEBUG, "foo: {}", 3);

    all_log_macros!("foo");
    all_log_macros!("foo: {}", 3);
}

#[test]
fn test_log_ref_scope_args() {
    let bar = 35;
    let world = "world";
    log!(target: "my_target", Level::DEBUG, "bar: {bar}");
    log!(target: "my_target", Level::DEBUG, "bar: {bar}, hello {}", world);
    log!(target: "my_target", Level::DEBUG, "bar: {bar}, hello {world}",);

    all_log_macros!(target: "my_target", "bar: {bar}");
    all_log_macros!(target: "my_target", "bar: {bar}, hello {}", world);
    all_log_macros!(target: "my_target", "bar: {bar}, hello {world}",);
}

#[test]
fn test_log_error() {
    init_default_ut_logging();

    let err = MockError::new(StatusCode::Unknown);
    let err_ref = &err;
    let err_ref2 = &err_ref;

    error!(target: "my_target", "hello {}", "world");
    // Supports both owned and reference type.
    error!(err; target: "my_target", "hello {}", "world");
    error!(%err; target: "my_target", "hello {}", "world");
    error!(err_ref; target: "my_target", "hello {}", "world");
    error!(err_ref2; "hello {}", "world");
    error!(%err_ref2; "hello {}", "world");
    error!("hello {}", "world");

    let root_err = MockError::with_source(err);
    error!(root_err; "Error with source hello {}", "world");
}
