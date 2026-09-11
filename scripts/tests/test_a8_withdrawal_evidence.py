"""Durable evidence across confirmed settlement and partial delivery failures."""
import copy
import importlib.util
import json
import sys
import tempfile
import unittest
from argparse import Namespace
from pathlib import Path
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "a8_evidence_under_test", Path(__file__).resolve().parents[1] / "a8_acceptance.py"
)
a8 = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = a8
SPEC.loader.exec_module(a8)


class PaymentChain:
    def __init__(self, path):
        self.path = path
        self.context = {
            "chain": {"chain_id": "test"},
            "terms": {"target_epoch": 5, "budget_micro_usdt": "100000000",
                      "price_micro_usdt_per_gnk": "1000000", "fee_bps": 150},
            "accounts": {"host": "host", "buyer": "buyer", "fee_recipient": "fees"},
            "contracts": {"deal": "deal", "cw20": "token", "factory": "factory"},
            "key_names": {"host": "host"}, "phases": [],
        }
        self.summary = {"epochPerformanceSummary": {
            "claimed": True, "epoch_index": "5", "participant_id": "host",
            "earned_coins": "80000000000", "rewarded_coins": "0",
        }}
        self.expected = a8.expected_claim_settlement(self.context, self.summary)
        self.settled_state = {key: value for key, value in self.expected.items()
                              if key not in {"cw20_deltas", "deal_outflow", "funded_capacity_ngonka"}}
        self.settled_state["buyer"] = "buyer"
        self.state = {"status": "locked", "buyer": "buyer"}
        self.balances = {"deal": 100000000, "host": 0, "buyer": 0, "fees": 0}
        self.roles = {"host": ("host", 78800000), "fee": ("fees", 1200000),
                      "buyer": ("buyer", 20000000)}
        self.pending = {role: amount for role, (_, amount) in self.roles.items()}
        self.blocked = "fee"
        self.calls = []
        self.query_failure = False
        self.payout_checks = []
        a8.write_object(path, self.context)

    def tx(self, *args, **kwargs):
        return {"txhash": "CLAIM", "code": 0, "height": "9"}

    def query_json(self, module, *args):
        return copy.deepcopy(self.summary) if module == "inference" else {}

    def bank_balance(self, address):
        return 0

    def cw20_balance(self, token, address):
        return self.balances[address]

    def smart(self, address, msg):
        if "state" in msg:
            return copy.deepcopy(self.state)
        if "usdt_payments" in msg:
            if self.query_failure:
                raise a8.AcceptanceError("query unavailable after settlement")
            return {role: {"recipient": recipient, "accrued_micro_usdt": str(amount),
                           "pending_micro_usdt": str(self.pending[role]),
                           "paid_micro_usdt": str(amount - self.pending[role])}
                    for role, (recipient, amount) in self.roles.items()}
        return {}

    def execute(self, node, key, deal, msg, **kwargs):
        self.calls.append(msg)
        if "settle_claim" in msg:
            if self.state["status"] != "locked":
                raise a8.AcceptanceError("cannot settle claim in state")
            self.state = copy.deepcopy(self.settled_state)
            return {"txhash": "SETTLED", "code": 0, "height": "10"}
        role = msg["withdraw_usdt"]["role"]
        # Observe the actual file before each broadcast, not merely mock calls.
        phases = a8.load_object(self.path)["phases"]
        self.payout_checks.append(copy.deepcopy(phases))
        if role == self.blocked:
            raise a8.AcceptanceError("recipient blocked")
        amount = self.pending[role]
        if not amount:
            raise a8.AcceptanceError("already paid")
        self.balances[self.roles[role][0]] += amount
        self.balances["deal"] -= amount
        self.pending[role] = 0
        return {"txhash": role.upper(), "code": 0, "height": "11"}

    def tx_attempt(self, *args, **kwargs):
        message = json.loads(args[-1])
        if "withdraw_usdt" in message:
            assert message["withdraw_usdt"]["role"] == self.blocked
            log = "a8 injected cw20 transfer failure"
        else:
            assert "settle_claim" in message
            log = "cannot settle claim in state Releasing"
        return {"layer": "deliver_tx", "tx_hash": "REJECTED", "height": "12",
                "code": 5, "codespace": "wasm", "raw_log": log}


class WithdrawalEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "context.json"
        self.chain = PaymentChain(self.path)
        for name, kwargs in (("DockerGonka", {"return_value": self.chain}),
                             ("assert_chain", {}), ("assert_deal_terms", {})):
            patcher = patch.object(a8, name, **kwargs)
            patcher.start()
            self.addCleanup(patcher.stop)
        self.args = Namespace(context=str(self.path), reward_epoch=5, reward_seed=1,
                              host_node="host", fault_retry=False, cw20_fault_positions=None)

    def phases(self):
        return a8.load_object(self.path)["phases"]

    def test_partial_failure_persists_then_retry_after_completed_verifies_full_oracle(self):
        with self.assertRaisesRegex(a8.AcceptanceError, "pending for: fee"):
            a8.claim_settle(self.args)
        phases = self.phases()
        checkpoint = next(p for p in phases if p["name"] == "settlement_committed")
        self.assertEqual(checkpoint["settle_tx"]["tx_hash"], "SETTLED")
        self.assertEqual(checkpoint["before"]["cw20"]["deal"], 100000000)
        attempts = [p for p in phases if p["name"] == "usdt_withdrawal_attempt"]
        self.assertEqual([(p["role"], p["status"]) for p in attempts],
                         [("host", "confirmed"), ("fee", "failed"), ("buyer", "confirmed")])
        self.assertEqual(attempts[0]["tx"]["tx_hash"], "HOST")
        self.assertEqual(attempts[1]["error"], "recipient blocked")
        self.assertFalse(any(p["name"] == "settlement_delivery_verified" for p in phases))
        for index, snapshot in enumerate(self.chain.payout_checks):
            self.assertTrue(any(p["name"] == "settlement_committed" for p in snapshot))
            self.assertEqual(sum(p["name"] == "usdt_withdrawal_attempt" for p in snapshot), index)
        self.chain.blocked = None
        self.chain.state["status"] = "completed"
        a8.withdraw_usdt(Namespace(context=str(self.path)))
        self.assertEqual(self.chain.calls[-1], {"withdraw_usdt": {"role": "fee"}})
        self.assertEqual(sum("settle_claim" in call for call in self.chain.calls), 1)
        verified = next(p for p in self.phases() if p["name"] == "settlement_delivery_verified")
        self.assertEqual(verified["actual"]["cw20_deltas"], self.chain.expected["cw20_deltas"])
        self.assertEqual(verified["actual"]["deal_outflow"], 100000000)
        self.assertEqual(self.phases()[:len(phases)], phases)

    def test_failed_retry_is_saved_and_does_not_claim_verification(self):
        with self.assertRaises(a8.AcceptanceError):
            a8.claim_settle(self.args)
        with self.assertRaises(a8.AcceptanceError):
            a8.withdraw_usdt(Namespace(context=str(self.path)))
        self.assertEqual(self.phases()[-1]["role"], "fee")
        self.assertEqual(self.phases()[-1]["status"], "failed")
        self.assertFalse(any(p["name"] == "settlement_delivery_verified" for p in self.phases()))

    def test_query_failure_after_settlement_keeps_checkpoint(self):
        self.chain.query_failure = True
        with self.assertRaisesRegex(a8.AcceptanceError, "query unavailable"):
            a8.claim_settle(self.args)
        self.assertEqual(self.phases()[0]["settle_tx"]["tx_hash"], "SETTLED")
        self.assertEqual(len(self.chain.calls), 1)

    def test_disk_failure_stops_before_next_broadcast(self):
        original = a8.write_object
        def fail_on_attempt(path, value):
            if value["phases"][-1]["name"] == "usdt_withdrawal_attempt":
                raise OSError("disk full")
            original(path, value)
        with patch.object(a8, "write_object", side_effect=fail_on_attempt):
            with self.assertRaisesRegex(OSError, "disk full"):
                a8.claim_settle(self.args)
        self.assertEqual(self.chain.calls, [{"settle_claim": {}}, {"withdraw_usdt": {"role": "host"}}])
        self.assertEqual(self.phases()[0]["name"], "settlement_committed")

    def test_success_still_runs_original_oracle_and_repeat_check(self):
        self.chain.blocked = None
        a8.claim_settle(self.args)
        self.assertEqual(self.phases()[-1]["name"], "claim_settle")
        self.assertEqual(self.phases()[-1]["expected"], self.chain.expected)
        self.assertEqual(self.phases()[-1]["settle_repeat"]["proof"]["contract_error"], "InvalidSettlementState")

    def test_retry_cannot_pass_oracle_with_wrong_balance(self):
        with self.assertRaises(a8.AcceptanceError):
            a8.claim_settle(self.args)
        self.chain.blocked = None
        self.chain.balances["host"] -= 1
        with self.assertRaisesRegex(a8.AcceptanceError, "CW20 deltas differ"):
            a8.withdraw_usdt(Namespace(context=str(self.path)))
        self.assertEqual(self.phases()[-1]["status"], "confirmed")
        self.assertFalse(any(p["name"] == "settlement_delivery_verified" for p in self.phases()))

    def test_merged_fault_positions_target_withdrawals_after_settlement(self):
        self.args.cw20_fault_positions = [1, 2, 3]
        configured = []
        def configure(gonka, context, recipient):
            self.assertEqual(self.chain.state["status"], "releasing")
            configured.append(recipient)
            self.chain.blocked = next((role for role, (addr, _) in self.chain.roles.items()
                                       if addr == recipient), None)
            return {"txhash": "CONFIG", "code": 0, "height": "10"}
        with patch.object(a8, "configure_cw20_transfer_failure", side_effect=configure):
            a8.claim_settle(self.args)
        self.assertEqual(configured, ["host", None, "fees", None, "buyer", None])
        faults = self.phases()[-1]["cw20_fault_rollbacks"]
        self.assertEqual(len(faults), 3)
        for fault in faults:
            self.assertEqual(fault["before"], fault["after"])
            self.assertEqual(fault["before"]["state"]["status"], "releasing")
            self.assertEqual(fault["pending_before"]["fee"]["pending_micro_usdt"], "1200000")

    def test_named_scenario_preserves_evidence_and_retries_only_its_debt(self):
        context = a8.load_object(self.path)
        context["scenarios"] = {"example": copy.deepcopy(context)}
        a8.write_object(self.path, context)
        with self.assertRaisesRegex(a8.AcceptanceError, "pending for: fee"):
            a8.settle_scenario(Namespace(context=str(self.path), name="example"))
        partial = a8.load_object(self.path)["scenarios"]["example"]["phases"]
        self.assertEqual(partial[0]["name"], "settlement_committed")
        self.assertEqual([p["role"] for p in partial[1:]], ["host", "fee", "buyer"])
        self.chain.blocked = None
        a8.withdraw_usdt(Namespace(context=str(self.path), name="example"))
        saved = a8.load_object(self.path)
        self.assertEqual(saved["phases"], [])
        self.assertEqual(saved["scenarios"]["example"]["phases"][:len(partial)], partial)
        self.assertTrue(any(p["name"] == "settlement_delivery_verified"
                            for p in saved["scenarios"]["example"]["phases"]))


if __name__ == "__main__":
    unittest.main()
