use strum::IntoEnumIterator;

use super::*;

fn assert_status_code_display(code: StatusCode, msg: &str) {
    let code_msg = format!("{code}");
    assert_eq!(msg, code_msg);
}

#[test]
fn test_display_status_code() {
    assert_status_code_display(StatusCode::Unknown, "Unknown");
}

#[test]
fn test_from_u32() {
    for code in StatusCode::iter() {
        let num = code as u32;
        assert_eq!(StatusCode::from_u32(num), Some(code));
    }

    assert_eq!(StatusCode::from_u32(10000), None);
}

#[test]
fn test_is_success() {
    assert!(StatusCode::is_success(0));
    assert!(!StatusCode::is_success(1));
    assert!(!StatusCode::is_success(2));
    assert!(!StatusCode::is_success(3));
}
