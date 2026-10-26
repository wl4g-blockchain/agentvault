use super::*;
#[test]
fn system_clock_exposes_nonnegative_epoch_time() {
    assert!(SystemUnixClock.now().unwrap() >= 0);
}
