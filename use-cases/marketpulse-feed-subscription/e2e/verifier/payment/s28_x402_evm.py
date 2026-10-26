"""Explicit Anvil entrypoint for the existing complete x402 EVM workflow."""
from __future__ import annotations

import json

from common.context import VerificationContext
from deploy.deployer import create_deployer


def verify(context: VerificationContext) -> dict[str, str]:
    reports = create_deployer(context).full(context.payer_language, context.payee_language)
    required = {
        "s20_full.md", "s21_payer_signing.md", "s22_payee_settlement.md",
        "s23_balance_sync.md", "s24_gateway_balance.md",
    }
    if set(reports) != required or any(not report.strip() for report in reports.values()):
        raise RuntimeError("x402 EVM acceptance requires every complete-flow verifier")
    evidence = {
        "protocol": "x402-v2-exact-eip3009",
        "network": "eip155:31337",
        "chain": "anvil",
        "chain_settlement": True,
        "payer_language": context.payer_language,
        "payee_language": context.payee_language,
        "transport": context.transport,
        "reports": reports,
    }
    directory = context.report_root / "payment" / "x402-evm"
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "evidence.json").write_text(json.dumps(evidence, indent=2) + "\n", encoding="utf-8")
    return {
        "s28_x402_evm.md": (
            "Real x402 v2/EVM payment on local Anvil passed through the shared full-flow executor.\n\n"
            "Vault custody signing, external HTTP facilitator verify/settle, approval and delivery recovery, "
            "two ERC-20 transfers, four inbound/outbound ledger rows, independent receipts, "
            "ten-wallet isolation, and exact facilitator native-gas conservation were verified.\n\n"
            f"payer={context.payer_language} payee={context.payee_language} transport={context.transport}; "
            "network=eip155:31337; machine evidence=payment/x402-evm/evidence.json; "
            f"financial evidence=payment/full-flow/{context.payer_language}-{context.payee_language}-"
            f"{context.transport}/evidence.json"
        ),
        **reports,
    }
