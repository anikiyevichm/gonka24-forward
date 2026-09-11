"""Finite package-C live controller. No Docker work occurs on import.

The API module is injected to preserve AcceptanceError identity when the main
harness is executed as __main__. Plans change only while ALL owned validators
are stopped. No chain storage is edited and no resource is deleted here.
"""
from __future__ import annotations

import copy
import json
import os
from pathlib import Path
import subprocess
import time
from types import SimpleNamespace

SUMMARY = "/inference.inference.Query/EpochPerformanceSummaryByParticipant"
ROUTING = "/inference.inference.Query/ListClaimRecipients"
EPOCH = "/inference.inference.Query/GetCurrentEpoch"
KINDS = ("handler_error", "malformed_protobuf", "oversized_response",
         "missing_nested_summary", "wrong_host", "wrong_epoch",
         "invalid_participant_address", "unsupported_request")
ROUTING_KINDS = ("handler_error", "malformed_protobuf", "duplicate_routing")
R4_NAMES = tuple("r4-" + k for k in KINDS)
ROUTING_NAMES = tuple(f"r3-{k}-{op}" for k in ROUTING_KINDS for op in ("lock", "refund"))
INACTIVE_NAMES = R4_NAMES + ROUTING_NAMES + ("r3-epoch-lock", "r3-epoch-refund")
ALL_NAMES = INACTIVE_NAMES + ("r5-recover", "r5-cancel")
PHASES = ("prepare", "activate", "routing", "early-r5", "recover-before", "early-r4",
          "late", "recover-after", "terminal", "bank-fault", "bank-retry", "report")


def fault_marker(kind, route):
    return {
        "handler_error": f"Gonka query failed for {route}",
        "malformed_protobuf": f"failed to decode protobuf response for {route}",
        "oversized_response": f"Gonka response for {route} is too large: 32769 bytes",
        "missing_nested_summary": f"Gonka response for {route} is missing required field",
        "wrong_host": f"Gonka response for {route} has mismatched participant_id",
        "wrong_epoch": f"Gonka response for {route} has mismatched epoch_index",
        "invalid_participant_address": f"Gonka response for {route} contains an invalid participant_id address",
        "unsupported_request": f"Gonka does not support the required query route {route}",
        "duplicate_routing": "claim recipient is configured more than once for epoch",
    }[kind]


def make_plan(context, stage, height, previous=None):
    """Schedule a future boundary, preserving old behavior during block replay."""
    if stage != "activate" and previous is None:
        raise ValueError("recovery requires the exact previous immutable plan")
    rules = []
    for name in ALL_NAMES:
        s = context["scenarios"][name]
        if stage == "recover-before" and (name == "r5-recover" or name in ROUTING_NAMES):
            continue
        kind = name[3:] if name in R4_NAMES else "handler_error"
        route = SUMMARY
        if name in ROUTING_NAMES:
            kind = name[3:].rsplit("-", 1)[0]
            route = ROUTING
        elif name.startswith("r3-epoch-"):
            route = EPOCH
        rule = dict(id=name if stage == "activate" else stage + ":" + name, deal=s["contracts"]["deal"], route=route,
                    host=s["accounts"]["host"], epoch=int(s["terms"]["target_epoch"]),
                    from_height=height, until_height=height + 5000, kind=kind)
        if kind == "wrong_host":
            rule["other_host"] = s["accounts"]["buyer"]
        rules.append(rule)
        if name == "r3-epoch-refund":
            rules.append(dict(rule, id=rule["id"] + "-summary", route=SUMMARY))
    history = [dict(r, until_height=min(r["until_height"], height)) for r in (previous or {}).get("rules", [])]
    if any(r["until_height"] <= r["from_height"] for r in history):
        raise ValueError("C transition must follow every previous activation boundary")
    if stage == "recover-after":
        # Keep historic behavior, but schedule no fault at/after the new height.
        rules = []
    return dict(version=1, chain_id=context["chain"]["chain_id"], rules=history + rules)


