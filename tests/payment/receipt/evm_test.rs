use super::*;

fn query() -> EvmTransferQuery {
    EvmTransferQuery {
        network: "eip155:8453".to_owned(),
        transaction: format!("0x{}", "11".repeat(32)),
        token_contract: format!("0x{}", "22".repeat(20)),
        payer: format!("0x{}", "33".repeat(20)),
        recipient: format!("0x{}", "44".repeat(20)),
        amount: Amount::new(1_000_000),
    }
}

fn transfer_log(query: &EvmTransferQuery, index: &str) -> RpcLog {
    RpcLog {
        address: query.token_contract.clone(),
        topics: vec![
            ERC20_TRANSFER_TOPIC.to_owned(),
            format!("0x{}{}", "0".repeat(24), &query.payer[2..]),
            format!("0x{}{}", "0".repeat(24), &query.recipient[2..]),
        ],
        data: format!("0x{:064x}", query.amount.value()),
        log_index: Some(index.to_owned()),
        transaction_hash: Some(query.transaction.clone()),
        block_hash: Some(format!("0x{}", "55".repeat(32))),
        block_number: Some("0xa".to_owned()),
        removed: false,
    }
}

fn authorization_query() -> EvmAuthorizationQuery {
    let transfer = query();
    EvmAuthorizationQuery {
        network: transfer.network,
        token_contract: transfer.token_contract,
        payer: transfer.payer,
        nonce: format!("0x{}", "66".repeat(32)),
        recipient: transfer.recipient,
        amount: transfer.amount,
        from_block: 8,
    }
}

fn authorization_log(query: &EvmAuthorizationQuery) -> RpcLog {
    RpcLog {
        address: query.token_contract.clone(),
        topics: vec![
            EvmRpcReceiptSource::authorization_used_topic(),
            EvmRpcReceiptSource::address_topic(&query.payer).unwrap(),
            query.nonce.clone(),
        ],
        data: "0x".to_owned(),
        log_index: Some("0x6".to_owned()),
        transaction_hash: Some(format!("0x{}", "11".repeat(32))),
        block_hash: Some(format!("0x{}", "55".repeat(32))),
        block_number: Some("0xa".to_owned()),
        removed: false,
    }
}

#[test]
fn exact_transfer_requires_a_unique_matching_effect() {
    let query = query();
    let mut receipt = RpcReceipt {
        transaction_hash: query.transaction.clone(),
        block_hash: Some(format!("0x{}", "55".repeat(32))),
        block_number: Some("0xa".to_owned()),
        status: Some("0x1".to_owned()),
        logs: vec![transfer_log(&query, "0x7")],
    };
    assert!(matches!(
        EvmRpcReceiptSource::exact_transfer(&receipt, &query).unwrap(),
        TransferMatch::Found(_)
    ));
    receipt.logs.push(transfer_log(&query, "0x8"));
    assert!(matches!(
        EvmRpcReceiptSource::exact_transfer(&receipt, &query).unwrap(),
        TransferMatch::Ambiguous
    ));
}

#[test]
fn transfer_rejects_wrong_amount_and_noncanonical_quantities() {
    let query = query();
    let mut log = transfer_log(&query, "0x7");
    log.data = format!("0x{:064x}", query.amount.value() + 1);
    let receipt = RpcReceipt {
        transaction_hash: query.transaction.clone(),
        block_hash: Some(format!("0x{}", "55".repeat(32))),
        block_number: Some("0xa".to_owned()),
        status: Some("0x1".to_owned()),
        logs: vec![log],
    };
    assert!(matches!(
        EvmRpcReceiptSource::exact_transfer(&receipt, &query).unwrap(),
        TransferMatch::Missing
    ));
    assert!(EvmRpcReceiptSource::parse_hex_u64("0x00").is_err());
    assert!(EvmRpcReceiptSource::parse_hex_u64("10").is_err());
}

#[test]
fn authorization_event_uniquely_locates_the_settlement_transaction() {
    let authorization = authorization_query();
    let log = authorization_log(&authorization);
    assert_eq!(
        EvmRpcReceiptSource::authorization_transactions(std::slice::from_ref(&log), &authorization).unwrap(),
        vec![format!("0x{}", "11".repeat(32))]
    );
    let receipt = RpcReceipt {
        transaction_hash: format!("0x{}", "11".repeat(32)),
        block_hash: Some(format!("0x{}", "55".repeat(32))),
        block_number: Some("0xa".to_owned()),
        status: Some("0x1".to_owned()),
        logs: vec![log],
    };
    assert!(EvmRpcReceiptSource::receipt_contains_authorization(&receipt, &authorization).unwrap());

    let mut wrong_nonce = authorization;
    wrong_nonce.nonce = format!("0x{}", "77".repeat(32));
    assert!(
        EvmRpcReceiptSource::authorization_transactions(&receipt.logs, &wrong_nonce)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn authorization_log_scan_is_batched_and_globally_bounded() {
    assert_eq!(EvmRpcReceiptSource::bounded_log_ranges(10, 10).unwrap(), vec![(10, 10)]);
    assert_eq!(
        EvmRpcReceiptSource::bounded_log_ranges(10, 2_010).unwrap(),
        vec![(10, 2_009), (2_010, 2_010)]
    );
    assert!(EvmRpcReceiptSource::bounded_log_ranges(0, MAX_AUTHORIZATION_RECOVERY_BLOCKS).is_err());
}

#[test]
fn absence_is_observed_only_through_the_confirmed_chain_prefix() {
    assert_eq!(EvmRpcReceiptSource::confirmed_through_block(10, 0), Some(10));
    assert_eq!(EvmRpcReceiptSource::confirmed_through_block(10, 1), Some(10));
    assert_eq!(EvmRpcReceiptSource::confirmed_through_block(10, 2), Some(9));
    assert_eq!(EvmRpcReceiptSource::confirmed_through_block(0, 2), None);
    assert_eq!(EvmRpcReceiptSource::confirmed_through_block(u64::MAX, 1), None);
}
