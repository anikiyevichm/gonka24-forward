import copy
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import a8_acceptance as a8
import a8_query_faults as c


def context():
    return {"chain": {"chain_id": "gonka-mainnet"}, "scenarios": {
        n: {"contracts": {"deal": "deal-" + n, "cw20": "token"},
            "accounts": {"host": "host-" + n, "buyer": "buyer", "fee_recipient": "fee"},
            "terms": {"target_epoch": 5, "budget_micro_usdt": "100"}, "phases": []}
        for n in c.ALL_NAMES}}


def owned_nodes(root):
    return [{"Name": "/" + n + "-node", "Id": n, "State": {"Running": True, "StartedAt": "now"},
             "Config": {"Labels": {"com.docker.compose.project.working_dir": str(root),
                                    "com.docker.compose.service": "chain-node"},
                        "Cmd": ["priv_validator_state.json; exec cosmovisor run start"]},
             "Mounts": [{"Destination": "/root/.inference", "Source": str(root / "prod-local" / n),
                         "Type": "bind", "RW": True}]}
            for n in ("genesis", "join1", "join2")]


class PlanTests(unittest.TestCase):
    def test_all_scopes_and_recovery_are_independent(self):
        ctx = context()
        active = c.make_plan(ctx, "activate", 100)
        self.assertEqual(len(active["rules"]), len(c.ALL_NAMES) + 1)
        self.assertEqual(len({(r["deal"], r["route"]) for r in active["rules"]}), len(active["rules"]))
        early_recovery = c.make_plan(ctx, "recover-before", 200, active)
        ids = {r["id"].split(":")[-1] for r in early_recovery["rules"] if r["from_height"] <= 200 < r["until_height"]}
        self.assertNotIn("r5-recover", ids)
        self.assertIn("r5-cancel", ids)
        self.assertTrue(set(c.R4_NAMES) <= ids)
        self.assertTrue(set(c.ROUTING_NAMES).isdisjoint(ids))
        simultaneous = [r for r in active["rules"] if r["deal"] == "deal-r3-epoch-refund"]
        self.assertEqual({r["route"] for r in simultaneous}, {c.EPOCH, c.SUMMARY})
        recovered = c.make_plan(ctx, "recover-after", 300, early_recovery)
        self.assertTrue(all(r["from_height"] < r["until_height"] <= 300 for r in recovered["rules"]))
        # A lagging validator must replay every earlier block with the same
        # faults after both restarts, including a tx in the last stopped block.
        def behavior(plan, h):
            return {(r["deal"], r["route"], r["kind"]) for r in plan["rules"] if r["from_height"] <= h < r["until_height"]}
        for h in (1, 99, 100, 198, 199):
            self.assertEqual(behavior(active, h), behavior(early_recovery, h))
        for h in (100, 199, 200, 299):
            self.assertEqual(behavior(early_recovery, h), behavior(recovered, h))
        self.assertFalse(behavior(recovered, 300))

    def test_ownership_rejects_other_run_mount_missing_node_and_unsafe_restart(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td) / "a8-runtime/run/gonka"
            nodes = owned_nodes(root)
            c.validate_owned_nodes(nodes, root)
            for mutate in (
                lambda ns: ns.pop(),
                lambda ns: ns[0]["Mounts"][0].update(Source=str(root.parent / "other")),
                lambda ns: ns[0]["Config"]["Labels"].update({"com.docker.compose.project.working_dir": str(root.parent)}),
                lambda ns: ns[0]["Config"].update(Cmd=["sh", "init-docker-genesis.sh"]),
                lambda ns: ns[0]["Mounts"][0].update(RW=False),
            ):
                with self.subTest(mutate=mutate):
                    modified = copy.deepcopy(nodes)
                    mutate(modified)
                    with self.assertRaises(ValueError):
                        c.validate_owned_nodes(modified, root)


class ControllerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / "live-context.json"
        a8.write_object(self.path, context())
        self.g = Mock()
        self.g.key_address.return_value = "independent-caller"
        self.g.bank_balance.return_value = 10
        with patch.object(a8, "DockerGonka", return_value=self.g):
            self.ctrl = c.Controller(SimpleNamespace(context=str(self.path), phase="late", proposal_id="1"), a8)
        self.snap = {"state": {"status": "locked", "recipient_locked": True,
                               "buyer_refund_usdt": "0", "gnk_release_policy": "unset", "host_net_usdt": "0"},
                     "cw20": dict(buyer=10, deal=100, host=0, fee_recipient=0),
                     "foreign_cw20": {"deal": 9}, "config": {"host": "host"},
                     "native": {r: {"portfolio": 50} for r in ("deal", "host", "buyer", "fee_recipient")}}
        self.epoch = patch.object(a8, "epoch_observation", return_value={"epoch": 8, "height": 100})
        self.epoch.start()
        self.addCleanup(self.epoch.stop)
        self.tx = dict(layer="deliver_tx", tx_hash="A" * 64, height="100", code=5,
                       codespace="wasm", raw_log=c.fault_marker("handler_error", c.SUMMARY), events=[])
        self.g.tx_attempt.return_value = self.tx
        self.ctrl.snapshot = Mock(side_effect=lambda _: copy.deepcopy(self.snap))

    def test_snapshot_retries_only_epoch_crossing_and_discards_entire_read(self):
        financial = {"bank_ngonka": {r: 10 for r in ("deal", "host", "buyer", "fee_recipient")}}
        self.g.smart.return_value = {}
        self.g.query_json.return_value = {"total_amount": None}
        with patch.object(a8, "scenario_financial_snapshot", side_effect=lambda *args: copy.deepcopy(financial)) as reads, patch.object(
            a8, "epoch_observation", side_effect=[
                {"epoch": 7, "height": 539}, {"epoch": 8, "height": 540},
                {"epoch": 8, "height": 541}, {"epoch": 8, "height": 542}]):
            result = c.Controller.snapshot(self.ctrl, "r5-cancel")
        self.assertEqual(reads.call_count, 2)
        self.assertEqual(result["observation"]["height"], 541)
        self.assertEqual(len(self.ctrl.results["discarded_snapshots"]), 1)
        self.g.tx_attempt.assert_not_called()

    def test_snapshot_retry_is_bounded_and_does_not_hide_query_errors(self):
        financial = {"bank_ngonka": {r: 10 for r in ("deal", "host", "buyer", "fee_recipient")}}
        self.g.smart.return_value = {}
        self.g.query_json.return_value = {"total_amount": None}
        with patch.object(a8, "scenario_financial_snapshot", side_effect=lambda *args: copy.deepcopy(financial)) as reads, patch.object(
            a8, "epoch_observation", side_effect=[{"epoch": n, "height": 100+n} for n in range(6)]):
            with self.assertRaisesRegex(a8.AcceptanceError, "all 3 reads"):
                c.Controller.snapshot(self.ctrl, "r5-cancel")
        self.assertEqual(reads.call_count, 3)
        with patch.object(a8, "scenario_financial_snapshot", side_effect=a8.AcceptanceError("bad query")) as reads:
            with self.assertRaisesRegex(a8.AcceptanceError, "bad query"):
                c.Controller.snapshot(self.ctrl, "r5-cancel")
        self.assertEqual(reads.call_count, 1)
        self.g.tx_attempt.assert_not_called()

    def test_effective_epoch_waits_for_contract_epoch_and_keeps_diagnostics(self):
        with patch.object(a8, "epoch_observation", side_effect=[
            {"height": 525, "epoch": 7}, {"height": 540, "epoch": 8}
        ]), patch.object(c.time, "sleep") as sleep:
            self.ctrl.wait_for_effective_epoch(3)
        sleep.assert_called_once_with(2)
        self.assertEqual(self.ctrl.results["epoch_waits"][-1]["observations"][-1]["epoch"], 8)
        self.g.tx_attempt.assert_not_called()

    def test_effective_epoch_rejects_missed_window_and_timeout(self):
        for epoch, message in ((9, "missed exact"), (7, "wait expired")):
            with patch.object(a8, "epoch_observation", return_value={"height": 525, "epoch": epoch}):
                with self.assertRaisesRegex(a8.AcceptanceError, message):
                    self.ctrl.wait_for_effective_epoch(3, timeout_seconds=0)
        self.g.tx_attempt.assert_not_called()

    def test_exact_error_and_included_transaction_are_mandatory(self):
        self.ctrl.attempt("r5-cancel", "refund", c.fault_marker("handler_error", c.SUMMARY), 3)
        for change in (dict(layer="check_tx"), dict(height="0"), dict(tx_hash=""),
                       dict(code=0), dict(codespace="sdk"), dict(raw_log="out of gas")):
            with self.subTest(change=change):
                self.g.tx_attempt.return_value = dict(self.tx, **change)
                with self.assertRaises(a8.AcceptanceError):
                    self.ctrl.attempt("r5-cancel", "refund", c.fault_marker("handler_error", c.SUMMARY), 3)
        self.assertGreaterEqual(len(list(self.ctrl.out.glob("tx-*.json"))), 7)

    def test_failed_call_rejects_state_money_foreign_and_native_deltas(self):
        for field, changed in (("state", {"status": "refunded"}), ("cw20", {"buyer": 110}),
                               ("foreign_cw20", {"deal": 0}), ("config", {"host": "other"}),
                               ("native", {"deal": {"portfolio": 51}})):
            with self.subTest(field=field):
                after = copy.deepcopy(self.snap)
                after[field].update(changed)
                self.ctrl.snapshot = Mock(side_effect=[copy.deepcopy(self.snap), after])
                with self.assertRaises(a8.AcceptanceError):
                    self.ctrl.attempt("r5-cancel", "refund", c.fault_marker("handler_error", c.SUMMARY))

    def test_emergency_refund_exact_distribution_and_accounting(self):
        self.g.tx_attempt.return_value = dict(self.tx, code=0, raw_log="", codespace="")
        good = copy.deepcopy(self.snap)
        good["state"].update(status="refunded", refund_reason="network_unconfirmed",
                             gnk_release_policy="host_only", buyer_refund_usdt="100")
        good["cw20"].update(buyer=110, deal=0)
        self.ctrl.snapshot = Mock(side_effect=[copy.deepcopy(self.snap), good])
        self.ctrl.refund("r5-cancel")
        for change in (lambda s: s["cw20"].update(host=100, buyer=10),
                       lambda s: s["state"].update(host_net_usdt="100"),
                       lambda s: s["state"].update(gnk_release_policy="unset")):
            bad = copy.deepcopy(good)
            change(bad)
            self.ctrl.snapshot = Mock(side_effect=[copy.deepcopy(self.snap), bad])
            with self.assertRaises(a8.AcceptanceError):
                self.ctrl.refund("r5-cancel")

    def test_failure_does_not_erase_other_cases_or_run_dependents(self):
        with patch.object(a8, "assert_chain"):
            self.ctrl.call("bad", Mock(side_effect=a8.AcceptanceError("semantic failure")))
            dependency = Mock()
            self.ctrl.call("blocked", dependency, ("bad",))
            self.ctrl.call("independent", lambda: {"tx": "real-proof"})
            dependency.assert_not_called()
            statuses = {k: v["status"] for k, v in self.ctrl.results["cases"].items()}
            self.assertEqual(statuses, dict(bad="FAIL", blocked="NOT_RUN", independent="PASS"))

    def test_phase_cannot_run_under_wrong_fault_plan(self):
        with self.assertRaisesRegex(a8.AcceptanceError, "activation history"):
            self.ctrl.run()

    def test_r5_requires_positive_claim_receipt_and_exact_native_credit(self):
        ctx = context()
        summary = {"epochPerformanceSummary": {"claimed": True, "earned_coins": "20", "rewarded_coins": "30"}}
        claim = {"name": "native_claim", "summary": summary,
                 "tx": {"code": 0, "height": "90", "tx_hash": "F" * 64},
                 "before": {"deal_bank": 0, "vesting": {}},
                 "after": {"deal_bank": 10, "vesting": {"total_amount": [{"denom": "ngonka", "amount": "40"}]}}}
        ctx["scenarios"]["r5-cancel"]["phases"] = [claim]
        a8.write_object(self.path, ctx)
        self.g.query_json.return_value = summary
        self.ctrl.native_claim("r5-cancel")
        for invalid in (lambda p: p["tx"].update(code=5), lambda p: p["after"].update(deal_bank=11)):
            bad = copy.deepcopy(ctx)
            invalid(bad["scenarios"]["r5-cancel"]["phases"][0])
            a8.write_object(self.path, bad)
            with self.assertRaises(a8.AcceptanceError):
                self.ctrl.native_claim("r5-cancel")

    def test_infrastructure_failure_aborts_package(self):
        with patch.object(a8, "assert_chain", side_effect=a8.AcceptanceError("node down")):
            with self.assertRaisesRegex(a8.AcceptanceError, "node down"):
                self.ctrl.call("case", lambda: None)
        self.assertNotIn("case", self.ctrl.results["cases"])

    def test_bank_retry_finalizes_before_return_without_another_process(self):
        self.ctrl.args.phase = "bank-retry"
        self.ctrl.results["activations"] = [{"stage": stage, "status": "INSTALLED"}
            for stage in ("activate", "recover-before", "recover-after")]
        self.ctrl.results["cases"]["r7.2-fault"] = {"status": "PASS"}
        self.ctrl.helper = Mock(return_value={"verified": True})
        def verify_saved_row():
            saved = a8.load_object(self.ctrl.out / "results.json")
            self.assertEqual(saved["cases"]["r7.2-retry"]["status"], "PASS")
        self.ctrl.finalize_report = Mock(side_effect=verify_saved_row)
        with patch.object(a8, "assert_chain"):
            self.ctrl.run()
        self.ctrl.finalize_report.assert_called_once()

    def test_incomplete_report_never_passes(self):
        self.ctrl.args.phase = "report"
        with self.assertRaisesRegex(a8.AcceptanceError, "incomplete"):
            self.ctrl.run()
        self.assertEqual(self.ctrl.results["status"], "PARTIAL")

    def test_activation_stops_every_validator_before_copy_and_never_restarts_after_copy_failure(self):
        root = Path(self.temp.name) / "a8-runtime/run/gonka"
        nodes = owned_nodes(root)
        binary = Path(self.temp.name) / "inferenced"
        binary.write_bytes(b"test binary")
        digest = a8.sha256_file(binary)
        a8.write_object(self.path.parent / "c-runtime-build.json", {
            "binary_sha256": digest, "gonka_source_sha": "sha", "build_tag": "a8faults"})
        calls = []
        state = {"running": True, "installed": False}
        fail_copy = [False]

        def docker(*args):
            calls.append(args)
            if args[0] == "ps":
                return "genesis\njoin1\njoin2"
            if args[0] == "inspect":
                current = copy.deepcopy(nodes)
                for node in current:
                    node["State"]["Running"] = state["running"]
                return json.dumps([n for n in current if n["Id"] in args[1:]])
            if args[0] == "stop":
                self.assertEqual(set(args[3:]), {"genesis", "join1", "join2"})
                state["running"] = False
            elif args[0] == "cp":
                self.assertFalse(state["running"])
                if fail_copy[0]:
                    raise a8.AcceptanceError("copy failed")
                if args[1].endswith("priv_validator_state.json"):
                    a8.write_object(Path(args[2]), {"height": "100"})
                elif args[2].endswith("original-inferenced"):
                    Path(args[2]).write_bytes(b"old")
                elif args[2].endswith("verified-inferenced"):
                    Path(args[2]).write_bytes(binary.read_bytes())
                elif ":/" in args[1] and args[1].endswith("a8-query-faults.json"):
                    Path(args[2]).write_bytes((self.ctrl.out / "plan-activate.json").read_bytes())
            elif args[0] == "start":
                state.update(running=True, installed=True)
            elif args[0] == "exec":
                if args[2] == "readlink":
                    return "/root/.inference/cosmovisor/genesis/bin/inferenced"
                if args[2] == "sha256sum":
                    if args[-1].endswith("a8-query-faults.json"):
                        return a8.sha256_file(self.ctrl.out / "plan-activate.json") + " file"
                    return (digest if state["installed"] else hashlib.sha256(b"old").hexdigest()) + " file"
            elif args[0] == "logs":
                return "A8 TEST-ONLY QUERY FAULTS plan_sha256=" + a8.sha256_file(self.ctrl.out / "plan-activate.json")
            return ""

        self.ctrl.docker = docker
        self.ctrl.node_height = Mock(side_effect=lambda _: 108 if state["installed"] else 100)
        self.ctrl.node_observation = Mock(return_value={"height": 108, "catching_up": False})
        with patch.dict("os.environ", A8_GONKA_DIR=str(root), A8_C_BINARY=str(binary)), patch.object(
            subprocess, "run", return_value=SimpleNamespace(stdout="sha\n")):
            self.ctrl.activate("activate")
            self.assertEqual(self.ctrl.results["activations"][-1]["status"], "INSTALLED")
            self.assertEqual(sum(cmd[0] == "cp" for cmd in calls), 16)
            state["installed"] = True
            fail_copy[0] = True
            calls.clear()
            with self.assertRaisesRegex(a8.AcceptanceError, "copy failed"):
                self.ctrl.activate("recover-before")
            self.assertFalse(any(cmd[0] == "start" for cmd in calls))

    def test_readiness_waits_for_sync_and_height_on_every_node(self):
        record = {"nodes": [{"id": str(i)} for i in range(3)]}
        self.ctrl.node_observation = Mock(side_effect=[
            {"height": 108, "catching_up": True}, {"height": 107, "catching_up": False},
            {"height": 108, "catching_up": False},
            *[{"height": 109, "catching_up": False} for _ in range(3)]])
        with patch.object(c.time, "sleep") as sleep, patch.object(c.time, "monotonic", return_value=0):
            self.assertEqual(self.ctrl.wait_for_nodes(record, 108, 10), [109]*3)
            sleep.assert_called_once_with(2)
        self.assertEqual(len(record["readiness_observations"]), 2)

    def test_readiness_transport_retry_is_bounded_and_bad_schema_aborts(self):
        for error, retry in [(a8.AcceptanceError("connection refused"), True),
                             (a8.AcceptanceError("missing catching_up field"), False)]:
            record = {"nodes": [{"id": "node"}]}
            self.ctrl.node_observation = Mock(side_effect=[error, {"height": 108, "catching_up": False}])
            with patch.object(c.time, "sleep") as sleep, patch.object(c.time, "monotonic", return_value=0):
                if retry:
                    self.assertEqual(self.ctrl.wait_for_nodes(record, 108, 10), [108])
                    sleep.assert_called_once()
                else:
                    with self.assertRaises(a8.AcceptanceError): self.ctrl.wait_for_nodes(record, 108, 10)
                    sleep.assert_not_called()
            self.assertIn("error", record["readiness_observations"][0][0])
        record = {"nodes": [{"id": "node"}]}
        self.ctrl.node_observation = Mock(return_value={"height": 108, "catching_up": True})
        with patch.object(c.time, "monotonic", return_value=11), self.assertRaisesRegex(a8.AcceptanceError, "deadline expired"):
            self.ctrl.wait_for_nodes(record, 108, 10)
        self.assertTrue(record["readiness_observations"][0][0]["catching_up"])

    def test_native_status_missing_flag_is_not_treated_as_ready(self):
        self.ctrl.docker = Mock(return_value=json.dumps({"sync_info": {"latest_block_height": "108"}}))
        with self.assertRaisesRegex(a8.AcceptanceError, "lacks authoritative"):
            self.ctrl.node_observation("node")
        for value in (None, [], {"sync_info": None}):
            self.ctrl.docker = Mock(return_value=json.dumps(value))
            with self.assertRaisesRegex(a8.AcceptanceError, "not an object"):
                self.ctrl.node_observation("node")

    def test_r5_early_batch_is_separate_from_r4_after_recovery(self):
        self.ctrl.call = Mock()
        self.ctrl.results["activations"] = [{"stage": "activate", "status": "INSTALLED"}]
        self.ctrl.wait_for_effective_epoch = Mock()
        self.ctrl.args.phase = "early-r5"
        self.ctrl.run()
        names = [call.args[0] for call in self.ctrl.call.call_args_list]
        self.assertEqual(len(names), 6)
        self.assertTrue(all(n.startswith("r5-") for n in names))
        self.ctrl.call.reset_mock()
        self.ctrl.results["activations"].append({"stage": "recover-before", "status": "INSTALLED"})
        self.ctrl.args.phase = "early-r4"
        self.ctrl.run()
        names = [call.args[0] for call in self.ctrl.call.call_args_list]
        self.assertEqual(len(names), 16)
        self.assertTrue(all(n.startswith("r4-") for n in names))


if __name__ == "__main__":
    unittest.main()