def validate_owned_nodes(items, root):
    root = Path(root).resolve()
    if root.parent.parent.name != "a8-runtime" or root.name != "gonka":
        raise ValueError("C activation requires an immutable a8-runtime/<id>/gonka snapshot")
    expected = {name + "-node": name for name in ("genesis", "join1", "join2")}
    if {i["Name"].lstrip("/") for i in items} != set(expected) or len(items) != 3:
        raise ValueError("C requires exactly the three owned validator containers")
    for item in items:
        labels = item["Config"].get("Labels") or {}
        name = item["Name"].lstrip("/")
        if Path(labels.get("com.docker.compose.project.working_dir", "")).resolve() != root:
            raise ValueError("validator compose working_dir does not belong to this snapshot")
        if labels.get("com.docker.compose.service") != "chain-node":
            raise ValueError("unexpected validator compose service")
        mounts = [m for m in item["Mounts"] if m["Destination"] == "/root/.inference"]
        if len(mounts) != 1 or mounts[0]["Type"] != "bind" or not mounts[0].get("RW"):
            raise ValueError("validator home must be one writable owned bind")
        if Path(mounts[0]["Source"]).resolve() != root / "prod-local" / expected[name]:
            raise ValueError("validator home is outside its exact snapshot path")
        command = " ".join(item["Config"].get("Cmd") or [])
        if "priv_validator_state.json" not in command or "exec cosmovisor run start" not in command:
            raise ValueError("C restart overlay is missing; refuse genesis reinitialization")


