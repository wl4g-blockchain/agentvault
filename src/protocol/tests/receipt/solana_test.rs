use super::*;
#[test]
fn invalid_solana_locator_is_rejected_before_any_rpc_query() {
    let query = TransferQuery {
        network: "solana:invalid".into(),
        transaction: "not-a-signature".into(),
        token_contract: "bad".into(),
        payer: "bad".into(),
        recipient: "bad".into(),
        amount: Amount::new(1),
    };
    assert!(SolanaRpcReceiptSource::validate_solana_query(&query, "solana:invalid").is_err());
}
