use super::*;
#[test]
fn wall_clock_observation_is_within_the_system_time_window() {
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let actual = SystemUnixClock.now().unwrap();
    let after = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!((i64::try_from(before).unwrap()..=i64::try_from(after).unwrap()).contains(&actual));
}