class Controller:
    def __init__(self, args, api):
        self.a, self.args = api, args
        self.path = Path(args.context).resolve()
        self.context = api.load_object(self.path)
        self.g = api.DockerGonka(api.Runner(), self.context["chain"]["chain_id"])
        self.out = self.path.parent / "package-c"
        self.out.mkdir(exist_ok=True)
        self.results = api.load_object(self.out / "results.json") if (self.out / "results.json").exists() else {
            "level": "NATIVE-FAULT", "cases": {}, "activations": [],
            "excluded": {"R4.4-InvalidResponse": "VM-boundary NOT RUN",
                         "R4.5": "VM-boundary NOT RUN", "R4.6": "contract-test only"}}

    def save(self):
        self.a.write_object(self.out / "results.json", self.results)

    def call(self, name, function, dependencies=()):
        self.a.assert_chain(self.g)  # infrastructure failure aborts the package
        if name in self.results["cases"]:
            raise self.a.AcceptanceError(f"refuse duplicate C case {name}")
        if any(self.results["cases"].get(d, {}).get("status") != "PASS" for d in dependencies):
            self.results["cases"][name] = {"status": "NOT_RUN", "dependencies": list(dependencies)}
        else:
            try:
                value = function()
                self.results["cases"][name] = {"status": "PASS", "evidence": value}
            except self.a.AcceptanceError as error:
                self.results["cases"][name] = {"status": "FAIL", "error": str(error)}
                self.save()
                self.a.assert_chain(self.g)
        self.save()

    def scenario(self, name):
        self.context = self.a.load_object(self.path)
        return self.a.scenario_record(self.context, name)

    def snapshot(self, name):
        # Retry reads only, never the execute transaction. Keep every rejected
        # candidate so an epoch boundary cannot silently weaken the proof.
        for attempt in range(3):
            s = self.scenario(name)
            observation = self.a.epoch_observation(self.g)
            snap = self.a.scenario_financial_snapshot(self.g, self.context, s)
            snap["config"] = self.g.smart(s["contracts"]["deal"], {"config": {}})
            snap["native"] = {}
            for role in ("deal", "host", "buyer", "fee_recipient"):
                addr = s["contracts"]["deal"] if role == "deal" else s["accounts"][role]
                total = self.g.query_json("streamvesting", "total-vesting", addr)
                schedule = self.g.query_json("streamvesting", "vesting-schedule", addr)
                snap["native"][role] = {"total": total, "schedule": schedule,
                    "portfolio": snap["bank_ngonka"][role] + self.a.native_coin_amounts(total, "total_amount").get("ngonka", 0)}
            end = self.a.epoch_observation(self.g)
            if end["epoch"] == observation["epoch"]:
                snap["observation"] = observation
                snap["observation_end"] = end
                return snap
            self.results.setdefault("discarded_snapshots", []).append({
                "scenario": name, "attempt": attempt + 1,
                "before": observation, "after": end, "snapshot": snap,
                "reason": "vesting_epoch_boundary",
            })
            self.save()
        raise self.a.AcceptanceError("snapshot crossed a vesting epoch in all 3 reads; no coherent balance proof")

    def unchanged(self, before, after, tx):
        for field in ("state", "cw20", "foreign_cw20", "config"):
            if before.get(field) != after.get(field):
                raise self.a.AcceptanceError(f"C transaction changed {field}")
        for role in before["native"]:
            if before["native"][role]["portfolio"] != after["native"][role]["portfolio"]:
                raise self.a.AcceptanceError(f"C transaction changed native portfolio of {role}")
        self.a.assert_no_bank_transfer_involving(tx, self._deal)

    def attempt(self, name, op, marker=None, offset=None):
        s = self.scenario(name)
        self._deal = s["contracts"]["deal"]
        before = self.snapshot(name)
        epoch = self.a.epoch_observation(self.g)
        if offset is not None and epoch["epoch"] != int(s["terms"]["target_epoch"]) + offset:
            raise self.a.AcceptanceError(f"{name}/{op} requires exact E+{offset}: {epoch}")
        caller = self.g.key_address("genesis-node", "genesis")
        if caller in {s["accounts"][r] for r in ("host", "buyer", "fee_recipient")} | {self._deal}:
            raise self.a.AcceptanceError("C caller must be independent of every financial role")
        fee_before = self.g.bank_balance(caller)
        tx = self.g.tx_attempt("genesis-node", "genesis", "wasm", "execute", self._deal,
                               self.a.compact_json({op: {}}), gas="2000000")
        # Write the raw attempt BEFORE assertions, so failures remain diagnosable.
        raw_path = self.out / f"{name}-{op}-{len(list(self.out.glob('tx-*.json'))):04d}.json"
        raw_path = raw_path.with_name("tx-" + raw_path.name)
        proof = {"transaction": tx, "before": before, "caller": caller,
                 "caller_fee_delta": self.g.bank_balance(caller) - fee_before}
        self.a.write_object(raw_path, proof)
        proof["epoch_bracket"] = self.a.assert_tx_epoch_bracket(tx, epoch, self.a.epoch_observation(self.g))
        after = self.snapshot(name)
        proof["after"] = after
        self.a.write_object(raw_path, proof)
        row = self.a.unwrap_tx(tx)
        if row.get("layer") != "deliver_tx" or not row.get("tx_hash") or int(row.get("height", 0)) <= 0:
            raise self.a.AcceptanceError("C requires an included DeliverTx with a hash")
        if marker is None:
            if int(row["code"]) != 0:
                raise self.a.AcceptanceError(f"C expected success: {row}")
            if op == "lock":
                if before["state"].get("status") != "funded":
                    raise self.a.AcceptanceError("C recovered Lock requires Funded")
                expected = dict(before["state"], status="locked", recipient_locked=True)
                if after["state"] != expected:
                    raise self.a.AcceptanceError("C recovered Lock changed unrelated accounting")
                self.unchanged(dict(before, state=expected), after, tx)
        else:
            if int(row["code"]) == 0 or row.get("codespace") != "wasm" or marker not in row.get("raw_log", ""):
                raise self.a.AcceptanceError(f"C wrong failure, expected {marker}: {row}")
            self.unchanged(before, after, tx)
        return {"file": str(raw_path), "sha256": self.a.sha256_file(raw_path), **proof}

    def native_claim(self, name):
        s = self.scenario(name)
        claims = [p for p in s["phases"] if p["name"] == "native_claim"]
        if len(claims) != 1:
            raise self.a.AcceptanceError("R5 requires one real claim receipt")
        original = claims[0]["summary"]
        current = self.g.query_json("inference", "show-epoch-performance-summary-by-participant",
                                    str(s["terms"]["target_epoch"]), s["accounts"]["host"])
        summary = current["epochPerformanceSummary"]
        if current != original or summary.get("claimed") is not True:
            raise self.a.AcceptanceError("R5 native claimed ledger was not preserved")
        tx = claims[0]["tx"]
        if tx.get("code") != 0 or not tx.get("tx_hash") or int(tx.get("height", 0)) <= 0:
            raise self.a.AcceptanceError("R5 requires an included successful native claim")
        native_total = sum(self.a.require_uint(summary.get(f, 0), f) for f in ("earned_coins", "rewarded_coins"))
        received = claims[0]["after"]["deal_bank"] + self.a.native_coin_amounts(claims[0]["after"]["vesting"], "total_amount").get("ngonka", 0)
        previous = claims[0]["before"]["deal_bank"] + self.a.native_coin_amounts(claims[0]["before"]["vesting"], "total_amount").get("ngonka", 0)
        if native_total <= 0 or received - previous != native_total:
            raise self.a.AcceptanceError("R5 native credit must equal its positive authoritative claim amount")
        return {"original": claims[0], "recovered": current}

    def refund(self, name, exact_epoch=True):
        target = int(self.scenario(name)["terms"]["target_epoch"])
        if self.a.epoch_observation(self.g)["epoch"] < target + 3:
            raise self.a.AcceptanceError("C emergency refund requires at least E+3")
        proof = self.attempt(name, "refund", offset=3 if exact_epoch else None)
        before, after = proof["before"], proof["after"]
        s = self.scenario(name)
        budget = int(s["terms"]["budget_micro_usdt"])
        state = after["state"]
        if state.get("status") != "refunded" or state.get("refund_reason") != "network_unconfirmed" or self.a.normalize_release_policy(state.get("gnk_release_policy")) != "host_only":
            raise self.a.AcceptanceError("C emergency refund did not freeze Refunded/NetworkUnconfirmed/HostOnly")
        expected_state = dict(before["state"], status="refunded", refund_reason="network_unconfirmed",
                              gnk_release_policy=state["gnk_release_policy"], buyer_refund_usdt=str(budget))
        if before["state"].get("status") != "locked" or state != expected_state:
            raise self.a.AcceptanceError("C emergency refund changed unrelated accounting or shares")
        expected = dict(buyer=budget, deal=-budget, host=0, fee_recipient=0)
        if {r: after["cw20"][r] - before["cw20"][r] for r in expected} != expected or after["cw20"]["deal"] != 0:
            raise self.a.AcceptanceError("C emergency refund violates exact deposit conservation")
        # Only the intended CW20/accounting transition is allowed.
        adjusted = dict(before, state=state, cw20=after["cw20"])
        self._deal = s["contracts"]["deal"]
        self.unchanged(adjusted, after, proof["transaction"])
        return proof

    def settle_recovered(self):
        name = "r5-recover"
        ledger = self.native_claim(name)
        proof = self.attempt(name, "settle_claim", offset=3)
        before = proof["before"]
        scenario = self.scenario(name)
        deal = scenario["contracts"]["deal"]
        self.a.record_settlement_phase(self.path, {
            "name": "settlement_committed", "deal": deal,
            "settle_tx": proof["transaction"], "summary": ledger["recovered"],
            "before": before, "addresses": {**scenario["accounts"], "deal": deal},
        }, name)
        payments = self.g.smart(deal, {"usdt_payments": {}})
        proof["withdrawal_txs"] = self.a.recorded_withdrawals(self.g, self.path, deal, payments, name)
        self.a.verify_recorded_settlement(self.g, self.path, name)
        after = self.snapshot(name)
        proof["after"] = after
        deltas = {r: after["cw20"][r] - before["cw20"][r] for r in ("host", "buyer", "fee_recipient")}
        proof["oracle"] = self.a.assert_claim_settlement_matches_oracle(
            self.scenario(name), ledger["recovered"], after["state"], deltas,
            before["cw20"]["deal"] - after["cw20"]["deal"])
        self.unchanged(dict(before, state=after["state"], cw20=after["cw20"]), after, proof["transaction"])
        proof["native_ledger"] = self.native_claim(name)
        return proof

    def docker(self, *args):
        p = subprocess.run(["docker", *args], capture_output=True, text=True, timeout=120, check=False)
        if p.returncode:
            raise self.a.AcceptanceError(f"Docker C operation {args[:2]} failed: {p.stderr}")
        return (p.stdout + p.stderr).strip() if args[0] == "logs" else p.stdout.strip()

    def activate(self, stage):
        root = Path(os.environ["A8_GONKA_DIR"]).resolve()
        binary = Path(os.environ["A8_C_BINARY"]).resolve()
        build = self.a.load_object(self.path.parent / "c-runtime-build.json")
        source = subprocess.run(["git", "-C", str(root), "rev-parse", "HEAD"],
                                capture_output=True, text=True, check=True, timeout=30).stdout.strip()
        if source != build["gonka_source_sha"] or build.get("build_tag") != "a8faults":
            raise self.a.AcceptanceError("C binary source/tag does not match the runtime snapshot")
        if self.a.sha256_file(binary) != build["binary_sha256"]:
            raise self.a.AcceptanceError("C tagged binary differs from its build provenance")
        ids = self.docker("ps", "-aq", "--filter", "label=com.docker.compose.service=chain-node").splitlines()
        if len(ids) != 3:
            raise self.a.AcceptanceError("C activation refuses missing or extra chain-node containers")
        items = json.loads(self.docker("inspect", *ids))
        validate_owned_nodes(items, root)
        if any(not i["State"]["Running"] for i in items):
            raise self.a.AcceptanceError("all three validators must be healthy before activation")
        heights = [self.node_height(i["Id"]) for i in items]
        height = max(heights)
        activation_height = height + 8
        previous = self.results["activations"][-1]["plan"] if self.results["activations"] else None
        plan = make_plan(self.context, stage, activation_height, previous)
        plan_path = self.out / f"plan-{stage}.json"
        if plan_path.exists():
            raise self.a.AcceptanceError("refuse reusing a C activation stage")
        self.a.write_object(plan_path, plan)
        digest = self.a.sha256_file(plan_path)
        record = {"stage": stage, "plan": plan, "plan_sha256": digest,
                  "binary": build, "before_height": height, "activation_height": activation_height,
                  "nodes": [], "status": "INSTALLING"}
        self.results["activations"].append(record)
        self.save()
        for item in items:
            ident = item["Id"]
            dest = self.docker("exec", ident, "readlink", "-f", "/root/.inference/cosmovisor/current/bin/inferenced")
            if not dest.startswith("/root/.inference/cosmovisor/") or not dest.endswith("/bin/inferenced") or ".." in dest:
                raise self.a.AcceptanceError("unsafe cosmovisor binary destination")
            old = self.docker("exec", ident, "sha256sum", dest).split()[0]
            record["nodes"].append({"id": ident, "name": item["Name"], "destination": dest, "old_binary_sha256": old})
        self.save()
        if len({node["old_binary_sha256"] for node in record["nodes"]}) != 1:
            raise self.a.AcceptanceError("validators had different binaries before C activation")
        if stage != "activate" and record["nodes"][0]["old_binary_sha256"] != build["binary_sha256"]:
            raise self.a.AcceptanceError("C binary unexpectedly changed between fault stages")
        # One stop operation, then verify ALL stopped before touching any file.
        self.docker("stop", "--time", "30", *ids)
        stopped = json.loads(self.docker("inspect", *ids))
        validate_owned_nodes(stopped, root)
        if any(i["State"]["Running"] for i in stopped):
            raise self.a.AcceptanceError("refuse plan mutation while any validator runs")
        for node in record["nodes"]:
            signed = self.out / f"signed-before-{stage}-{node['id'][:12]}.json"
            self.docker("cp", f"{node['id']}:/root/.inference/data/priv_validator_state.json", str(signed))
            signed_state = self.a.load_object(signed)
            if int(signed_state["height"]) >= activation_height:
                raise self.a.AcceptanceError("a validator already signed the scheduled boundary; leave all stopped")
            node["last_signed_height"] = int(signed_state["height"])
            node["last_signed_state_sha256"] = self.a.sha256_file(signed)
        self.save()
        for index, node in enumerate(record["nodes"]):
            if stage == "activate":
                if index == 0:
                    # All originals have the same observed hash. One backup is
                    # sufficient; avoid storing the same large binary nine times.
                    backup = self.out / "original-inferenced"
                    self.docker("cp", f"{node['id']}:{node['destination']}", str(backup))
                    if self.a.sha256_file(backup) != node["old_binary_sha256"]:
                        raise self.a.AcceptanceError("original binary backup hash mismatch")
                self.docker("cp", str(binary), f"{node['id']}:{node['destination']}")
                verify_binary = self.out / "verified-inferenced"
                self.docker("cp", f"{node['id']}:{node['destination']}", str(verify_binary))
                if self.a.sha256_file(verify_binary) != build["binary_sha256"]:
                    raise self.a.AcceptanceError("stopped validator binary hash mismatch")
            self.docker("cp", str(plan_path), f"{node['id']}:/root/.inference/config/a8-query-faults.json")
            verified_plan = self.out / f"verified-plan-{stage}-{node['id'][:12]}.json"
            self.docker("cp", f"{node['id']}:/root/.inference/config/a8-query-faults.json", str(verified_plan))
            if self.a.sha256_file(verified_plan) != digest:
                raise self.a.AcceptanceError("stopped validators do not share the exact plan")
            node["verified_while_stopped"] = True
        self.save()
        self.docker("start", *ids)
        # Bounded readiness observation, never compose retry or cluster recreation.
        deadline = time.monotonic() + 90
        for node in record["nodes"]:
            node["binary_sha256"] = self.docker("exec", node["id"], "sha256sum", node["destination"]).split()[0]
            node["plan_sha256"] = self.docker("exec", node["id"], "sha256sum", "/root/.inference/config/a8-query-faults.json").split()[0]
            if node["binary_sha256"] != build["binary_sha256"] or node["plan_sha256"] != digest:
                raise self.a.AcceptanceError("validators do not share the exact binary and plan")
            started = json.loads(self.docker("inspect", node["id"]))[0]["State"]["StartedAt"]
            while True:
                logs = self.docker("logs", "--since", started, node["id"])
                if f"plan_sha256={digest}" in logs:
                    break
                if time.monotonic() >= deadline:
                    raise self.a.AcceptanceError("validator did not acknowledge the current plan")
                time.sleep(1)
            node["startup_acknowledged"] = True
            self.save()
        observed = self.wait_for_nodes(record, activation_height, deadline)
        record["confirmed_heights"] = observed
        if stage == "recover-before":
            target = int(self.scenario("r5-recover")["terms"]["target_epoch"])
            if self.a.epoch_observation(self.g)["epoch"] >= target + 3:
                raise self.a.AcceptanceError("R5.1 summary recovery missed its strictly-before-E+3 deadline")
        record["status"] = "INSTALLED"
        self.save()

    def node_observation(self, ident):
        response = self.docker("exec", ident, "inferenced", "status", "--output", "json")
        try:
            status = json.loads(response)
        except ValueError as error:
            raise self.a.AcceptanceError(f"C validator {ident} returned invalid status JSON: {response[:2000]}") from error
        if not isinstance(status, dict):
            raise self.a.AcceptanceError(f"C validator {ident} status is not an object: {response[:2000]}")
        sync = status.get("sync_info", status.get("SyncInfo", {}))
        if not isinstance(sync, dict):
            raise self.a.AcceptanceError(f"C validator {ident} sync_info is not an object: {sync}")
        catching = sync.get("catching_up", sync.get("catchingUp"))
        if not isinstance(catching, bool):
            raise self.a.AcceptanceError(f"C validator {ident} lacks authoritative sync status: {sync}")
        height = self.a.require_uint(sync.get("latest_block_height", sync.get("latestBlockHeight")), "C validator height")
        if height <= 0:
            raise self.a.AcceptanceError("C validator has not committed a block")
        return {"height": height, "catching_up": catching, "sync_info": sync}

    def node_height(self, ident):
        observation = self.node_observation(ident)
        if observation["catching_up"]:
            raise self.a.AcceptanceError(f"C validator {ident} is catching up before activation: {observation}")
        return observation["height"]

    def wait_for_nodes(self, record, activation_height, deadline):
        """Transient sync is expected AFTER restart, but never accepted as ready."""
        history = record.setdefault("readiness_observations", [])
        while True:
            observations = []
            for node in record["nodes"]:
                try:
                    observed = {"id": node["id"], **self.node_observation(node["id"])}
                except (self.a.AcceptanceError, ValueError, KeyError, TypeError) as error:
                    # A stopped-starting RPC endpoint can briefly refuse a
                    # connection. Bad status schemas and other errors fail closed.
                    transient = isinstance(error, self.a.AcceptanceError) and any(
                        word in str(error).lower() for word in ("connection refused", "connection reset by peer"))
                    observed = {"id": node["id"], "error": str(error),
                                "category": "transport_not_ready" if transient else "invalid_status"}
                    observations.append(observed)
                    if not transient:
                        history.append(observations)
                        self.save()
                        raise
                    continue
                observations.append(observed)
            history.append(observations)
            self.save()
            if all(o.get("catching_up") is False and o.get("height", 0) >= activation_height for o in observations):
                return [o["height"] for o in observations]
            if time.monotonic() >= deadline:
                raise self.a.AcceptanceError(f"C readiness deadline expired at boundary {activation_height}: {observations}")
            time.sleep(2)

    def helper(self, function, name, **kwargs):
        function(SimpleNamespace(context=str(self.path), name=name, gas="2000000", **kwargs))
        return {"context_sha256": self.a.sha256_file(self.path), "scenario": self.scenario(name)}

    def wait_for_effective_epoch(self, offset, timeout_seconds=600):
        target = int(self.scenario("r5-recover")["terms"]["target_epoch"]) + offset
        deadline = time.monotonic() + timeout_seconds
        history = self.results.setdefault("epoch_waits", [])
        record = {"phase": self.args.phase, "required_epoch": target, "observations": []}
        history.append(record)
        while True:
            observation = self.a.epoch_observation(self.g)
            record["observations"].append(observation)
            self.save()
            if observation["epoch"] == target:
                return
            if observation["epoch"] > target:
                raise self.a.AcceptanceError(f"C missed exact epoch {target}: {observation}")
            if time.monotonic() >= deadline:
                raise self.a.AcceptanceError(f"C effective epoch wait expired for {target}: {observation}")
            time.sleep(2)

    def run(self):
        phase = self.args.phase
        installed = [r["stage"] for r in self.results["activations"] if r["status"] == "INSTALLED"]
        required = {"activate": [], "routing": ["activate"], "early-r5": ["activate"],
                    "early-r4": ["activate", "recover-before"],
                    "recover-before": ["activate"], "late": ["activate", "recover-before"],
                    "recover-after": ["activate", "recover-before"],
                    "terminal": ["activate", "recover-before", "recover-after"],
                    "bank-fault": ["activate", "recover-before", "recover-after"],
                    "bank-retry": ["activate", "recover-before", "recover-after"]}
        if phase in required and installed != required[phase]:
            raise self.a.AcceptanceError(f"C phase {phase} requires activation history {required[phase]}")
        if phase in ("early-r5", "early-r4", "late"):
            self.wait_for_effective_epoch(3 if phase == "late" else 2)
        if phase == "prepare":
            if "r5-recover" in self.context.get("scenarios", {}):
                raise self.a.AcceptanceError("C bootstrap alias already exists")
            self.context.setdefault("scenarios", {})["r5-recover"] = copy.deepcopy({k: self.context[k] for k in ("contracts", "accounts", "key_names", "terms", "phases")})
            self.a.write_object(self.path, self.context)
            for name in ALL_NAMES:
                s = self.scenario(name)
                roles = [s["accounts"][r] for r in ("host", "buyer", "fee_recipient")]
                if len(set(roles)) != 3:
                    raise self.a.AcceptanceError(f"{name} requires distinct Host/Buyer/fee")
            self.results["fixture"] = {n: self.scenario(n) for n in ALL_NAMES}
            self.results["native_parameters"] = self.g.query_json("inference", "params")
            self.results["genesis_configuration"] = {"epoch_length": 75, "work_vesting_period": 2,
                                                      "reward_vesting_period": 2, "test_only": True}
            self.save()
        elif phase in ("activate", "recover-before", "recover-after"):
            self.activate(phase)
        elif phase == "routing":
            for name in ROUTING_NAMES:
                kind, op = name[3:].rsplit("-", 1)
                self.call(name, lambda n=name, k=kind, o=op: self.attempt(n, o, fault_marker(k, ROUTING)))
            self.call("r3-epoch-lock", lambda: self.attempt("r3-epoch-lock", "lock", fault_marker("handler_error", EPOCH)))
        elif phase in ("early-r5", "early-r4"):
            names = ("r5-recover", "r5-cancel") if phase == "early-r5" else R4_NAMES
            for name in names:
                kind = name[3:] if name in R4_NAMES else "handler_error"
                if name.startswith("r5-"):
                    self.call(name + "-native", lambda n=name: self.native_claim(n))
                self.call(name + "-probe", lambda n=name, k=kind: self.attempt(n, "settle_claim", fault_marker(k, SUMMARY), 2))
                self.call(name + "-e2", lambda n=name, k=kind: self.attempt(n, "refund", fault_marker(k, SUMMARY), 2), (name + "-probe",))
        elif phase == "late":
            self.call("r3-epoch-refund", lambda: self.attempt("r3-epoch-refund", "refund", fault_marker("handler_error", EPOCH), 3))
            self.call("r5-recover-ledger", lambda: self.native_claim("r5-recover"), ("r5-recover-native",))
            epoch = self.scenario("r5-recover")["terms"]["target_epoch"]
            self.call("r5-recover-refund", lambda: self.attempt("r5-recover", "refund", f"native claim for target epoch {epoch} is already confirmed", 3), ("r5-recover-e2", "r5-recover-ledger"))
            self.call("r5-recover-settle", self.settle_recovered, ("r5-recover-refund",))
            self.call("r5-recover-repeat", lambda: self.attempt("r5-recover", "settle_claim", "cannot settle claim in state"), ("r5-recover-settle",))
            for name in R4_NAMES:
                self.call(name + "-e3", lambda n=name: self.refund(n), (name + "-e2",))
            self.call("r6.3", self.cw20_rollback, ("r5-cancel-native", "r5-cancel-e2"))
            self.call("r5-cancel-e3", lambda: self.refund("r5-cancel"), ("r6.3",))
        elif phase == "terminal":
            for name in ROUTING_NAMES:
                op = name.rsplit("-", 1)[1]
                marker = None if op == "lock" else f"claim recipient for epoch {self.scenario(name)['terms']['target_epoch']} still routes exactly to this Deal"
                self.call(name + "-healthy", lambda n=name, o=op, m=marker: self.attempt(n, o, m), (name,))
            self.call("r3-epoch-lock-healthy", lambda: self.attempt("r3-epoch-lock", "lock"), ("r3-epoch-lock",))
            self.call("r3-epoch-refund-healthy", lambda: self.refund("r3-epoch-refund", exact_epoch=False), ("r3-epoch-refund",))
            self.call("r5-cancel-ledger", lambda: self.native_claim("r5-cancel"), ("r5-cancel-e3",))
            for name in R4_NAMES + ("r5-cancel",):
                for op, marker in (("refund", "cannot refund Deal in state"), ("settle_claim", "cannot settle claim in state")):
                    self.call(name + "-terminal-" + op, lambda n=name, o=op, m=marker: self.attempt(n, o, m), (name + "-e3",))
        elif phase == "bank-fault":
            self.call("r7.2-fault", self.bank_fault, ("r5-cancel-ledger",))
        elif phase == "bank-retry":
            self.call("r7.2-retry", lambda: self.helper(self.a.bank_release_retry_scenario, "r5-cancel", expected_send_index=1, rejected_recipient=self.scenario("r5-cancel")["accounts"]["host"]), ("r7.2-fault",))
            self.finalize_report()  # Persist before returning to Gradle.
        elif phase == "report":
            self.finalize_report()
        print(self.a.compact_json({"package": "C", "phase": phase, "report": str(self.out / 'results.json')}))

    def finalize_report(self):
        expected = set(ROUTING_NAMES) | {n + "-healthy" for n in ROUTING_NAMES}
        expected |= {n + suffix for n in R4_NAMES for suffix in ("-probe", "-e2", "-e3", "-terminal-refund", "-terminal-settle_claim")}
        expected |= {n + suffix for n in ("r5-recover", "r5-cancel") for suffix in ("-native", "-probe", "-e2")}
        expected |= {"r3-epoch-lock", "r3-epoch-lock-healthy", "r3-epoch-refund", "r3-epoch-refund-healthy", "r5-recover-ledger", "r5-recover-refund", "r5-recover-settle", "r5-recover-repeat", "r6.3", "r5-cancel-e3", "r5-cancel-ledger", "r5-cancel-terminal-refund", "r5-cancel-terminal-settle_claim", "r7.2-fault", "r7.2-retry"}
        missing = sorted(expected - self.results["cases"].keys())
        failed = sorted(k for k in expected if self.results["cases"].get(k, {}).get("status") != "PASS")
        stages = [r["stage"] for r in self.results["activations"] if r["status"] == "INSTALLED"]
        self.results["status"] = "PASS" if not failed and stages == ["activate", "recover-before", "recover-after"] else "PARTIAL"
        self.results["missing"] = missing
        self.results["required_native_cases"] = sorted(expected)
        self.save()
        if self.results["status"] != "PASS":
            raise self.a.AcceptanceError(f"C incomplete: {failed}; activation stages={stages}")

    def cw20_rollback(self):
        name = "r5-cancel"
        self.native_claim(name)
        setup = self.a.configure_cw20_transfer_failure(self.g, self.context, self.scenario(name)["accounts"]["buyer"])
        try:
            proof = self.attempt(name, "refund", "a8 injected CW20 transfer failure", 3)
        finally:
            # This clears ONLY the token selector. Native summary fault stays on.
            clear = self.a.configure_cw20_transfer_failure(self.g, self.context, None)
        return {"setup": setup, "rollback": proof, "clear": clear}

    def bank_fault(self):
        name = "r5-cancel"
        self.native_claim(name)
        snap = self.snapshot(name)
        if self.a.native_coin_amounts(snap["native"]["deal"]["total"], "total_amount").get("ngonka", 0) != 0:
            raise self.a.AcceptanceError("R7.2 requires original vesting fully unlocked for repeat proof")
        if self.a.normalize_release_policy(snap["state"].get("gnk_release_policy")) != "host_only":
            raise self.a.AcceptanceError("R7.2 requires HostOnly")
        return self.helper(self.a.bank_release_rollback_scenario, name, expected_send_index=1,
                           allowed_earlier_recipient=None, rejected_recipient=self.scenario(name)["accounts"]["host"],
                           exemption_id=None, proposal_id=self.args.proposal_id)


def run(args, api):
    Controller(args, api).run()
