use super::*;

#[test]
fn parses_balance_without_truncating_large_values() {
    assert_eq!(parse_hex_amount("0x0").unwrap(), Amount::ZERO);
    assert_eq!(parse_hex_amount("0x3b9aca00").unwrap(), Amount::new(1_000_000_000));
    assert_eq!(
        parse_hex_amount("0x100000000000000000000000000000000").unwrap(),
        Amount::new(u128::MAX)
    );
    assert!(parse_hex_amount("1").is_err());
}

#[test]
fn requires_https_except_explicit_loopback() {
    assert!(EvmRpcBalanceSource::new(
        "http://example.com/",
        BTreeMap::new(),
        Duration::from_secs(1),
        1024,
        Amount::ZERO,
        true,
    )
    .is_err());
    assert!(EvmRpcBalanceSource::new(
        "http://127.0.0.1:8545/",
        BTreeMap::new(),
        Duration::from_secs(1),
        1024,
        Amount::ZERO,
        true,
    )
    .is_ok());
}
