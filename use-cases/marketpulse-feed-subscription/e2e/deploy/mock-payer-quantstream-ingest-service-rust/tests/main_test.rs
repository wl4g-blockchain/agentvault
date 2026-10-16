use super::run_once_requested;

#[test]
fn command_line_accepts_only_the_single_once_switch() {
    assert!(!run_once_requested(&[]).expect("server mode"));
    assert!(run_once_requested(&["--once".to_owned()]).expect("one-shot mode"));
    assert!(run_once_requested(&["--unknown".to_owned()]).is_err());
    assert!(run_once_requested(&["--once".to_owned(), "extra".to_owned()]).is_err());
}
