use super::*;

#[test]
fn challenge_identity_is_stable_across_object_order_but_binds_the_nonce() {
    let first = json!({"x402Version":2,"accepts":[{"amount":"10","network":"eip155:31337"}]});
    let reordered = json!({"accepts":[{"network":"eip155:31337","amount":"10"}],"x402Version":2});
    let nonce = "11".repeat(32);
    let expected = X402Wire::challenge_hash("https://merchant.test/resource", 160, &first, &nonce).unwrap();
    assert_eq!(
        expected,
        X402Wire::challenge_hash("https://merchant.test/resource", 160, &reordered, &nonce).unwrap()
    );
    assert_ne!(
        expected,
        X402Wire::challenge_hash("https://merchant.test/resource", 160, &first, &"22".repeat(32)).unwrap()
    );
    assert!(X402Wire::challenge_hash("https://merchant.test/resource", 160, &first, "not-a-nonce").is_err());
}
