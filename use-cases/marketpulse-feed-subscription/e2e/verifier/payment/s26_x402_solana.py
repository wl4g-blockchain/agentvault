from __future__ import annotations

import json
import re

from common.context import VerificationContext
from common.deploy import create_deployer


EVIDENCE_PATTERN = re.compile(r"X402_SOLANA_EVIDENCE=(\{[^\n]+\})")
BASE58_PATTERN = re.compile(r"[1-9A-HJ-NP-Za-km-z]+")


def verify(context: VerificationContext) -> str:
    output = create_deployer(context).x402_solana()
    match = EVIDENCE_PATTERN.search(output)
    if match is None:
        raise RuntimeError("x402 Solana test omitted its machine-readable evidence")
    try:
        evidence = json.loads(match.group(1))
    except json.JSONDecodeError as error:
        raise RuntimeError("x402 Solana test emitted invalid evidence JSON") from error

    required = {
        "version", "scheme", "network", "transaction", "payer", "recipient", "mint", "amount",
        "payer_token_before", "payer_token_after", "recipient_token_before", "recipient_token_after",
        "payer_native_before", "payer_native_after", "facilitator_native_before", "facilitator_native_after",
        "transaction_fee", "verify_before_settle", "broadcast_count",
        "vault_protocol_adapter", "vault_custody_signing",
        "vault_handler_roundtrip", "outbound_receipt_reconciled", "payer_ledger_entries",
        "payee_ledger_entries", "settlement_requested_events",
    }
    if not required.issubset(evidence):
        raise RuntimeError(f"x402 Solana evidence is incomplete: {sorted(required - evidence.keys())}")
    network = str(evidence["network"])
    transaction = str(evidence["transaction"])
    identities = (str(evidence[name]) for name in ("payer", "recipient", "mint"))
    if (
        evidence["version"] != 2
        or evidence["scheme"] != "exact"
        or not re.fullmatch(r"solana:[1-9A-HJ-NP-Za-km-z]{32}", network)
        or not BASE58_PATTERN.fullmatch(transaction)
        or not 80 <= len(transaction) <= 90
        or any(not BASE58_PATTERN.fullmatch(value) or not 32 <= len(value) <= 44 for value in identities)
        or evidence["verify_before_settle"] is not True
        or evidence["broadcast_count"] != 1
        or evidence["vault_protocol_adapter"] is not True
        or evidence["vault_custody_signing"] is not True
        or evidence["vault_handler_roundtrip"] is not True
        or evidence["outbound_receipt_reconciled"] is not True
        or evidence["payer_ledger_entries"] != 1
        or evidence["payee_ledger_entries"] != 1
        or evidence["settlement_requested_events"] != 1
    ):
        raise RuntimeError(f"x402 Solana protocol evidence is invalid: {evidence}")

    amount = int(evidence["amount"])
    payer_before = int(evidence["payer_token_before"])
    payer_after = int(evidence["payer_token_after"])
    recipient_before = int(evidence["recipient_token_before"])
    recipient_after = int(evidence["recipient_token_after"])
    payer_native_before = int(evidence["payer_native_before"])
    payer_native_after = int(evidence["payer_native_after"])
    facilitator_before = int(evidence["facilitator_native_before"])
    facilitator_after = int(evidence["facilitator_native_after"])
    fee = int(evidence["transaction_fee"])
    if amount <= 0 or payer_before - payer_after != amount or recipient_after - recipient_before != amount:
        raise RuntimeError("x402 Solana SPL token deltas do not match the exact payment")
    if payer_before + recipient_before != payer_after + recipient_after:
        raise RuntimeError("x402 Solana aggregate SPL token accounting is not conserved")
    if payer_native_before != payer_native_after:
        raise RuntimeError("x402 Solana payer was charged native gas despite the facilitator fee-payer contract")
    if fee <= 0 or facilitator_before - facilitator_after != fee:
        raise RuntimeError("x402 Solana facilitator lamport delta does not equal the confirmed transaction fee")

    return (
        "real x402 v2/Solana exact payment passed: Vault custody + official client builder -> official facilitator verify -> "
        "one on-chain settle; "
        f"transaction={transaction} amount={amount} fee={fee} broadcasts=1; "
        "payer/payee_ledgers=1/1 trusted_receipt_reconciled=true; "
        f"payer_token={payer_before}->{payer_after} recipient_token={recipient_before}->{recipient_after} "
        f"token_conserved={payer_before + recipient_before}; "
        f"payer_lamports={payer_native_before}->{payer_native_after} "
        f"facilitator_lamports={facilitator_before}->{facilitator_after}"
    )
