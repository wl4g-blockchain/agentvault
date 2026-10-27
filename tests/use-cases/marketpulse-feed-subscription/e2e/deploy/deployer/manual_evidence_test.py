"""Reject financially plausible payments bound to another identity, resource or chain."""
from __future__ import annotations
import json
from contextlib import contextmanager
from pathlib import Path
from types import SimpleNamespace
import sys
import unittest
from unittest.mock import Mock, patch

ROOT=Path(__file__).resolve().parents[6]
sys.path.insert(0,str(ROOT / "use-cases/marketpulse-feed-subscription/e2e"))
from common.payer_identity import LANGUAGES, payer_identity
from deploy.deployer.manual_evidence import ManualPaymentEvidence
from deploy.deployer.manual_profiles import ManualProfiles


class ManualEvidenceTest(unittest.TestCase):
    def fixture(self,profile="evm"):
        value=object.__new__(ManualPaymentEvidence)
        value.profile=profile; value.payer="rust"; value.payee="go"; value.message="a2a"
        value.state={"solana":{"network":"solana:test-network","mint":"mint-test",
                               "recipient":"recipient-solana","tempo_recipient":"0x"+"3"*40}}
        value.asset=ManualProfiles.asset(profile,value.state["solana"])
        value.network=value.asset["network"]
        suffix=ManualProfiles.suffix(profile)
        identity=payer_identity("rust")
        selected=identity.wallet_ids[0]+suffix
        resource=ManualProfiles.origin("go",profile)+"/a2a"
        common={"run_id":"run-1","protocol":"mpp" if profile=="tempo" else "x402","status":"settled",
                "amount":"1000000","tx_hash":"0xtx","resource":resource,"recipient":value.recipient(),
                "network":value.asset["network"],"asset":value.asset["asset"],
                "token_contract":value.asset["token_contract"],"asset_decimals":value.asset["decimals"]}
        value.intents=[dict(common,id="in-1",scenario_id="scenario-marketpulse-feed"+suffix,
                            policy_id="policy-marketpulse-sales"+suffix,price_id=f"price-go-{profile}-a2a",
                            wallet_id="wallet-marketpulse-revenue"+suffix,direction="inbound"),
                       dict(common,id="out-1",scenario_id=identity.scenario_id+suffix,
                            policy_id=identity.policy_id+suffix,price_id=None,wallet_id=selected,direction="outbound")]
        value.ledger=[{"id":"li","intent_id":"in-1","wallet_id":"wallet-marketpulse-revenue"+suffix,
                       "direction":"inbound","amount":"1000000","tx_hash":"0xtx"},
                      {"id":"lo","intent_id":"out-1","wallet_id":selected,
                       "direction":"outbound","amount":"1000000","tx_hash":"0xtx"}]
        value.rows=lambda sql:value.ledger if "pay_ledger" in sql else value.intents
        value.receipt=Mock(return_value=({"status":"0x1","transactionHash":"0xtx"},12))
        value.before={key:2000000 for key in value.wallet_ids()}
        value.before.update(recipient=0,fee_payer_native=1000)
        if profile=="solana": value.before.update({"native/"+key:10000 for key in value.wallet_ids()})
        value.after=dict(value.before)
        value.after[selected]-=1000000+(12 if profile=="tempo" else 0)
        value.after["recipient"]+=1000000
        if profile!="tempo":value.after["fee_payer_native"]-=12
        value.balances=lambda:value.after
        return value

    def test_payment_requires_exact_requested_scope_two_ledgers_and_fee_conservation(self):
        for profile in ManualProfiles.NAMES:
            value=self.fixture(profile)
            result=value.verify(set(),value.before)
            self.assertEqual(result["run_id"],"run-1")
            self.assertEqual(result["transaction"],"0xtx")
            self.assertEqual(result["fee"],"12")
            self.assertEqual(len(result["ledger"]),2)
            value.receipt.assert_called_once_with("0xtx")

    def test_missing_or_duplicate_ledger_side_is_rejected(self):
        for ledger in ([{"direction":"outbound","amount":"1000000","tx_hash":"0xtx"}],
                       [{"direction":"inbound","amount":"1000000","tx_hash":"0xtx"}]*2):
            value=self.fixture(); value.ledger=ledger
            with self.assertRaisesRegex(RuntimeError,"matching ledger facts"):value.verify(set(),value.before)

    def test_wrong_payer_pool_price_policy_or_scenario_is_rejected(self):
        for row,field,bad in ((1,"wallet_id","wallet-quantstream-go-primary"),(1,"scenario_id","other-scenario"),
                              (1,"policy_id","other-policy"),(1,"price_id","inbound-only-price"),
                              (0,"price_id","price-java-evm-mcp"),(0,"wallet_id","another-recipient")):
            value=self.fixture(); value.intents[row][field]=bad
            with self.assertRaisesRegex(RuntimeError,"different payer pool or payee price"):value.verify(set(),value.before)

    def test_wrong_resource_protocol_asset_or_recipient_is_rejected(self):
        for field,bad in (("resource","http://another-payee/mcp"),("protocol","mpp"),("network","eip155:1"),
                          ("asset","OTHER"),("token_contract","0x"+"9"*40),("asset_decimals",18),
                          ("recipient","0x"+"8"*40)):
            value=self.fixture(); value.intents[0][field]=bad
            with self.assertRaisesRegex(RuntimeError,"resource or chain asset"):value.verify(set(),value.before)

    def test_ledger_cannot_bind_another_wallet_even_when_amount_and_tx_match(self):
        value=self.fixture();value.ledger[1]["wallet_id"]="wallet-quantstream-go-primary"
        with self.assertRaisesRegex(RuntimeError,"matching ledger facts"):value.verify(set(),value.before)

    def test_unrelated_wallet_debit_wrong_native_fee_or_recipient_delta_is_rejected(self):
        for key,pattern in (("wallet-quantstream-go-primary","wallet isolation"),("fee_payer_native","native balance delta"),
                            ("recipient","recipient")):
            value=self.fixture();value.after[key]-=1
            with self.assertRaisesRegex(RuntimeError,pattern):value.verify(set(),value.before)
        value=self.fixture("solana");value.after["native/wallet-quantstream-rust-primary-solana"]-=1
        with self.assertRaisesRegex(RuntimeError,"native balance changed"):value.verify(set(),value.before)

    def test_cross_run_or_cross_transaction_pair_is_rejected(self):
        for field in ("run_id","tx_hash"):
            value=self.fixture(); value.intents[1][field]="different"
            with self.assertRaisesRegex(RuntimeError,"one chain transaction"):value.verify(set(),value.before)

    def test_reverted_or_mismatched_rpc_receipt_is_rejected(self):
        @contextmanager
        def forward(*args):yield 12345
        for profile,wire,pattern in (
            ("evm",{"status":"0x0"},"did not confirm"),
            ("evm",{"status":"0x1","transactionHash":"another-tx"},"identity differs"),
            ("solana",{"meta":{"err":{"InstructionError":[0,"failed"]},"fee":5000}},"did not confirm"),
            ("solana",{"meta":{"err":None,"fee":5000},"transaction":{"signatures":["another-tx"]}},"identity differs")):
            value=self.fixture(profile)
            value.d=SimpleNamespace(_forward_service=forward,_json_rpc_result=Mock(return_value=wire))
            with self.assertRaisesRegex(RuntimeError,pattern):ManualPaymentEvidence.receipt(value,"tx")

    def test_stale_or_future_chain_clock_is_rejected_before_spending(self):
        @contextmanager
        def forward(*args): yield 1234
        for profile in ("solana", "tempo"):
            for offset in (-61, 61, 0):
                value = self.fixture(profile)
                stamp = 1_000_000 + offset
                answers = [123, stamp] if profile == "solana" else [{"timestamp": hex(stamp)}]
                value.d = SimpleNamespace(_forward_service=forward, _json_rpc_result=Mock(side_effect=answers))
                with patch("deploy.deployer.manual_evidence.time.time", return_value=1_000_000):
                    if offset:
                        with self.assertRaisesRegex(RuntimeError, "refusing to create a payment Job"):
                            value.validate_clock()
                    else:
                        self.assertEqual(value.validate_clock()["skew_seconds"], 0)

    def test_local_anvil_preparation_mines_one_wallclock_block_then_checks_clock(self):
        @contextmanager
        def forward(*args): yield 1234
        value=self.fixture()
        rpc=Mock(side_effect=[{"timestamp":hex(900_000)}, "anvil/v1.7.1", "0x7a69", "0x0",
                              {"timestamp":hex(1_000_000)}])
        value.d=SimpleNamespace(_forward_service=forward,_json_rpc_result=rpc)
        with patch("deploy.deployer.manual_evidence.time.time",return_value=1_000_000):
            self.assertEqual(value.validate_clock()["skew_seconds"],0)
        self.assertEqual([call.args[2] for call in rpc.call_args_list],
            ["eth_getBlockByNumber","web3_clientVersion","eth_chainId","evm_mine","eth_getBlockByNumber"])
        self.assertEqual(rpc.call_args_list[3].args[3],[1_000_000])

    def test_anvil_preflight_rejects_other_nodes_future_time_and_failed_refresh(self):
        @contextmanager
        def forward(*args): yield 1234
        for initial,version,chain,final,pattern in (
            (900_000,"geth/v1","0x7a69",1_000_000,"requires local Anvil"),
            (900_000,"anvil/v1","0x1",1_000_000,"requires local Anvil"),
            (1_000_061,"anvil/v1","0x7a69",1_000_000,"refusing to create"),
            (900_000,"anvil/v1","0x7a69",900_000,"refusing to create")):
            value=self.fixture()
            rpc=Mock(side_effect=[{"timestamp":hex(initial)},version,chain,"0x0",{"timestamp":hex(final)}])
            value.d=SimpleNamespace(_forward_service=forward,_json_rpc_result=rpc)
            with patch("deploy.deployer.manual_evidence.time.time",return_value=1_000_000):
                with self.assertRaisesRegex(RuntimeError,pattern):value.validate_clock()
            if initial>1_000_060 or version.startswith("geth") or chain=="0x1":
                self.assertNotIn("evm_mine",[call.args[2] for call in rpc.call_args_list])

    def test_absent_evm_contract_refuses_job_with_explicit_error(self):
        @contextmanager
        def forward(*args): yield 1234
        value=self.fixture()
        value.rows=lambda sql:[{"id":key,"address":"0x"+"1"*40} for key in value.wallet_ids()]
        value.d=SimpleNamespace(_forward_service=forward,_json_rpc_result=Mock(return_value="0x"))
        with self.assertRaisesRegex(RuntimeError,"token contract missing.*refusing to create"):
            ManualPaymentEvidence.balances(value)
