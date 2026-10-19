use super::*;

#[test]
fn domain_errors_have_stable_http_statuses() {
    assert_eq!(
        ApiError::from(Error::Unauthenticated).into_response().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        ApiError::from(Error::PaymentConflict("duplicate".to_owned()))
            .into_response()
            .status(),
        StatusCode::CONFLICT
    );
}
