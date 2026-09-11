#!/usr/bin/env python3
"""Live Gonka A8/B acceptance orchestration.

The script talks only to explicitly named local Docker containers. It never
prints or persists key material. All broadcasts are followed by a tx query and
`code == 0` verification before their results can enter evidence.
"""

from __future__ import annotations

import argparse
import base64
import datetime as dt
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Iterable, Mapping, Sequence


EXPECTED_GONKA_BASE_SHA = "379bebced638aeb5e6077bfd51c986f898443832"
EXPECTED_GONKA_SHA = "29a58fcf64b87967cb874b6169ce2c61e1f269b1"
EXPECTED_PROTO_SHA = "379bebced638aeb5e6077bfd51c986f898443832"
EXPECTED_WASMD_VERSION = "v0.54.2"
EXPECTED_WASMVM_VERSION = "v2.2.4"
DEFAULT_CHAIN_ID = "gonka-mainnet"
DEFAULT_GONKA_OVERLAY_DIR = Path(__file__).resolve().parents[1] / "gonka-overlay"
TEST_CW20_SYMBOL = "AUSDT"
DEFAULT_DENOM = "ngonka"
DEFAULT_NODE = "genesis-node"
DEFAULT_HOST_NODE = "join1-node"
DEFAULT_HOST_KEY = "join1"
DEFAULT_BUYER_NODE = "join2-node"
DEFAULT_BUYER_KEY = "join2"
DEFAULT_BUDGET = 100_000_000
DEFAULT_PRICE = 1_000_000
GNK_SCALE = 1_000_000_000
BPS_DENOMINATOR = 10_000
PROTOCOL_FEE_BPS = 150
NETWORK_UNCONFIRMED_DELAY_EPOCHS = 3
CLAIM_EXPIRY_DELAY_EPOCHS = 2
UINT64_MAX = (1 << 64) - 1
TESTERMINT_COMPOSE_PROJECTS = {"genesis", "join1", "join2", "testdns"}
TESTERMINT_RESERVED_CONTAINERS = {
    "genesis-node",
    "genesis-api",
    "genesis-proxy",
    "genesis-edge-api",
    "genesis-postgres",
    "genesis-mock-server",
    "join1-node",
    "join1-api",
    "join1-proxy",
    "join1-edge-api",
    "join1-postgres",
    "join1-mock-server",
    "join2-node",
    "join2-api",
    "join2-proxy",
    "join2-edge-api",
    "join2-postgres",
    "join2-mock-server",
    "test-dns",
}


class AcceptanceError(RuntimeError):
    """An expected fail-closed orchestration or assertion failure."""


@dataclass(frozen=True)
class CommandResult:
    returncode: int
    stdout: str
    stderr: str


class Runner:
    def run(
        self,
        argv: Sequence[str],
        *,
        input_text: str | None = None,
        timeout: float | None = None,
    ) -> CommandResult:
        completed = subprocess.run(
            list(argv),
            input=input_text,
            capture_output=True,
            text=True,
            check=False,
            timeout=timeout,
        )
        return CommandResult(completed.returncode, completed.stdout, completed.stderr)


def utc_now() -> str:
    return (
        dt.datetime.now(dt.timezone.utc)
        .replace(microsecond=0)
        .isoformat()
        .replace("+00:00", "Z")
    )


def compact_json(value: Mapping[str, Any]) -> str:
    return json.dumps(value, separators=(",", ":"), sort_keys=True)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def parse_runtime_identity(version_output: str) -> dict[str, str]:
    fields: dict[str, str] = {}
    for line in version_output.splitlines():
        stripped = line.strip()
        if stripped.startswith("commit:"):
            fields["gonka_source_sha"] = stripped.partition(":")[2].strip()
        elif "github.com/CosmWasm/wasmd@" in stripped:
            fields["wasmd"] = stripped.rpartition("@")[2].strip()
        elif "github.com/CosmWasm/wasmvm/v2@" in stripped:
            fields["wasmvm"] = stripped.rpartition("@")[2].strip()
        elif stripped.startswith("go:"):
            fields["go"] = stripped.partition(":")[2].strip()
        elif stripped.startswith("cosmos_sdk_version:"):
            fields["cosmos_sdk"] = stripped.partition(":")[2].strip()
    expected = {
        "gonka_source_sha": EXPECTED_GONKA_SHA,
        "wasmd": EXPECTED_WASMD_VERSION,
        "wasmvm": EXPECTED_WASMVM_VERSION,
    }
    mismatches = {
        key: {"expected": value, "actual": fields.get(key)}
        for key, value in expected.items()
        if fields.get(key) != value
    }
    if mismatches:
        raise AcceptanceError(
            "running Gonka runtime provenance mismatch: " + compact_json(mismatches)
        )
    return fields


def require_uint(
    value: Any, label: str, *, maximum: int | None = None
) -> int:
    if isinstance(value, bool):
        raise AcceptanceError(f"{label} must be an unsigned integer")
    try:
        parsed = int(value)
    except (TypeError, ValueError) as exc:
        raise AcceptanceError(f"{label} must be an unsigned integer") from exc
    if parsed < 0 or (maximum is not None and parsed > maximum):
        raise AcceptanceError(f"{label} is outside its allowed unsigned range")
    return parsed


def protobuf_bool(value: Mapping[str, Any], field: str) -> bool:
    """Decode a proto3 JSON bool, whose false default may be omitted."""
    raw = value.get(field, False)
    if not isinstance(raw, bool):
        raise AcceptanceError(f"{field} must be a protobuf boolean, got {raw!r}")
    return raw


def expected_claim_settlement(
    context: Mapping[str, Any], summary_response: Mapping[str, Any]
) -> dict[str, Any]:
    """Independent funded-settlement oracle using inputs and native evidence only."""
    terms = context.get("terms")
    accounts = context.get("accounts")
    if not isinstance(terms, Mapping) or not isinstance(accounts, Mapping):
        raise AcceptanceError("settlement oracle requires context terms and accounts")
    summary = summary_response.get("epochPerformanceSummary")
    if not isinstance(summary, Mapping):
        raise AcceptanceError("native performance summary is missing")

    target_epoch = require_uint(terms.get("target_epoch"), "terms.target_epoch")
    if require_uint(
        summary.get("epoch_index"), "summary.epoch_index", maximum=UINT64_MAX
    ) != target_epoch:
        raise AcceptanceError("native summary epoch does not match Deal terms")
    if summary.get("participant_id") != accounts.get("host"):
        raise AcceptanceError("native summary participant does not match Deal Host")
    if summary.get("claimed") is not True:
        raise AcceptanceError("native summary does not prove a claimed epoch")

    # Protobuf JSON may omit scalar zero values. Missing earned_coins therefore
    # means the native u64 default 0, while every non-zero value is parsed and
    # range-checked explicitly.
    work = require_uint(
        summary.get("earned_coins", 0), "summary.earned_coins", maximum=UINT64_MAX
    )
    reward = require_uint(
        summary.get("rewarded_coins", 0),
        "summary.rewarded_coins",
        maximum=UINT64_MAX,
    )
    total = work + reward
    configured_budget = require_uint(
        terms.get("budget_micro_usdt"), "terms.budget_micro_usdt"
    )
    price = require_uint(
        terms.get("price_micro_usdt_per_gnk"), "terms.price_micro_usdt_per_gnk"
    )
    fee_bps = require_uint(terms.get("fee_bps"), "terms.fee_bps")
    if price == 0:
        raise AcceptanceError("settlement oracle cannot use zero price")
    if fee_bps != PROTOCOL_FEE_BPS:
        raise AcceptanceError(
            f"settlement oracle requires protocol fee {PROTOCOL_FEE_BPS} bps"
        )

    has_buyer = isinstance(accounts.get("buyer"), str)
    budget = configured_budget if has_buyer else 0
    funded_capacity = budget * GNK_SCALE // price
    buyer_entitlement = min(total, funded_capacity)
    host_entitlement = total - buyer_entitlement
    gross = buyer_entitlement * price // GNK_SCALE
    if gross > budget:
        raise AcceptanceError("settlement oracle calculated gross above funded budget")
    fee = gross * fee_bps // BPS_DENOMINATOR
    host_net = gross - fee
    refund = budget - gross
    policy: str | dict[str, dict[str, int]]
    if total == 0:
        policy = "host_only"
        status = "completed"
    else:
        policy = {
            "proportional": {
                "buyer_share_numerator": buyer_entitlement,
                "share_denominator": total,
            }
        }
        status = "releasing"
    return {
        "status": status,
        "work_ngonka": work,
        "reward_ngonka": reward,
        "total_claim_ngonka": total,
        "funded_capacity_ngonka": funded_capacity,
        "buyer_entitlement_ngonka": buyer_entitlement,
        "host_entitlement_ngonka": host_entitlement,
        "gnk_release_policy": policy,
        "gross_usdt": gross,
        "fee_usdt": fee,
        "host_net_usdt": host_net,
        "buyer_refund_usdt": refund,
        "cw20_deltas": {
            "host": host_net,
            "fee_recipient": fee,
            "buyer": refund,
        },
        "deal_outflow": budget,
    }


def assert_claim_settlement_matches_oracle(
    context: Mapping[str, Any],
    summary_response: Mapping[str, Any],
    state: Mapping[str, Any],
    actual_cw20_deltas: Mapping[str, int],
    deal_outflow: int,
) -> dict[str, Any]:
    expected = expected_claim_settlement(context, summary_response)
    state_fields = (
        "status",
        "work_ngonka",
        "reward_ngonka",
        "total_claim_ngonka",
        "buyer_entitlement_ngonka",
        "host_entitlement_ngonka",
        "gnk_release_policy",
        "gross_usdt",
        "fee_usdt",
        "host_net_usdt",
        "buyer_refund_usdt",
    )
    actual_state: dict[str, Any] = {}
    expected_state: dict[str, Any] = {}
    for field in state_fields:
        expected_value = expected[field]
        actual_value = state.get(field)
        if isinstance(expected_value, int):
            actual_value = require_uint(actual_value, f"state.{field}")
        elif field == "gnk_release_policy":
            actual_value = normalize_release_policy(actual_value)
            expected_value = normalize_release_policy(expected_value)
        actual_state[field] = actual_value
        expected_state[field] = expected_value
    if actual_state != expected_state:
        raise AcceptanceError(
            "settlement state differs from independent oracle: "
            + compact_json({"expected": expected_state, "actual": actual_state})
        )
    normalized_deltas = {
        role: require_uint(actual_cw20_deltas.get(role), f"CW20 delta {role}")
        for role in ("host", "fee_recipient", "buyer")
    }
    if normalized_deltas != expected["cw20_deltas"]:
        raise AcceptanceError(
            "settlement CW20 deltas differ from independent oracle: "
            + compact_json(
                {"expected": expected["cw20_deltas"], "actual": normalized_deltas}
            )
        )
    if deal_outflow != expected["deal_outflow"]:
        raise AcceptanceError("settlement Deal outflow differs from exact funded budget")
    return expected


def cw20_settlement_fault_targets(
    context: Mapping[str, Any],
    summary_response: Mapping[str, Any],
    positions: Sequence[int],
) -> list[dict[str, Any]]:
    """Return auditable, recipient-selected CW20 failure targets for R6.1.

    The test CW20 contract selects a recipient, rather than retaining a send
    counter in state that would disappear with the failed transaction. This
    binds each requested position to the production payout order and the
    independent settlement oracle before a transaction is attempted.
    """
    expected = expected_claim_settlement(context, summary_response)
    accounts = context.get("accounts")
    if not isinstance(accounts, Mapping):
        raise AcceptanceError("CW20 settlement fault selection requires accounts")
    payout_order = ("host", "fee_recipient", "buyer")
    if sorted(positions) != list(positions) or len(set(positions)) != len(positions):
        raise AcceptanceError("CW20 fault positions must be strictly increasing")
    if not positions or any(position < 1 or position > len(payout_order) for position in positions):
        raise AcceptanceError("CW20 fault position is outside the three-send settlement order")

    targets: list[dict[str, Any]] = []
    recipients: set[str] = set()
    for position in positions:
        role = payout_order[position - 1]
        recipient = accounts.get(role)
        amount = require_uint(expected["cw20_deltas"].get(role), f"CW20 {role} payout")
        if not isinstance(recipient, str) or not recipient:
            raise AcceptanceError(f"CW20 send #{position} lacks a {role} recipient")
        if amount == 0:
            raise AcceptanceError(
                f"R6.1 requires three non-zero payouts; CW20 send #{position} is zero"
            )
        if recipient in recipients:
            raise AcceptanceError("R6.1 requires distinct recipients for recipient-selected faults")
        recipients.add(recipient)
        targets.append(
            {
                "outgoing_transfer_index": position,
                "role": role,
                "recipient": recipient,
                "amount": amount,
            }
        )
    return targets


def normalize_release_policy(value: Any) -> str | dict[str, dict[str, int]]:
    if value == "host_only":
        return "host_only"
    if isinstance(value, Mapping) and isinstance(value.get("proportional"), Mapping):
        proportional = value["proportional"]
        return {
            "proportional": {
                "buyer_share_numerator": require_uint(
                    proportional.get("buyer_share_numerator"),
                    "release policy buyer_share_numerator",
                ),
                "share_denominator": require_uint(
                    proportional.get("share_denominator"),
                    "release policy share_denominator",
                ),
            }
        }
    raise AcceptanceError(f"invalid GNK release policy: {value!r}")


def assert_deal_terms(
    deal_config: Mapping[str, Any], expected: Mapping[str, Any]
) -> None:
    integer_fields = {
        "target_epoch",
        "price_micro_usdt_per_gnk",
        "buyer_budget_micro_usdt",
        "funded_capacity_ngonka",
        "fee_bps",
    }
    actual: dict[str, Any] = {}
    normalized_expected: dict[str, Any] = {}
    for field, expected_value in expected.items():
        actual_value = deal_config.get(field)
        if field in integer_fields:
            actual_value = require_uint(actual_value, f"Deal config {field}")
            expected_value = require_uint(expected_value, f"expected Deal {field}")
        actual[field] = actual_value
        normalized_expected[field] = expected_value
    if actual != normalized_expected:
        raise AcceptanceError(
            "on-chain Deal terms differ from independent offer inputs: "
            + compact_json({"expected": normalized_expected, "actual": actual})
        )


def assert_fresh_gonka_local_state(gonka_dir: Path) -> None:
    prod_local = gonka_dir.resolve() / "prod-local"
    if os.path.lexists(prod_local):
        if prod_local.is_dir() and not any(prod_local.iterdir()):
            return
        raise AcceptanceError(
            "refusing destructive Testermint reboot because Gonka prod-local already "
            f"exists: {prod_local}. Testermint deletes this entire ignored path; use "
            "a fresh checkout or preserve/remove it explicitly outside the harness"
        )
    # Create the bind root from the launching host before crossing into WSL.
    # Docker Desktop's DrvFs/9p bind can retain a deleted name as simultaneously
    # existing for mkdir and absent for lookup. A host-created empty root avoids
    # that inconsistent first mkdir; DockerGroup still owns child cleanup/setup.
    prod_local.mkdir()


def expected_release(before_state: Mapping[str, Any], available: int) -> dict[str, int]:
    if available < 0:
        raise AcceptanceError("release oracle received a negative available balance")
    previous_total = int(before_state["released_total_ngonka"])
    previous_buyer = int(before_state["buyer_released_ngonka"])
    previous_host = int(before_state["host_released_ngonka"])
    policy = before_state.get("gnk_release_policy")
    if policy == "host_only":
        numerator, denominator = 0, 1
    elif isinstance(policy, dict) and isinstance(policy.get("proportional"), dict):
        proportional = policy["proportional"]
        numerator = int(proportional["buyer_share_numerator"])
        denominator = int(proportional["share_denominator"])
        if denominator <= 0 or numerator < 0 or numerator > denominator:
            raise AcceptanceError("release oracle received an invalid proportional policy")
    else:
        raise AcceptanceError(f"release oracle cannot use policy {policy!r}")

    canonical_previous_buyer = previous_total * numerator // denominator
    canonical_previous_host = previous_total - canonical_previous_buyer
    if (previous_buyer, previous_host) != (
        canonical_previous_buyer,
        canonical_previous_host,
    ):
        raise AcceptanceError("release oracle found non-canonical pre-release counters")

    released_total = previous_total + available
    buyer_released = released_total * numerator // denominator
    host_released = released_total - buyer_released
    return {
        "released_total": released_total,
        "buyer_released": buyer_released,
        "host_released": host_released,
        "buyer_delta": buyer_released - previous_buyer,
        "host_delta": host_released - previous_host,
    }


def assert_release_matches_oracle(
    before_state: Mapping[str, Any],
    after_state: Mapping[str, Any],
    available: int,
    buyer_delta: int,
    host_delta: int,
) -> dict[str, int]:
    expected = expected_release(before_state, available)
    actual_counters = {
        "released_total": int(after_state["released_total_ngonka"]),
        "buyer_released": int(after_state["buyer_released_ngonka"]),
        "host_released": int(after_state["host_released_ngonka"]),
    }
    expected_counters = {
        key: expected[key]
        for key in ("released_total", "buyer_released", "host_released")
    }
    if actual_counters != expected_counters:
        raise AcceptanceError(
            "release counters differ from independent oracle: "
            + compact_json({"expected": expected_counters, "actual": actual_counters})
        )
    actual_deltas = {"buyer_delta": buyer_delta, "host_delta": host_delta}
    expected_deltas = {
        key: expected[key] for key in ("buyer_delta", "host_delta")
    }
    if actual_deltas != expected_deltas:
        raise AcceptanceError(
            "release bank deltas differ from independent oracle: "
            + compact_json({"expected": expected_deltas, "actual": actual_deltas})
        )
    return expected


def bank_release_fault_target(
    before_state: Mapping[str, Any],
    available: int,
    buyer: str | None,
    host: str,
    send_index: int,
) -> dict[str, Any]:
    """Select the real BankMsg recipient from release order and exact oracle."""
    planned = expected_release(before_state, available)
    if send_index == 1:
        if planned["buyer_delta"] > 0:
            if not isinstance(buyer, str):
                raise AcceptanceError("proportional Bank send #1 lacks Buyer")
            return {"recipient": buyer, "amount": planned["buyer_delta"], "plan": planned}
        if planned["host_delta"] <= 0:
            raise AcceptanceError("Bank send #1 has no positive payout")
        return {"recipient": host, "amount": planned["host_delta"], "plan": planned}
    if send_index == 2:
        if not isinstance(buyer, str) or planned["buyer_delta"] <= 0 or planned["host_delta"] <= 0:
            raise AcceptanceError("Bank send #2 requires two non-zero proportional payouts")
        return {"recipient": host, "amount": planned["host_delta"], "plan": planned}
    raise AcceptanceError("Bank fault send index must be 1 or 2")


def assert_live_bank_second_send_exemption(
    params: Mapping[str, Any],
    current_height: int,
    exemption_id: str,
    deal: str,
    buyer: str,
    buyer_amount: int,
) -> dict[str, Any]:
    """Prove live Bank params, not a launcher argument, allow only send #1."""
    exemptions = params.get("emergency_transfer_exemptions")
    if not isinstance(exemptions, list):
        raise AcceptanceError("restrictions params lack emergency_transfer_exemptions")
    exact = [
        exemption
        for exemption in exemptions
        if isinstance(exemption, Mapping)
        and exemption.get("exemption_id") == exemption_id
        and exemption.get("from_address") == deal
        and exemption.get("to_address") == buyer
    ]
    if len(exact) != 1:
        raise AcceptanceError("live Bank params lack one exact Deal-to-Buyer exemption")
    exemption = exact[0]
    if require_uint(exemption.get("max_amount"), "Bank exemption max_amount") < buyer_amount:
        raise AcceptanceError("live Deal-to-Buyer exemption cannot cover Bank send #1")
    if require_uint(exemption.get("expiry_block"), "Bank exemption expiry_block") <= current_height:
        raise AcceptanceError("live Deal-to-Buyer exemption is already expired")
    if require_uint(exemption.get("usage_limit"), "Bank exemption usage_limit") == 0:
        raise AcceptanceError("live Deal-to-Buyer exemption has no usage")
    usage_entries = params.get("exemption_usage_tracking")
    # Protobuf repeated fields can be omitted or null when empty. Other
    # representations must still fail; never coerce arbitrary falsy values.
    if usage_entries is None:
        usage_entries = []
    if not isinstance(usage_entries, list):
        raise AcceptanceError("restrictions params lack exemption_usage_tracking")
    usage = 0
    for entry in usage_entries:
        if not isinstance(entry, Mapping):
            raise AcceptanceError("restrictions params contain a malformed usage entry")
        if entry.get("exemption_id") == exemption_id and entry.get("account_address") == deal:
            usage = require_uint(entry.get("usage_count"), "Bank exemption usage_count")
            break
    if usage >= require_uint(exemption.get("usage_limit"), "Bank exemption usage_limit"):
        raise AcceptanceError("live Deal-to-Buyer exemption is already exhausted")
    # Broad permissions would invalidate a recipient-selected proof even if the
    # intended exact entry also exists.
    for other in exemptions:
        if not isinstance(other, Mapping) or other is exemption:
            continue
        if other.get("from_address") == deal and other.get("to_address") in {buyer, "*"}:
            raise AcceptanceError("live Bank params contain an additional broad Buyer permission")
    return dict(exemption)


def docker_resource_collisions(runner: Runner) -> dict[str, list[str]]:
    containers = runner.run(
        [
            "docker",
            "ps",
            "-a",
            "--format",
            '{{.Names}}\t{{.Label "com.docker.compose.project"}}',
        ],
        timeout=30,
    )
    volumes = runner.run(
        [
            "docker",
            "volume",
            "ls",
            "--format",
            '{{.Name}}\t{{.Label "com.docker.compose.project"}}',
        ],
        timeout=30,
    )
    networks = runner.run(
        ["docker", "network", "ls", "--format", "{{.Name}}"], timeout=30
    )
    for label, result in (
        ("containers", containers),
        ("volumes", volumes),
        ("networks", networks),
    ):
        if result.returncode != 0:
            raise AcceptanceError(
                result.stderr.strip() or f"cannot inspect Docker {label}"
            )

    container_hits: list[str] = []
    for line in containers.stdout.splitlines():
        name, _, project = line.partition("\t")
        if name in TESTERMINT_RESERVED_CONTAINERS or project in TESTERMINT_COMPOSE_PROJECTS:
            container_hits.append(name)
    volume_hits: list[str] = []
    prefixes = tuple(f"{project}_" for project in TESTERMINT_COMPOSE_PROJECTS)
    for line in volumes.stdout.splitlines():
        name, _, project = line.partition("\t")
        if project in TESTERMINT_COMPOSE_PROJECTS or name.startswith(prefixes):
            volume_hits.append(name)
    network_hits = [
        name
        for name in networks.stdout.splitlines()
        if name.strip() == "chain-public"
    ]
    return {
        "containers": sorted(set(container_hits)),
        "volumes": sorted(set(volume_hits)),
        "networks": sorted(set(network_hits)),
    }


def load_object(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise AcceptanceError(f"cannot read JSON {path}: {exc}") from exc
    if not isinstance(value, dict):
        raise AcceptanceError(f"JSON root must be an object: {path}")
    return value


def write_object(path: Path, value: Mapping[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(
        json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    os.replace(temporary, path)


def parse_object(result: CommandResult, context: str) -> dict[str, Any]:
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip()
        raise AcceptanceError(f"{context} failed: {detail}")
    try:
        value = json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        raise AcceptanceError(f"{context} did not return JSON: {exc}") from exc
    if not isinstance(value, dict):
        raise AcceptanceError(f"{context} JSON root must be an object")
    return value


def unwrap_tx(value: Mapping[str, Any]) -> Mapping[str, Any]:
    nested = value.get("tx_response")
    return nested if isinstance(nested, dict) else value


def tx_code(value: Mapping[str, Any]) -> int:
    raw = unwrap_tx(value).get("code", 0)
    try:
        return int(raw)
    except (TypeError, ValueError) as exc:
        raise AcceptanceError(f"transaction code is not an integer: {raw!r}") from exc


def tx_hash(value: Mapping[str, Any]) -> str:
    tx = unwrap_tx(value)
    raw = tx.get("txhash") or tx.get("tx_hash")
    if not isinstance(raw, str) or not raw:
        raise AcceptanceError("transaction response has no tx hash")
    return raw


def tx_events(value: Mapping[str, Any]) -> list[Mapping[str, Any]]:
    tx = unwrap_tx(value)
    events: list[Mapping[str, Any]] = []
    direct = tx.get("events")
    if isinstance(direct, list):
        events.extend(item for item in direct if isinstance(item, dict))
    logs = tx.get("logs")
    if isinstance(logs, list):
        for log in logs:
            if isinstance(log, dict) and isinstance(log.get("events"), list):
                events.extend(item for item in log["events"] if isinstance(item, dict))
    return events


def event_values(value: Mapping[str, Any], keys: Iterable[str]) -> list[str]:
    wanted = set(keys)
    found: list[str] = []
    for event in tx_events(value):
        attributes = event.get("attributes")
        if not isinstance(attributes, list):
            continue
        for attribute in attributes:
            if (
                isinstance(attribute, dict)
                and attribute.get("key") in wanted
                and isinstance(attribute.get("value"), str)
            ):
                found.append(attribute["value"])
    return sorted(set(found))


def one_event_value(value: Mapping[str, Any], keys: Iterable[str], context: str) -> str:
    values = event_values(value, keys)
    if len(values) != 1:
        raise AcceptanceError(f"{context}: expected one event value, got {values}")
    return values[0]


def assert_exact_bank_transfer_event(
    value: Mapping[str, Any], sender: str, recipient: str, amount: int, denom: str
) -> dict[str, str]:
    """Require one Bank transfer event proving the requested native movement."""
    expected = {
        "sender": sender,
        "recipient": recipient,
        "amount": f"{amount}{denom}",
    }
    matches: list[dict[str, str]] = []
    for event in tx_events(value):
        if event.get("type") != "transfer":
            continue
        attributes = event.get("attributes")
        if not isinstance(attributes, list):
            continue
        observed = {
            str(attribute.get("key")): str(attribute.get("value"))
            for attribute in attributes
            if isinstance(attribute, dict)
            and isinstance(attribute.get("key"), str)
            and isinstance(attribute.get("value"), str)
        }
        if all(observed.get(key) == expected_value for key, expected_value in expected.items()):
            matches.append(expected)
    if len(matches) != 1:
        raise AcceptanceError(
            f"expected one exact Bank transfer event {expected}, got {len(matches)}"
        )
    return matches[0]


def assert_no_bank_transfer_involving(value: Mapping[str, Any], address: str) -> None:
    """Reject any Bank transfer event whose sender or recipient is the Deal."""
    for event in tx_events(value):
        if event.get("type") != "transfer":
            continue
        attributes = event.get("attributes")
        if not isinstance(attributes, list):
            continue
        endpoints = {
            attribute.get("value")
            for attribute in attributes
            if isinstance(attribute, dict)
            and attribute.get("key") in {"sender", "recipient"}
        }
        if address in endpoints:
            raise AcceptanceError("transaction emitted a Bank transfer involving Deal")


def assert_no_denom_transfer_involving(
    value: Mapping[str, Any], address: str, denom: str
) -> None:
    """Reject a Deal Bank transfer whose amount contains the protected denom."""
    for event in tx_events(value):
        if event.get("type") != "transfer":
            continue
        attributes = event.get("attributes")
        if not isinstance(attributes, list):
            continue
        observed = {
            str(attribute.get("key")): str(attribute.get("value"))
            for attribute in attributes
            if isinstance(attribute, dict)
            and isinstance(attribute.get("key"), str)
            and isinstance(attribute.get("value"), str)
        }
        if address not in {observed.get("sender"), observed.get("recipient")}:
            continue
        amounts = observed.get("amount", "").split(",")
        if any(amount.endswith(denom) for amount in amounts):
            raise AcceptanceError(
                f"transaction transferred protected denom {denom} involving Deal"
            )


def filtered_tx(value: Mapping[str, Any]) -> dict[str, Any]:
    tx = unwrap_tx(value)
    allowed_types = {
        "store_code",
        "instantiate",
        "execute",
        "wasm",
        "wasm-offer_created",
        "wasm-deal_funded",
        "wasm-deal_locked",
        "wasm-claim_settled",
        "wasm-usdt_paid",
        "wasm-usdt_refunded",
        "wasm-gnk_released",
        "wasm-deal_completed",
        "wasm-deal_refunded",
        "wasm-deal_expired",
        "wasm-deal_cancelled",
        "wasm-refund_reason",
        "transfer",
    }
    events = [event for event in tx_events(value) if event.get("type") in allowed_types]
    return {
        "tx_hash": tx_hash(value),
        "height": str(tx.get("height", "")),
        "code": tx_code(value),
        "gas_wanted": str(tx.get("gas_wanted", tx.get("gasWanted", ""))),
        "gas_used": str(tx.get("gas_used", tx.get("gasUsed", ""))),
        "events": events,
    }


def withdraw_pending_usdt(
    gonka, deal: str, payments: Mapping[str, Any],
    record_result: Callable[[dict[str, Any]], None] | None = None,
) -> dict[str, Any]:
    """One confirmed transaction per role; a failed role cannot starve others.

    Re-query usdt_payments on the next keeper run to retry only remaining debt.
    This acceptance helper reports failure after attempting every pending role.
    """
    transactions = {}
    failures = []
    for role in ("host", "fee", "buyer"):
        amount = require_uint(payments[role]["pending_micro_usdt"], f"{role}.pending_micro_usdt")
        if amount == 0:
            continue
        try:
            result = gonka.execute(
                DEFAULT_NODE, "genesis", deal, {"withdraw_usdt": {"role": role}}, gas="auto"
            )
            transactions[role] = filtered_tx(result)
        except AcceptanceError as error:
            failures.append(role)
            outcome = {"role": role, "status": "failed", "error": str(error)}
        else:
            outcome = {"role": role, "status": "confirmed", "tx": transactions[role]}
        # Persist outside the try block: a disk failure must stop further broadcasts.
        if record_result is not None:
            record_result(outcome)
    if failures:
        raise AcceptanceError(
            "USDT withdrawals remain pending for: " + ", ".join(failures)
            + "; other roles were attempted independently; retry using usdt_payments"
        )
    return transactions


def record_settlement_phase(path: Path, phase: Mapping[str, Any], name: str | None = None) -> None:
    context = load_object(path)
    target = context if name is None else context["scenarios"][name]
    target.setdefault("phases", []).append({"recorded_at_utc": utc_now(), **phase})
    write_object(path, context)


def recorded_withdrawals(gonka, path: Path, deal: str, payments: Mapping[str, Any],
                         name: str | None = None) -> dict[str, Any]:
    return withdraw_pending_usdt(
        gonka, deal, payments,
        lambda outcome: record_settlement_phase(path, {
            "name": "usdt_withdrawal_attempt", "deal": deal, **outcome,
        }, name),
    )


def verify_recorded_settlement(gonka, path: Path, name: str | None = None) -> None:
    """Resume the full balance oracle from durable pre-settlement evidence."""
    context = load_object(path)
    target = context if name is None else context["scenarios"][name]
    checkpoints = [p for p in target.get("phases", []) if p.get("name") == "settlement_committed"]
    if not checkpoints:
        return  # Existing contexts may predate checkpoint support.
    checkpoint = checkpoints[-1]
    deal = target["contracts"]["deal"]
    cw20 = target["contracts"]["cw20"]
    payments = gonka.smart(deal, {"usdt_payments": {}})
    if any(require_uint(payments[r]["pending_micro_usdt"], r) for r in ("host", "fee", "buyer")):
        raise AcceptanceError("settlement delivery verification requires all USDT payments")
    after = {role: gonka.cw20_balance(cw20, address)
             for role, address in checkpoint["addresses"].items()}
    before = checkpoint["before"]["cw20"]
    state = gonka.smart(deal, {"state": {}})
    # GNK may finish while USDT awaits retry; settlement economics remain frozen.
    expected = expected_claim_settlement(target, checkpoint["summary"])
    oracle_state = dict(state)
    if state.get("status") == "completed" and expected["status"] == "releasing":
        oracle_state["status"] = "releasing"
    deltas = {r: after[r] - before[r] for r in ("host", "fee_recipient", "buyer")}
    result = assert_claim_settlement_matches_oracle(
        target, checkpoint["summary"], oracle_state, deltas, before["deal"] - after["deal"]
    )
    record_settlement_phase(path, {
        "name": "settlement_delivery_verified", "deal": deal,
        "settle_tx": checkpoint["settle_tx"], "expected": result,
        "actual": {"cw20_deltas": deltas, "deal_outflow": before["deal"] - after["deal"]},
        "payments": payments, "state": state,
    }, name)


def filtered_tx_attempt(value: Mapping[str, Any], layer: str) -> dict[str, Any]:
    """Keep failure evidence useful while excluding command input and key material."""
    tx = unwrap_tx(value)
    raw_hash = tx.get("txhash") or tx.get("tx_hash")
    return {
        "layer": layer,
        "tx_hash": raw_hash if isinstance(raw_hash, str) else "",
        "height": str(tx.get("height", "")),
        "code": tx_code(value),
        "codespace": str(tx.get("codespace", "")),
        "gas_wanted": str(tx.get("gas_wanted", tx.get("gasWanted", ""))),
        "gas_used": str(tx.get("gas_used", tx.get("gasUsed", ""))),
        "raw_log": str(tx.get("raw_log", tx.get("rawLog", ""))),
        "events": [event for event in tx_events(value) if event.get("type") == "transfer"],
    }


class DockerGonka:
    def __init__(self, runner: Runner, chain_id: str = DEFAULT_CHAIN_ID) -> None:
        self.runner = runner
        self.chain_id = chain_id

    def docker(self, *args: str, timeout: float = 60) -> CommandResult:
        return self.runner.run(["docker", *args], timeout=timeout)

    def cli(
        self,
        container: str,
        *args: str,
        input_text: str | None = None,
        timeout: float = 60,
    ) -> CommandResult:
        return self.runner.run(
            ["docker", "exec", "-i", container, "inferenced", *args],
            input_text=input_text,
            timeout=timeout,
        )

    @staticmethod
    def keyring(container: str) -> tuple[str, str | None]:
        pair = container.removesuffix("-node")
        if "genesis" in pair:
            return "test", None
        return "file", pair.lstrip("/").ljust(10, "0") + "\n"

    def query_json(self, *args: str, container: str = DEFAULT_NODE) -> dict[str, Any]:
        result = self.cli(container, "query", *args, "--output", "json")
        return parse_object(result, f"query {' '.join(args)}")

    def status(self) -> dict[str, Any]:
        return parse_object(
            self.cli(DEFAULT_NODE, "status", "--output", "json"), "node status"
        )

    def binary_version(self) -> str:
        result = self.cli(DEFAULT_NODE, "version", "--long", timeout=30)
        if result.returncode != 0:
            raise AcceptanceError(result.stderr.strip() or "cannot read binary version")
        return result.stdout.strip()

    def transfer_restriction_status(self) -> dict[str, Any]:
        result = self.cli(DEFAULT_NODE, "query", "restrictions", "status")
        if result.returncode != 0:
            raise AcceptanceError(
                result.stderr.strip() or "cannot read transfer restriction status"
            )
        fields: dict[str, str] = {}
        for line in result.stdout.splitlines():
            key, separator, value = line.partition(":")
            if separator:
                fields[key.strip()] = value.strip().strip('"')
        try:
            return {
                # Protobuf text omits scalar zero values, so missing fields are
                # their schema defaults: false and 0.
                "is_active": fields.get("is_active", "false").lower() == "true",
                "restriction_end_block": int(fields.get("restriction_end_block", "0")),
                "current_block_height": int(fields["current_block_height"]),
                "remaining_blocks": int(fields.get("remaining_blocks", "0")),
            }
        except (KeyError, ValueError) as exc:
            raise AcceptanceError(
                f"cannot parse transfer restriction status: {result.stdout!r}"
            ) from exc

    def transfer_restriction_params(self) -> dict[str, Any]:
        response = self.query_json("restrictions", "params")
        params = response.get("params")
        if not isinstance(params, dict):
            raise AcceptanceError(f"restrictions params query has no params object: {response}")
        return params

    def wait_tx(self, hash_value: str, timeout: float = 120) -> dict[str, Any]:
        deadline = time.monotonic() + timeout
        while True:
            result = self.cli(
                DEFAULT_NODE,
                "query",
                "tx",
                hash_value,
                "--output",
                "json",
                timeout=20,
            )
            if result.returncode == 0:
                value = parse_object(result, f"query tx {hash_value}")
                if tx_code(value) != 0:
                    tx = unwrap_tx(value)
                    raise AcceptanceError(
                        f"confirmed tx {hash_value} failed with code {tx_code(value)}: "
                        f"{tx.get('raw_log', '')}"
                    )
                return value
            if time.monotonic() >= deadline:
                raise AcceptanceError(
                    f"tx {hash_value} was broadcast but confirmation is ambiguous; do not resend"
                )
            time.sleep(1)

    def wait_tx_any(self, hash_value: str, timeout: float = 120) -> dict[str, Any]:
        """Return an included transaction even when DeliverTx rejected it."""
        deadline = time.monotonic() + timeout
        while True:
            result = self.cli(
                DEFAULT_NODE,
                "query",
                "tx",
                hash_value,
                "--output",
                "json",
                timeout=20,
            )
            if result.returncode == 0:
                return parse_object(result, f"query tx {hash_value}")
            if time.monotonic() >= deadline:
                raise AcceptanceError(
                    f"tx {hash_value} was broadcast but confirmation is ambiguous; do not resend"
                )
            time.sleep(1)

    def tx_attempt(
        self,
        container: str,
        key: str,
        *args: str,
        gas: str,
    ) -> dict[str, Any]:
        """Broadcast once and capture CheckTx or DeliverTx evidence without retry."""
        backend, password = self.keyring(container)
        result = self.cli(
            container,
            "tx",
            *args,
            "--from",
            key,
            "--keyring-backend",
            backend,
            "--keyring-dir=/root/.inference",
            "--chain-id",
            self.chain_id,
            "--gas",
            gas,
            "--gas-adjustment",
            "5.0",
            "--unordered",
            "--timeout-duration",
            "60s",
            "--broadcast-mode",
            "sync",
            "--yes",
            "--output",
            "json",
            input_text=password,
            timeout=120,
        )
        broadcast = parse_object(result, f"broadcast attempt {' '.join(args[:3])}")
        if tx_code(broadcast) != 0:
            return filtered_tx_attempt(broadcast, "check_tx")
        included = self.wait_tx_any(tx_hash(broadcast))
        return filtered_tx_attempt(included, "deliver_tx")

    def tx(
        self,
        container: str,
        key: str,
        *args: str,
        gas: str = "2000000",
    ) -> dict[str, Any]:
        backend, password = self.keyring(container)
        result = self.cli(
            container,
            "tx",
            *args,
            "--from",
            key,
            "--keyring-backend",
            backend,
            "--keyring-dir=/root/.inference",
            "--chain-id",
            self.chain_id,
            "--gas",
            gas,
            "--gas-adjustment",
            "5.0",
            "--unordered",
            "--timeout-duration",
            "60s",
            "--broadcast-mode",
            "sync",
            "--yes",
            "--output",
            "json",
            input_text=password,
            timeout=120,
        )
        broadcast = parse_object(result, f"broadcast {' '.join(args[:3])}")
        if tx_code(broadcast) != 0:
            tx = unwrap_tx(broadcast)
            raise AcceptanceError(
                f"CheckTx failed with code {tx_code(broadcast)}: {tx.get('raw_log', '')}"
            )
        return self.wait_tx(tx_hash(broadcast))

    def key_address(self, container: str, key: str) -> str:
        backend, password = self.keyring(container)
        result = self.cli(
            container,
            "keys",
            "show",
            key,
            "--address",
            "--keyring-backend",
            backend,
            "--keyring-dir=/root/.inference",
            input_text=password,
        )
        if result.returncode != 0:
            raise AcceptanceError(result.stderr.strip() or f"cannot read key {key}")
        address = result.stdout.strip()
        if not address.startswith("gonka1"):
            raise AcceptanceError(f"unexpected address for key {key}: {address!r}")
        return address

    def create_key(self, key: str) -> str:
        result = self.cli(
            DEFAULT_NODE,
            "keys",
            "add",
            key,
            "--keyring-backend",
            "test",
            "--keyring-dir=/root/.inference",
            "--output",
            "json",
        )
        # The JSON contains a mnemonic. Parse it in memory, retain only address,
        # and never place stdout in an exception or evidence.
        if result.returncode != 0:
            raise AcceptanceError(f"cannot create isolated local test key {key}")
        try:
            value = json.loads(result.stdout)
            address = value["address"]
        except (json.JSONDecodeError, KeyError, TypeError) as exc:
            raise AcceptanceError(f"cannot parse address for isolated key {key}") from exc
        if not isinstance(address, str) or not address.startswith("gonka1"):
            raise AcceptanceError(f"invalid address created for isolated key {key}")
        return address

    def copy_wasm(self, path: Path, remote_name: str) -> str:
        path = path.resolve()
        if not path.is_file():
            raise AcceptanceError(f"Wasm artifact missing: {path}")
        remote = f"/tmp/{remote_name}"
        result = self.docker("cp", str(path), f"{DEFAULT_NODE}:{remote}", timeout=120)
        if result.returncode != 0:
            raise AcceptanceError(result.stderr.strip() or f"cannot copy {path}")
        remote_hash = self.docker(
            "exec", DEFAULT_NODE, "sha256sum", remote, timeout=30
        )
        if remote_hash.returncode != 0:
            raise AcceptanceError(remote_hash.stderr.strip() or "cannot hash copied Wasm")
        actual = remote_hash.stdout.split()[0]
        expected = sha256_file(path)
        if actual != expected:
            raise AcceptanceError(f"copied Wasm hash mismatch: {expected} != {actual}")
        return remote

    def store(self, path: Path, label: str) -> tuple[str, dict[str, Any]]:
        digest = sha256_file(path)
        remote = self.copy_wasm(path, f"a8-{label}-{digest[:12]}.wasm")
        tx = self.tx(DEFAULT_NODE, "genesis", "wasm", "store", remote, gas="auto")
        code_id = one_event_value(tx, ("code_id",), f"store {label}")
        checksums = event_values(tx, ("code_checksum", "checksum"))
        if checksums and digest not in {item.lower().removeprefix("0x") for item in checksums}:
            raise AcceptanceError(f"store checksum mismatch for {label}: {checksums} != {digest}")
        return code_id, {
            "label": label,
            "local_path": str(path),
            "sha256": digest,
            "code_id": code_id,
            "tx": filtered_tx(tx),
        }

    def instantiate(
        self,
        code_id: str,
        message: Mapping[str, Any],
        label: str,
        *,
        key: str = "genesis",
    ) -> tuple[str, dict[str, Any]]:
        tx = self.tx(
            DEFAULT_NODE,
            key,
            "wasm",
            "instantiate",
            code_id,
            compact_json(message),
            "--label",
            label,
            "--no-admin",
            gas="auto",
        )
        address = one_event_value(
            tx, ("_contract_address", "contract_address"), f"instantiate {label}"
        )
        info = self.query_json("wasm", "contract", address)
        nested = info.get("contract_info") if isinstance(info.get("contract_info"), dict) else info
        if nested.get("admin") not in (None, ""):
            raise AcceptanceError(f"test deployment unexpectedly has admin: {address}")
        return address, {"address": address, "contract_info": info, "tx": filtered_tx(tx)}

    def execute(
        self,
        container: str,
        key: str,
        contract: str,
        message: Mapping[str, Any],
        *,
        gas: str = "2000000",
    ) -> dict[str, Any]:
        return self.tx(
            container,
            key,
            "wasm",
            "execute",
            contract,
            compact_json(message),
            gas=gas,
        )

    def smart(self, contract: str, message: Mapping[str, Any]) -> dict[str, Any]:
        value = self.query_json(
            "wasm", "contract-state", "smart", contract, compact_json(message)
        )
        nested = value.get("data")
        return nested if isinstance(nested, dict) else value

    def bank_balance(self, address: str, denom: str = DEFAULT_DENOM) -> int:
        value = self.query_json("bank", "balance", address, denom)
        balance = value.get("balance")
        if not isinstance(balance, dict):
            raise AcceptanceError(f"bank balance response missing balance: {value}")
        return int(balance.get("amount", "0"))

    def bank_balances(self, address: str) -> dict[str, int]:
        value = self.query_json("bank", "balances", address)
        balances = value.get("balances")
        if balances is None:
            balances = []
        if not isinstance(balances, list):
            raise AcceptanceError(f"bank balances response missing balances: {value}")
        normalized: dict[str, int] = {}
        for coin in balances:
            if not isinstance(coin, Mapping) or not isinstance(coin.get("denom"), str):
                raise AcceptanceError(f"malformed bank balance coin: {coin}")
            denom = coin["denom"]
            normalized[denom] = normalized.get(denom, 0) + require_uint(
                coin.get("amount"), f"bank balance {address}/{denom}"
            )
        return normalized

    def cw20_balance(self, contract: str, address: str) -> int:
        value = self.smart(contract, {"balance": {"address": address}})
        return int(value["balance"])


def require_exact_sha(gonka_dir: Path) -> None:
    result = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=gonka_dir,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0 or result.stdout.strip() != EXPECTED_GONKA_SHA:
        raise AcceptanceError(
            f"Gonka checkout must be exact {EXPECTED_GONKA_SHA}, got {result.stdout.strip()!r}"
        )


def wsl_path(path: Path, runner: Runner) -> str:
    del runner
    resolved = path.resolve()
    if os.name != "nt":
        return str(resolved)
    drive = resolved.drive.removesuffix(":").lower()
    if len(drive) != 1 or not drive.isalpha():
        raise AcceptanceError(f"cannot convert non-drive path for WSL: {resolved}")
    tail = resolved.as_posix()[2:].lstrip("/")
    return f"/mnt/{drive}/{tail}"


PR4_MANUAL_WORKFLOWS_SHA = "f4a8b8ab10b106e68bd80c1e176f276b8989859e"

B3_HARNESS_GONKA_PATHS = {
    "local-test-net/docker-compose.a8-query-faults.yml",
    "docs/a8-preparation/package-a.md",
    "docs/a8-preparation/package-b.md",
    "docs/a8-preparation/runtime-faults.md",
    "testermint/src/test/kotlin/A8PackageBBankFaultPlan.kt",
    "testermint/src/test/kotlin/A8PackageBBankFaultPlanTests.kt",
    "inference-chain/app/a8_query_fault_selector_test.go",
    "inference-chain/app/a8_faults_disabled_test.go",
    "inference-chain/app/a8_faults_enabled_test.go",
    "inference-chain/app/a8faults/plan_test.go",

    "inference-chain/scripts/init-docker-genesis.sh",
    "local-test-net/docker-compose.genesis-a8-b3-foreign-denom.yml",
    "testermint/src/main/kotlin/DockerGroup.kt",
    "testermint/src/main/kotlin/LocalInferencePair.kt",
    "testermint/src/test/kotlin/DockerBindUserArgsTests.kt",
    "testermint/src/test/kotlin/MarketplaceContractAcceptanceTests.kt",
    "testermint/src/test/kotlin/MarketplaceHarnessProcess.kt",
    "testermint/src/test/kotlin/MarketplaceHarnessProcessTests.kt",
    "testermint/src/test/resources/a8-b3-genesis-validation-overrides.json",
}

# Exact reviewed blobs: the default build is a no-op; only a8faults installs
# the bounded test provider. Any later runtime edit requires explicit repinning.
A8_RUNTIME_ADAPTER_BLOBS = {
    "inference-chain/app/legacy.go": "a2949596a4eb21c0380c32b7b27c6050eca461d1",
    "inference-chain/app/a8_faults_disabled.go": "9e482887299cc86be720001c2dae51ab24851181",
    "inference-chain/app/a8_faults_enabled.go": "6cea27971df79d9a3348b537fa91c18b6178e425",
    "inference-chain/app/a8faults/plan.go": "19aabed256279a6ad6a098ac2483fd7e4115911f",
    "inference-chain/cmd/a8-query-fault-plan/main.go": "ffca66051acb97c393c1f45481e2fb556780bc33",
}

PR4_MANUAL_WORKFLOW_PATHS = {
    ".github/workflows/build-upgrades.yml",
    ".github/workflows/devshard-testenv.yml",
    ".github/workflows/dont_panic.yml",
    ".github/workflows/integration.yml",
    ".github/workflows/publish_upgrade_binaries.yml",
    ".github/workflows/release.yml",
    ".github/workflows/sanity.yml",
    ".github/workflows/testermint-upgrade-rehearsal.yml",
    ".github/workflows/update_voting.yml",
    ".github/workflows/verify-proto-go-generation.yml",
    ".github/workflows/verify.yml",
}

# These are Git blob IDs, not a broad allowance for `.github`.  PR #4 is the
# reviewed source for the manual-only workflows.  `verify.yml` is exceptional:
# the pinned runtime already contained the P0 fixture job, so its approved
# result is that pinned content with *only* the triggers changed to
# workflow_dispatch (blob 1293e8...).
PR4_MANUAL_WORKFLOW_BLOBS = {
    ".github/workflows/build-upgrades.yml": "5f8316f32abeede05128a932f25243b6d78272c6",
    ".github/workflows/devshard-testenv.yml": "10e60e08be71deac8720354626edb512ab34cd8d",
    ".github/workflows/dont_panic.yml": "3c386c943557159b211c54915a643055f538506b",
    ".github/workflows/integration.yml": "a27cfde56b68457bdd1b217edb0e1f1d530cbe36",
    ".github/workflows/publish_upgrade_binaries.yml": "72caf719b170132228de55b9c226eb4d54ac3670",
    ".github/workflows/release.yml": "09b5b2d887fa4f529d8187d566ac5c0db64a6f29",
    ".github/workflows/sanity.yml": "ef626505348883119648c72693719dd90c1ecf74",
    ".github/workflows/testermint-upgrade-rehearsal.yml": "255bf9698e6529350f10cec026ca6aa2d3add1ca",
    ".github/workflows/update_voting.yml": "1d2eaa50838324855c1583fdd0c5e427156f367c",
    ".github/workflows/verify-proto-go-generation.yml": "07261ed074bac119ab1cb0af97724793b5d32cb0",
    ".github/workflows/verify.yml": "1293e8b533c87cdd6208e20205c822b546b9327b",
}

OVERLAY_ADDITIONAL_GONKA_PATHS = {
    "README_SMART_CONTRACT_TEST.md",
    "inference-chain/app/legacy_test.go",
    "inference-chain/app/wasm_grpc_query_allowlist_test.go",
    "inference-chain/contracts/p0-probe/Cargo.lock",
    "inference-chain/contracts/p0-probe/Cargo.toml",
    "inference-chain/contracts/p0-probe/Makefile",
    "inference-chain/contracts/p0-probe/README.md",
    "inference-chain/contracts/p0-probe/artifacts/checksums.txt",
    "inference-chain/contracts/p0-probe/artifacts/p0_probe.wasm",
    "inference-chain/contracts/p0-probe/build.sh",
    "inference-chain/contracts/p0-probe/src/contract.rs",
    "inference-chain/contracts/p0-probe/src/lib.rs",
    "inference-chain/contracts/p0-probe/src/msg.rs",
    "inference-chain/contracts/p0-probe/src/proto.rs",
}


def verify_gonka_overlay_integrity(overlay_dir: Path) -> dict[str, str]:
    checksum_file = overlay_dir / "CHECKSUMS.sha256"
    if not checksum_file.is_file():
        raise AcceptanceError(f"Overlay checksum manifest missing: {checksum_file}")
    entries: dict[str, str] = {}
    for line in checksum_file.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split(maxsplit=1)
        if len(parts) != 2:
            raise AcceptanceError("Malformed overlay checksum entry")
        digest, rel = parts
        if (len(digest) != 64 or any(c not in "0123456789abcdef" for c in digest)
                or "\\" in rel or ":" in rel or rel.startswith("/")
                or any(part in ("", ".", "..", ".git") for part in rel.split("/"))
                or rel in ("CHECKSUMS.sha256", "README.md") or rel in entries):
            raise AcceptanceError(f"Invalid or duplicate overlay checksum entry: {rel}")
        entries[rel] = digest
    if not entries:
        raise AcceptanceError("Empty overlay checksum manifest")
    actual_files = set()
    for path in overlay_dir.rglob("*"):
        if path.is_symlink():
            raise AcceptanceError(f"Overlay symlink is forbidden: {path}")
        if path.is_file():
            rel = path.relative_to(overlay_dir).as_posix()
            if rel not in ("CHECKSUMS.sha256", "README.md"):
                actual_files.add(rel)
    if actual_files != set(entries):
        raise AcceptanceError("Overlay file inventory differs from checksum manifest")
    for rel, digest in entries.items():
        if sha256_file(overlay_dir / rel) != digest:
            raise AcceptanceError(f"Overlay checksum mismatch for {rel}")
    return entries


def apply_gonka_overlay(overlay_dir: Path, target_dir: Path) -> int:
    entries = verify_gonka_overlay_integrity(overlay_dir)
    for rel, digest in sorted(entries.items()):
        dst_file = target_dir / rel
        if not dst_file.resolve().is_relative_to(target_dir.resolve()):
            raise AcceptanceError(f"Overlay destination escapes workspace: {rel}")
        dst_file.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(overlay_dir / rel, dst_file)
        if sha256_file(dst_file) != digest:
            raise AcceptanceError(f"Copied overlay checksum mismatch: {rel}")
        if dst_file.suffix == ".sh" or dst_file.name == "gradlew":
            dst_file.chmod(0o755)
    return len(entries)


def prepare_temporary_gonka_workspace(
    base_gonka_dir: Path,
    overlay_dir: Path,
    runner: Runner,
    target_dir: Path | None = None,
    base_sha: str = EXPECTED_GONKA_BASE_SHA,
    keep_temp: bool = False,
) -> tuple[Path, Any]:
    verify_gonka_overlay_integrity(overlay_dir)
    base_gonka_dir = base_gonka_dir.resolve()
    if not base_gonka_dir.is_dir():
        raise AcceptanceError(f"Base Gonka directory does not exist: {base_gonka_dir}")

    rev = runner.run(["git", "-c", "safe.bareRepository=all", "-C", str(base_gonka_dir), "rev-parse", "HEAD"], timeout=30)
    if rev.returncode != 0:
        raise AcceptanceError(
            rev.stderr.strip() or f"Cannot resolve git HEAD for base Gonka directory: {base_gonka_dir}"
        )
    base_head = rev.stdout.strip()

    cleanup_obj = None
    if target_dir is None and keep_temp:
        workspace = Path(tempfile.mkdtemp(prefix="gonka-a8-workspace-")).resolve()
    elif target_dir is None:
        cleanup_obj = tempfile.TemporaryDirectory(prefix="gonka-a8-workspace-")
        workspace = Path(cleanup_obj.name).resolve()
    else:
        workspace = target_dir.resolve()
        workspace.mkdir(parents=True, exist_ok=True)

    def checked(command: list[str], timeout: int) -> subprocess.CompletedProcess:
        result = runner.run(command, timeout=timeout)
        if result.returncode != 0:
            raise AcceptanceError(result.stderr.strip() or f"Workspace preparation failed: {command}")
        return result

    try:
        checked(["git", "-c", "safe.bareRepository=all", "clone", "-s",
                 str(base_gonka_dir), str(workspace)], 180)
        checked(["git", "-C", str(workspace), "checkout", "--detach", base_sha], 60)
        actual = checked(["git", "-C", str(workspace), "rev-parse", "HEAD"], 30)
        if actual.stdout.strip() != base_sha:
            raise AcceptanceError("Prepared Gonka HEAD differs from pinned base SHA")
        apply_gonka_overlay(overlay_dir, workspace)
        checked(["git", "-C", str(workspace), "config", "user.name", "Acceptance Runner"], 10)
        checked(["git", "-C", str(workspace), "config", "user.email", "runner@test.local"], 10)
        checked(["git", "-C", str(workspace), "add", "-A"], 60)
        checked(["git", "-C", str(workspace), "commit", "-m",
                 "test(a8): apply marketplace test overlay"], 30)
    except BaseException:
        if cleanup_obj is not None:
            cleanup_obj.cleanup()
        raise

    return workspace, cleanup_obj


def verify_gonka_test_checkout(
    gonka_dir: Path, runner: Runner, base_sha: str = EXPECTED_GONKA_SHA
) -> str:
    head = runner.run(
        ["git", "-C", str(gonka_dir), "rev-parse", "HEAD"], timeout=30
    )
    if head.returncode != 0:
        raise AcceptanceError(head.stderr.strip() or "cannot resolve Gonka checkout")
    actual = head.stdout.strip()
    if actual in (base_sha, EXPECTED_GONKA_SHA, EXPECTED_GONKA_BASE_SHA):
        return actual
    diff = runner.run(
        [
            "git",
            "-C",
            str(gonka_dir),
            "diff",
            "--name-only",
            f"{base_sha}..{actual}",
        ],
        timeout=30,
    )
    allowed = (
        B3_HARNESS_GONKA_PATHS
        | PR4_MANUAL_WORKFLOW_PATHS
        | set(A8_RUNTIME_ADAPTER_BLOBS)
        | OVERLAY_ADDITIONAL_GONKA_PATHS
    )
    changed = {line.strip().replace("\\", "/") for line in diff.stdout.splitlines() if line.strip()}
    if diff.returncode != 0 or not changed or not changed.issubset(allowed):
        raise AcceptanceError(
            f"Gonka checkout must be {EXPECTED_GONKA_SHA} or test-only child; "
            f"HEAD={actual}, changed={sorted(changed)}"
        )
    if changed & set(A8_RUNTIME_ADAPTER_BLOBS):
        for path, expected_blob in A8_RUNTIME_ADAPTER_BLOBS.items():
            blob = runner.run(
                ["git", "-C", str(gonka_dir), "rev-parse", f"{actual}:{path}"], timeout=30
            )
            if blob.returncode != 0 or blob.stdout.strip() != expected_blob:
                raise AcceptanceError(f"unreviewed A8 runtime adapter blob: {path}")
    changed_workflows = changed & PR4_MANUAL_WORKFLOW_PATHS
    if changed_workflows:
        pr4_ancestor = runner.run(
            ["git", "-C", str(gonka_dir), "merge-base", "--is-ancestor", PR4_MANUAL_WORKFLOWS_SHA, actual],
            timeout=30,
        )
        workflow_blobs: dict[str, str] = {}
        for workflow in sorted(PR4_MANUAL_WORKFLOW_PATHS):
            blob = runner.run(
                ["git", "-C", str(gonka_dir), "rev-parse", f"{actual}:{workflow}"],
                timeout=30,
            )
            workflow_blobs[workflow] = blob.stdout.strip()
            if blob.returncode != 0:
                break
        mismatched_workflows = {
            workflow: {"expected": PR4_MANUAL_WORKFLOW_BLOBS[workflow], "actual": blob}
            for workflow, blob in workflow_blobs.items()
            if blob != PR4_MANUAL_WORKFLOW_BLOBS[workflow]
        }
        if pr4_ancestor.returncode != 0 or len(workflow_blobs) != len(PR4_MANUAL_WORKFLOW_PATHS) or mismatched_workflows:
            raise AcceptanceError(
                "Gonka checkout CI workflows must match pinned PR #4 blobs, with "
                "the approved P0-preserving manual verify.yml exception; "
                f"HEAD={actual}, changed_workflows={sorted(changed_workflows)}, "
                f"mismatched_workflows={compact_json(mismatched_workflows)}"
            )
    return actual


def git_head(repo: Path, runner: Runner) -> str:
    result = runner.run(["git", "-C", str(repo), "rev-parse", "HEAD"], timeout=30)
    value = result.stdout.strip()
    if result.returncode != 0 or len(value) != 40:
        raise AcceptanceError(result.stderr.strip() or "cannot resolve Marketplace HEAD")
    return value


def verified_release_artifacts(
    repo: Path, manifest_path: Path, runner: Runner
) -> tuple[Path, Path, dict[str, Any]]:
    manifest_path = manifest_path.resolve()
    verifier = runner.run(
        [
            sys.executable,
            str(repo / "scripts" / "a9_release.py"),
            "verify-artifacts",
            "--manifest",
            str(manifest_path),
        ],
        timeout=120,
    )
    if verifier.returncode != 0:
        raise AcceptanceError(
            verifier.stderr.strip() or "A9 manifest/artifact verification failed"
        )
    manifest = load_object(manifest_path)
    head = git_head(repo, runner)
    if manifest.get("marketplace_commit_sha") != head:
        raise AcceptanceError(
            "A9 manifest is stale: "
            f"manifest={manifest.get('marketplace_commit_sha')!r}, HEAD={head}"
        )
    contracts = manifest.get("contracts")
    if not isinstance(contracts, list):
        raise AcceptanceError("A9 manifest contracts must be a list")
    by_name = {
        item.get("name"): item
        for item in contracts
        if isinstance(item, dict) and isinstance(item.get("name"), str)
    }
    paths: dict[str, Path] = {}
    for name in ("marketplace-deal", "marketplace-factory"):
        item = by_name.get(name)
        if not isinstance(item, dict) or not isinstance(item.get("path"), str):
            raise AcceptanceError(f"A9 manifest missing contract {name}")
        artifact = (manifest_path.parent / item["path"]).resolve()
        if sha256_file(artifact) != item.get("sha256"):
            raise AcceptanceError(f"A9 artifact hash mismatch after verification: {name}")
        paths[name] = artifact
    return paths["marketplace-deal"], paths["marketplace-factory"], manifest


def run_live(args: argparse.Namespace) -> None:
    runner = Runner()
    repo = Path(args.marketplace_dir).resolve()
    base_gonka_dir = Path(args.gonka_dir).resolve()
    overlay_dir = Path(getattr(args, "overlay_dir", None) or DEFAULT_GONKA_OVERLAY_DIR).resolve()

    use_temp = not getattr(args, "no_temp_gonka", False)
    keep_temp = getattr(args, "keep_temp_gonka", False)
    temp_dir_arg = getattr(args, "temp_gonka_dir", None)

    cleanup_obj = None
    if use_temp:
        target_ws = Path(temp_dir_arg).resolve() if temp_dir_arg else None
        gonka_dir, cleanup_obj = prepare_temporary_gonka_workspace(
            base_gonka_dir,
            overlay_dir,
            runner,
            target_dir=target_ws,
            keep_temp=keep_temp,
        )
    else:
        gonka_dir = base_gonka_dir

    test_head = verify_gonka_test_checkout(
        gonka_dir,
        runner,
        base_sha=EXPECTED_GONKA_BASE_SHA if use_temp else EXPECTED_GONKA_SHA,
    )
    dirty = runner.run(
        ["git", "-C", str(gonka_dir), "status", "--porcelain=v1"], timeout=30
    )
    if dirty.returncode != 0 or dirty.stdout.strip():
        raise AcceptanceError(
            "Gonka test checkout must be clean before Testermint reboot:\n"
            + dirty.stdout.rstrip()
        )
    assert_fresh_gonka_local_state(gonka_dir)

    collisions = docker_resource_collisions(runner)
    if any(collisions.values()):
        raise AcceptanceError(
            "refusing destructive Testermint reboot because owned Docker resources "
            "already exist: "
            + compact_json(collisions)
        )

    run_id = args.run_id or dt.datetime.now().strftime("%Y%m%d%H%M%S")
    evidence_dir = Path(args.evidence_dir).resolve() / run_id
    evidence_dir.mkdir(parents=True, exist_ok=False)
    context = evidence_dir / "live-context.json"

    marketplace_dirty = runner.run(
        ["git", "-C", str(repo), "status", "--porcelain=v1", "--untracked-files=no"],
        timeout=30,
    )
    if marketplace_dirty.returncode != 0 or marketplace_dirty.stdout.strip():
        raise AcceptanceError("Marketplace checkout must be clean before A9 provenance")
    if args.manifest:
        manifest_path = Path(args.manifest).resolve()
    else:
        release_dir = evidence_dir / "a9-release"
        build = runner.run(
            [
                sys.executable,
                str(repo / "scripts" / "a9_release.py"),
                "build",
                "--commit",
                "HEAD",
                "--output",
                str(release_dir),
            ],
            timeout=30 * 60,
        )
        if build.returncode != 0:
            raise AcceptanceError(build.stderr.strip() or "A9 release build failed")
        manifest_path = release_dir / "build-manifest.json"
    deal, factory, manifest = verified_release_artifacts(repo, manifest_path, runner)

    test_target = evidence_dir / "test-wasm-target"
    test_build = runner.run(
        [
            "cargo",
            "build",
            "-p",
            "a8-caller",
            "-p",
            "a8-cw20",
            "--release",
            "--target",
            "wasm32-unknown-unknown",
            "--target-dir",
            str(test_target),
            "--locked",
        ],
        timeout=10 * 60,
    )
    if test_build.returncode != 0:
        raise AcceptanceError(
            test_build.stderr.strip() or "cannot build fresh test-only Wasm artifacts"
        )
    test_artifacts = test_target / "wasm32-unknown-unknown" / "release"
    caller = test_artifacts / "a8_caller.wasm"
    cw20 = test_artifacts / "a8_cw20.wasm"
    for artifact in (deal, factory, caller, cw20):
        if not artifact.is_file():
            raise AcceptanceError(f"required verified Wasm artifact missing: {artifact}")

    gradle = gonka_dir / "testermint" / "gradlew"
    c_binary = None
    if args.scenario == "package-c-query-faults":
        if os.name == "nt":
            raise AcceptanceError("C must use the Linux snapshot launcher")
        export = evidence_dir / "c-runtime-export"
        if export.exists():
            raise AcceptanceError("C runtime exporter destination already exists")
        build_command = [
            "docker", "build", "--file", str(gonka_dir / "inference-chain/Dockerfile"),
            "--target", "binary-exporter", "--build-arg", "TAGS=a8faults",
            "--output", f"type=local,dest={export}", str(gonka_dir),
        ]
        # Build before starting Testermint. A failure here is infrastructure,
        # not evidence that a blockchain case ran.
        build_result = runner.run(build_command, timeout=20 * 60)
        (evidence_dir / "c-runtime-build.log").write_text(
            build_result.stdout + build_result.stderr, encoding="utf-8")
        c_binary = export / "build_output/inferenced"
        if build_result.returncode != 0 or not c_binary.is_file():
            raise AcceptanceError("C tagged binary build failed; see c-runtime-build.log")
        c_binary.chmod(0o755)
        write_object(evidence_dir / "c-runtime-build.json", {
            "level": "NATIVE-FAULT", "production_evidence": False,
            "gonka_source_sha": test_head, "command": build_command,
            "binary_sha256": sha256_file(c_binary), "build_tag": "a8faults",
            "runtime_adapter_blobs": A8_RUNTIME_ADAPTER_BLOBS,
        })
    wrapper = gonka_dir / "testermint" / "gradlew.a8-wsl"
    if wrapper.exists():
        raise AcceptanceError(f"temporary Gradle wrapper already exists: {wrapper}")
    wrapper.write_bytes(gradle.read_bytes().replace(b"\r\n", b"\n"))
    try:
        wsl_gonka = wsl_path(gonka_dir, runner)
        wsl_repo = wsl_path(repo, runner)
        wsl_context = wsl_path(context, runner)
        wsl_git_dir = wsl_path(
            Path(runner.run(
                ["git", "-C", str(gonka_dir), "rev-parse", "--absolute-git-dir"],
                timeout=30,
            ).stdout.strip()),
            runner,
        )
        env = [
            f"GIT_DIR={wsl_git_dir}",
            f"GIT_WORK_TREE={wsl_gonka}",
            "A8_PYTHON=/usr/bin/python3",
            f"A8_HARNESS={wsl_repo}/scripts/a8_acceptance.py",
            f"A8_MARKETPLACE_DIR={wsl_repo}",
            f"A8_GONKA_DIR={wsl_gonka}",
            f"A8_CONTEXT={wsl_context}",
            f"A8_RUN_ID={run_id}",
            f"A8_DEAL_WASM={wsl_path(deal, runner)}",
            f"A8_FACTORY_WASM={wsl_path(factory, runner)}",
            f"A8_CW20_WASM={wsl_path(cw20, runner)}",
            f"A8_CALLER_WASM={wsl_path(caller, runner)}",
        ]
        if c_binary is not None:
            env.append(f"A8_C_BINARY={wsl_path(c_binary, runner)}")
        test_name = {
            "package-c-query-faults": (
                "MarketplaceContractAcceptanceTests.marketplace package C proves query faults and recovery"
            ),
            "full": (
                "MarketplaceContractAcceptanceTests.marketplace funded claim "
                "settles and releases on real Gonka"
            ),
            "claim-expiry-positive": (
                "MarketplaceContractAcceptanceTests.marketplace positive unclaimed "
                "summary refunds only at claim expiry"
            ),
            "claim-expiry-zero": (
                "MarketplaceContractAcceptanceTests.marketplace zero unclaimed "
                "summary refunds only at claim expiry"
            ),
            "network-unconfirmed": (
                "MarketplaceContractAcceptanceTests.marketplace absent native summary "
                "refunds only at emergency deadline"
            ),
            "terminal-release-repeat": (
                "MarketplaceContractAcceptanceTests.marketplace terminal release repeat "
                "is a native no-op"
            ),
            "b3-foreign-native": (
                "MarketplaceContractAcceptanceTests.marketplace successful release "
                "preserves foreign native denom"
            ),
            "late-donation-after-completed": (
                "MarketplaceContractAcceptanceTests.marketplace late liquid donations "
                "after Completed use cumulative GNK rounding"
            ),
            "lock-exact-e": ("MarketplaceContractAcceptanceTests.marketplace funded lock succeeds exactly at E"),
            "lock-e-plus-4": (
                "MarketplaceContractAcceptanceTests.marketplace funded lock succeeds "
                "exactly at E plus 4"
            ),
            "lock-e-plus-5": (
                "MarketplaceContractAcceptanceTests.marketplace funded lock rejects "
                "exactly at E plus 5"
            ),
            "package-a-r1-r2": (
                "MarketplaceContractAcceptanceTests.marketplace package A preserves R1 refund "
                "boundary and releases a new vested gift"
            ),
            "package-b-r6-1": (
                "MarketplaceContractAcceptanceTests.marketplace R6 dot 1 rejects all three "
                "selected CW20 sends then settles once"
            ),
            "package-b-r7-1": (
                "MarketplaceContractAcceptanceTests.marketplace R7 dot 1 rejects selected "
                "second Bank send then retries once"
            ),
        }[args.scenario]
        command = [
            *(["wsl.exe", "-d", "Ubuntu", "--"] if os.name == "nt" else []),
            "env",
            *env,
            "bash",
            "-lc",
            f"cd {wsl_gonka}/testermint && chmod +x gradlew.a8-wsl && "
            "./gradlew.a8-wsl :test --tests "
            f"'{test_name}' "
            "-DexcludeTags=unstable,exclude",
        ]
        result = runner.run(command, timeout=args.timeout_minutes * 60)
        log_path = evidence_dir / "testermint.log"
        # The dedicated test and harness never print key material. Keep the
        # bounded runner output for failure diagnostics and reproducibility.
        log_path.write_text(result.stdout + result.stderr, encoding="utf-8")
        if context.is_file():
            context_value = load_object(context)
            context_value["source"]["gonka_test_harness_sha"] = test_head
            context_value["source"]["marketplace_commit_sha"] = manifest[
                "marketplace_commit_sha"
            ]
            context_value["source"]["a9_manifest_sha256"] = sha256_file(manifest_path)
            context_value["source"]["a9_contract_sha256"] = {
                "deal": sha256_file(deal),
                "factory": sha256_file(factory),
            }
            context_value["source"]["test_contract_sha256"] = {
                "caller": sha256_file(caller),
                "cw20": sha256_file(cw20),
            }
            context_value["command"] = {
                "entrypoint": "python scripts/a8_acceptance.py run-live",
                "scenario": args.scenario,
                "test": test_name,
            }
            write_object(context, context_value)
        if result.returncode != 0:
            raise AcceptanceError(
                f"Testermint marketplace scenario failed; see {log_path}"
            )
        print(compact_json({"status": "pass", "evidence": str(evidence_dir)}))
    finally:
        if wrapper is not None:
            wrapper.unlink(missing_ok=True)
        if cleanup_obj is not None:
            cleanup_obj.cleanup()


def assert_chain(gonka: DockerGonka) -> dict[str, Any]:
    status = gonka.status()
    node_info = status.get("node_info")
    if not isinstance(node_info, dict):
        node_info = status.get("NodeInfo")
    chain_id = node_info.get("network") if isinstance(node_info, dict) else None
    if chain_id != gonka.chain_id:
        raise AcceptanceError(f"expected chain ID {gonka.chain_id}, got {chain_id!r}")
    sync = status.get("sync_info")
    if isinstance(sync, dict) and sync.get("catching_up") not in (False, "false"):
        raise AcceptanceError("local Gonka node is still catching up")
    return status


def bootstrap(args: argparse.Namespace) -> None:
    gonka = DockerGonka(Runner(), args.chain_id)
    status = assert_chain(gonka)
    runtime_identity = parse_runtime_identity(gonka.binary_version())
    restrictions = gonka.transfer_restriction_status()
    if restrictions["is_active"] or restrictions["restriction_end_block"] != 0:
        raise AcceptanceError(
            "local acceptance genesis must set restrictions.restriction_end_block to 0"
        )
    native_test_configuration: dict[str, Any] | None = None
    if args.expected_initial_epoch_reward is not None:
        params_query = gonka.query_json("inference", "params")
        params = params_query.get("params")
        if not isinstance(params, Mapping):
            raise AcceptanceError(f"native params query has no params object: {params_query}")
        bitcoin_params = params.get("bitcoin_reward_params")
        if not isinstance(bitcoin_params, Mapping):
            raise AcceptanceError(
                f"native params query has no bitcoin_reward_params: {params_query}"
            )
        actual_initial_epoch_reward = require_uint(
            bitcoin_params.get("initial_epoch_reward", 0),
            "native bitcoin initial_epoch_reward",
        )
        if actual_initial_epoch_reward != args.expected_initial_epoch_reward:
            raise AcceptanceError(
                "native bitcoin initial_epoch_reward mismatch: "
                f"expected {args.expected_initial_epoch_reward}, "
                f"got {actual_initial_epoch_reward}"
            )
        native_test_configuration = {
            "scope": "special_test_genesis_only",
            "production_reachability_claimed": False,
            "changed_genesis_parameters": {
                "inference.params.bitcoin_reward_params.initial_epoch_reward": (
                    actual_initial_epoch_reward
                )
            },
            "validation": {
                "genesis_accepted_by_native_binary": True,
                "queried_value_matches_expected": True,
                "protobuf_field_present": "initial_epoch_reward" in bitcoin_params,
                "protobuf_omitted_zero_decoded_as_zero": (
                    "initial_epoch_reward" not in bitcoin_params
                    and actual_initial_epoch_reward == 0
                ),
            },
            "params_query": params_query,
        }
    run_id = args.run_id
    host = gonka.key_address(args.host_node, args.host_key)
    buyer_key = args.buyer_key
    fee_key = f"a8-fee-{run_id}"
    inactive_key = f"a8-inactive-{run_id}"
    buyer = gonka.key_address(args.buyer_node, buyer_key)
    fee = gonka.create_key(fee_key)
    inactive_host = gonka.create_key(inactive_key)
    genesis_address = gonka.key_address(DEFAULT_NODE, "genesis")
    inactive_funding_tx = gonka.tx(
        DEFAULT_NODE,
        "genesis",
        "bank",
        "send",
        genesis_address,
        inactive_host,
        f"100000000{DEFAULT_DENOM}",
        gas="auto",
    )

    artifacts = {
        "deal": Path(args.deal_wasm),
        "factory": Path(args.factory_wasm),
        "cw20": Path(args.cw20_wasm),
        "caller": Path(args.caller_wasm),
    }
    stores: dict[str, Any] = {}
    code_ids: dict[str, str] = {}
    for label, path in artifacts.items():
        code_id, evidence = gonka.store(path, label)
        code_ids[label] = code_id
        stores[label] = evidence

    cw20, cw20_evidence = gonka.instantiate(
        code_ids["cw20"],
        {
            "decimals": 6,
            "initial_balances": [
                {"address": buyer, "amount": str(args.buyer_tokens)}
            ],
            "marketing": None,
            "mint": None,
            "name": "A8 Local Test USDT",
            "symbol": TEST_CW20_SYMBOL,
        },
        f"a8-cw20-{run_id}",
    )
    token_info = gonka.smart(cw20, {"token_info": {}})
    if token_info.get("decimals") != 6:
        raise AcceptanceError(f"test CW20 decimals mismatch: {token_info}")
    foreign_cw20, foreign_cw20_evidence = gonka.instantiate(
        code_ids["cw20"],
        {
            "decimals": 6,
            "initial_balances": [{"address": buyer, "amount": "1000000"}],
            "marketing": None,
            "mint": None,
            "name": "A8 Foreign Asset",
            "symbol": "AFRGN",
        },
        f"a8-foreign-cw20-{run_id}",
    )

    factory, factory_evidence = gonka.instantiate(
        code_ids["factory"],
        {
            "deal_code_id": int(code_ids["deal"]),
            "fee_bps": PROTOCOL_FEE_BPS,
            "fee_recipient": fee,
            "settlement_cw20": cw20,
        },
        f"a8-factory-{run_id}",
    )
    caller, caller_evidence = gonka.instantiate(
        code_ids["caller"], {}, f"a8-caller-{run_id}"
    )

    offer_tx = gonka.execute(
        args.host_node,
        args.host_key,
        factory,
        {
            "create_offer": {
                "buyer_budget_micro_usdt": str(args.budget),
                "price_micro_usdt_per_gnk": str(args.price),
                "target_epoch": args.target_epoch,
            }
        },
        gas="auto",
    )
    deal_row = gonka.smart(
        factory,
        {"deal_by_host_epoch": {"epoch": args.target_epoch, "host": host}},
    )
    deal = deal_row.get("address")
    if not isinstance(deal, str):
        raise AcceptanceError(f"Factory index did not return Deal address: {deal_row}")
    deal_info = gonka.query_json("wasm", "contract", deal)
    nested_info = (
        deal_info.get("contract_info")
        if isinstance(deal_info.get("contract_info"), dict)
        else deal_info
    )
    if nested_info.get("admin") not in (None, ""):
        raise AcceptanceError("Factory-created Deal unexpectedly has admin")
    deal_config = gonka.smart(deal, {"config": {}})
    if args.price <= 0:
        raise AcceptanceError("acceptance price must be positive")
    expected_deal_config = {
        "factory": factory,
        "host": host,
        "deal_address": deal,
        "target_epoch": args.target_epoch,
        "price_micro_usdt_per_gnk": args.price,
        "buyer_budget_micro_usdt": args.budget,
        "funded_capacity_ngonka": args.budget * GNK_SCALE // args.price,
        "settlement_cw20": cw20,
        "fee_recipient": fee,
        "fee_bps": PROTOCOL_FEE_BPS,
        "pinned_gonka_sha": EXPECTED_PROTO_SHA,
    }
    assert_deal_terms(deal_config, expected_deal_config)

    recipient_tx = gonka.tx(
        args.host_node,
        args.host_key,
        "inference",
        "set-claim-recipients",
        compact_json([{"epoch": args.target_epoch, "recipient": deal}]),
        gas="auto",
    )
    recipients = gonka.query_json("inference", "list-claim-recipients", host)
    entries = recipients.get("entries")
    if not isinstance(entries, list) or not any(
        str(item.get("epoch")) == str(args.target_epoch)
        and item.get("recipient") == deal
        for item in entries
        if isinstance(item, dict)
    ):
        raise AcceptanceError(f"native recipient schedule missing exact Deal: {recipients}")

    hook = base64.b64encode(compact_json({"fund": {}}).encode()).decode()
    balances_before = {
        "buyer": gonka.cw20_balance(cw20, buyer),
        "deal": gonka.cw20_balance(cw20, deal),
    }
    fund_tx = gonka.execute(
        args.buyer_node,
        buyer_key,
        cw20,
        {
            "send": {
                "amount": str(args.budget),
                "contract": deal,
                "msg": hook,
            }
        },
        gas="auto",
    )
    balances_after = {
        "buyer": gonka.cw20_balance(cw20, buyer),
        "deal": gonka.cw20_balance(cw20, deal),
    }
    if balances_before["buyer"] - balances_after["buyer"] != args.budget:
        raise AcceptanceError("Buyer CW20 delta does not equal exact budget")
    if balances_after["deal"] - balances_before["deal"] != args.budget:
        raise AcceptanceError("Deal CW20 delta does not equal exact budget")
    state = gonka.smart(deal, {"state": {}})
    if state.get("status") != "funded" or state.get("buyer") != buyer:
        raise AcceptanceError(f"Deal did not enter exact Funded state: {state}")

    evidence = {
        "schema_version": "1.0.0",
        "kind": "gonka-marketplace-a8-live-context",
        "created_at_utc": utc_now(),
        "run_id": run_id,
        "level": "live_network",
        "source": {
            "gonka_sha": EXPECTED_GONKA_SHA,
            "protobuf_sha": EXPECTED_PROTO_SHA,
            "runtime": runtime_identity,
            **(
                {"native_test_configuration": native_test_configuration}
                if native_test_configuration is not None
                else {}
            ),
        },
        "chain": {
            "chain_id": args.chain_id,
            "status": status,
            "transfer_restrictions": restrictions,
        },
        "terms": {
            "target_epoch": args.target_epoch,
            "budget_micro_usdt": str(args.budget),
            "price_micro_usdt_per_gnk": str(args.price),
            "fee_bps": PROTOCOL_FEE_BPS,
        },
        "accounts": {
            "host": host,
            "buyer": buyer,
            "fee_recipient": fee,
            "inactive_host": inactive_host,
        },
        "key_names": {
            "host": args.host_key,
            "host_node": args.host_node,
            "buyer": buyer_key,
            "buyer_node": args.buyer_node,
            "inactive_host": inactive_key,
            "inactive_host_node": DEFAULT_NODE,
        },
        "contracts": {
            "cw20": cw20,
            "foreign_cw20": foreign_cw20,
            "factory": factory,
            "deal": deal,
            "caller": caller,
        },
        "code_ids": code_ids,
        "stores": stores,
        "deployments": {
            "cw20": cw20_evidence,
            "foreign_cw20": foreign_cw20_evidence,
            "factory": factory_evidence,
            "caller": caller_evidence,
            "deal": {"contract_info": deal_info, "config": deal_config},
        },
        "bootstrap": {
            "offer_tx": filtered_tx(offer_tx),
            "recipient_tx": filtered_tx(recipient_tx),
            "recipient_query": recipients,
            "fund_tx": filtered_tx(fund_tx),
            "inactive_host_funding_tx": filtered_tx(inactive_funding_tx),
            "cw20_before": balances_before,
            "cw20_after": balances_after,
            "deal_state": state,
        },
        "phases": [],
    }
    write_object(Path(args.context), evidence)
    print(compact_json({"context": str(Path(args.context)), "deal": deal}))


def append_phase(path: Path, phase: Mapping[str, Any]) -> dict[str, Any]:
    context = load_object(path)
    phases = context.setdefault("phases", [])
    if not isinstance(phases, list):
        raise AcceptanceError("context phases must be a list")
    phases.append(dict(phase))
    write_object(path, context)
    return context


def scenario_record(context: Mapping[str, Any], name: str) -> dict[str, Any]:
    scenarios = context.get("scenarios")
    if not isinstance(scenarios, Mapping) or not isinstance(scenarios.get(name), dict):
        raise AcceptanceError(f"unknown acceptance scenario: {name}")
    return dict(scenarios[name])


def named_scenario_or_bootstrap(context: Mapping[str, Any], name: str) -> Mapping[str, Any]:
    """Use the primary bootstrap Deal only when the caller names it explicitly."""
    if name == "bootstrap":
        return context
    return scenario_record(context, name)


def append_named_phase(context: dict[str, Any], name: str, phase: Mapping[str, Any]) -> None:
    phases = context.setdefault("phases", []) if name == "bootstrap" else context["scenarios"][name]["phases"]
    if not isinstance(phases, list):
        raise AcceptanceError("selected scenario phases must be a list")
    phases.append(dict(phase))


def create_deal(args: argparse.Namespace) -> None:
    path = Path(args.context)
    context = load_object(path)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    scenarios = context.setdefault("scenarios", {})
    if not isinstance(scenarios, dict):
        raise AcceptanceError("context scenarios must be an object")
    if args.name in scenarios:
        raise AcceptanceError(f"acceptance scenario already exists: {args.name}")

    host = gonka.key_address(args.host_node, args.host_key)
    buyer = context["accounts"]["buyer"]
    factory = context["contracts"]["factory"]
    cw20 = context["contracts"]["cw20"]
    offer_tx = gonka.execute(
        args.host_node,
        args.host_key,
        factory,
        {
            "create_offer": {
                "buyer_budget_micro_usdt": str(args.budget),
                "price_micro_usdt_per_gnk": str(args.price),
                "target_epoch": args.target_epoch,
            }
        },
        gas="auto",
    )
    row = gonka.smart(
        factory,
        {"deal_by_host_epoch": {"epoch": args.target_epoch, "host": host}},
    )
    deal = row.get("address")
    if not isinstance(deal, str):
        raise AcceptanceError(f"Factory index did not return scenario Deal: {row}")
    config = gonka.smart(deal, {"config": {}})
    assert_deal_terms(
        config,
        {
            "factory": factory,
            "host": host,
            "deal_address": deal,
            "target_epoch": args.target_epoch,
            "price_micro_usdt_per_gnk": args.price,
            "buyer_budget_micro_usdt": args.budget,
            "funded_capacity_ngonka": args.budget * GNK_SCALE // args.price,
            "settlement_cw20": cw20,
            "fee_recipient": context["accounts"]["fee_recipient"],
            "fee_bps": PROTOCOL_FEE_BPS,
            "pinned_gonka_sha": EXPECTED_PROTO_SHA,
        },
    )

    recipient_tx = None
    if args.route_exact:
        recipient_tx = gonka.tx(
            args.host_node,
            args.host_key,
            "inference",
            "set-claim-recipients",
            compact_json([{"epoch": args.target_epoch, "recipient": deal}]),
            gas="auto",
        )
    fund_tx = None
    balances_before = None
    balances_after = None
    if args.fund:
        if not args.route_exact:
            raise AcceptanceError("funded scenario preparation requires exact routing")
        balances_before = {
            "buyer": gonka.cw20_balance(cw20, buyer),
            "deal": gonka.cw20_balance(cw20, deal),
        }
        hook = base64.b64encode(compact_json({"fund": {}}).encode()).decode()
        fund_tx = gonka.execute(
            context["key_names"]["buyer_node"],
            context["key_names"]["buyer"],
            cw20,
            {"send": {"amount": str(args.budget), "contract": deal, "msg": hook}},
            gas="auto",
        )
        balances_after = {
            "buyer": gonka.cw20_balance(cw20, buyer),
            "deal": gonka.cw20_balance(cw20, deal),
        }
        if balances_before["buyer"] - balances_after["buyer"] != args.budget:
            raise AcceptanceError("scenario Buyer funding delta differs from budget")
        if balances_after["deal"] - balances_before["deal"] != args.budget:
            raise AcceptanceError("scenario Deal funding delta differs from budget")

    state = gonka.smart(deal, {"state": {}})
    expected_status = "funded" if args.fund else "open"
    if state.get("status") != expected_status:
        raise AcceptanceError(f"scenario Deal state mismatch: {state}")
    scenario = {
        "name": args.name,
        "terms": {
            "target_epoch": args.target_epoch,
            "budget_micro_usdt": str(args.budget),
            "price_micro_usdt_per_gnk": str(args.price),
            "fee_bps": PROTOCOL_FEE_BPS,
        },
        "accounts": {
            "host": host,
            "buyer": buyer if args.fund else None,
            "fee_recipient": context["accounts"]["fee_recipient"],
        },
        "key_names": {"host": args.host_key, "host_node": args.host_node},
        "contracts": {"deal": deal, "factory": factory, "cw20": cw20},
        "prepared": {
            "offer_tx": filtered_tx(offer_tx),
            "recipient_tx": filtered_tx(recipient_tx) if recipient_tx else None,
            "fund_tx": filtered_tx(fund_tx) if fund_tx else None,
            "cw20_before": balances_before,
            "cw20_after": balances_after,
            "deal_config": config,
            "deal_state": state,
        },
        "phases": [],
    }
    scenarios[args.name] = scenario
    write_object(path, context)
    print(compact_json({"scenario": args.name, "deal": deal, "status": expected_status}))


def set_scenario_routing(args: argparse.Namespace) -> None:
    path = Path(args.context)
    context = load_object(path)
    scenario = scenario_record(context, args.name)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    if args.recipient == "missing":
        recipient = ""
    elif args.recipient == "buyer":
        recipient = context["accounts"]["buyer"]
    elif args.recipient == "caller":
        recipient = context["contracts"]["caller"]
    else:
        raise AcceptanceError(f"unsupported scenario recipient: {args.recipient}")
    tx = gonka.tx(
        scenario["key_names"]["host_node"],
        scenario["key_names"]["host"],
        "inference",
        "set-claim-recipients",
        compact_json(
            [{"epoch": scenario["terms"]["target_epoch"], "recipient": recipient}]
        ),
        gas="auto",
    )
    recipients = gonka.query_json(
        "inference", "list-claim-recipients", scenario["accounts"]["host"]
    )
    phase = {
        "name": "routing_mutation",
        "recipient_kind": args.recipient,
        "recipient": recipient,
        "tx": filtered_tx(tx),
        "recipient_query": recipients,
    }
    context["scenarios"][args.name]["phases"].append(phase)
    write_object(path, context)
    print(compact_json({"scenario": args.name, "recipient": args.recipient}))


def lock_scenario(args: argparse.Namespace) -> None:
    path = Path(args.context)
    context = load_object(path)
    scenario = scenario_record(context, args.name)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    deal = scenario["contracts"]["deal"]
    recipients = gonka.query_json(
        "inference", "list-claim-recipients", scenario["accounts"]["host"]
    )
    before = gonka.smart(deal, {"state": {}})
    tx = gonka.execute(DEFAULT_NODE, "genesis", deal, {"lock": {}}, gas="auto")
    after = gonka.smart(deal, {"state": {}})
    if before.get("status") not in ("open", "funded") or after.get("status") != "locked":
        raise AcceptanceError(f"scenario Lock transition mismatch: {before} -> {after}")
    if after.get("recipient_locked") is not True:
        raise AcceptanceError("scenario Lock did not persist routing proof")
    context["scenarios"][args.name]["phases"].append(
        {
            "name": "lock",
            "tx": filtered_tx(tx),
            "recipient_query": recipients,
            "state_before": before,
            "state_after": after,
        }
    )
    write_object(path, context)
    print(compact_json({"scenario": args.name, "status": "locked"}))


def lock_exact_e_scenario(args: argparse.Namespace) -> None:
    lock_e_plus_4_scenario(args, offset=0)


def lock_e_plus_4_scenario(args: argparse.Namespace, offset: int = 4) -> None:
    """Prove a funded permissionless Lock was included in precisely E + 4."""
    path = Path(args.context)
    context = load_object(path)
    scenario = scenario_record(context, args.name)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)

    target_epoch = require_uint(scenario["terms"]["target_epoch"], "scenario epoch")
    if offset not in (0, 4):
        raise AcceptanceError("unsupported Lock boundary")
    expected_epoch = target_epoch + offset
    if offset == 0:
        deadline = time.monotonic() + 600
        while True:
            observed = epoch_observation(gonka)
            if observed["epoch"] >= target_epoch:
                break
            if time.monotonic() >= deadline:
                raise AcceptanceError("exact-E wait expired before execute")
            time.sleep(2)
    accounts = scenario["accounts"]
    host = accounts["host"]
    buyer = accounts.get("buyer")
    fee_recipient = accounts["fee_recipient"]
    if not isinstance(buyer, str) or len({host, buyer, fee_recipient}) != 3:
        raise AcceptanceError("boundary Lock requires distinct Host, Buyer, and fee recipient")
    caller = gonka.key_address(DEFAULT_NODE, "genesis")
    if host == caller:
        raise AcceptanceError("boundary Lock caller must differ from Host")

    deal = scenario["contracts"]["deal"]
    before_epoch = epoch_observation(gonka)
    if before_epoch["epoch"] != expected_epoch:
        raise AcceptanceError(
            f"boundary Lock requires epoch {expected_epoch}, got {before_epoch['epoch']}"
        )
    recipients = gonka.query_json("inference", "list-claim-recipients", host)
    entries = recipients.get("entries")
    if not isinstance(entries, list):
        raise AcceptanceError("native recipient response has no entries list")
    exact_entries = [
        item
        for item in entries
        if isinstance(item, Mapping)
        and require_uint(item.get("epoch"), "recipient epoch") == target_epoch
        and item.get("recipient") == deal
    ]
    if len(exact_entries) != 1:
        raise AcceptanceError(
            "boundary Lock requires exactly one native recipient Host/E -> Deal row"
        )

    before = scenario_financial_snapshot(gonka, context, scenario)
    if before["state"].get("status") != "funded" or before["state"].get("recipient_locked"):
        raise AcceptanceError(f"boundary Lock requires an unlocked Funded Deal: {before['state']}")
    prepared = scenario.get("prepared")
    if not isinstance(prepared, Mapping) or before["state"] != prepared.get("deal_state"):
        raise AcceptanceError("Funded Deal state differs from its prepared immutable snapshot")
    config_before = gonka.smart(deal, {"config": {}})
    if config_before != prepared.get("deal_config"):
        raise AcceptanceError("Deal config differs from its prepared immutable snapshot")

    tx = gonka.execute(DEFAULT_NODE, "genesis", deal, {"lock": {}}, gas="auto")
    after_epoch = epoch_observation(gonka)
    epoch_bracket = assert_tx_epoch_bracket(tx, before_epoch, after_epoch)
    if epoch_bracket["epoch"] != expected_epoch:
        raise AcceptanceError(f"Lock inclusion epoch is not the requested boundary: {epoch_bracket}")
    assert_no_bank_transfer_involving(tx, deal)

    after = scenario_financial_snapshot(gonka, context, scenario)
    config_after = gonka.smart(deal, {"config": {}})
    if config_after != config_before:
        raise AcceptanceError("Lock changed immutable Deal config")
    if after["state"].get("status") != "locked" or after["state"].get("recipient_locked") is not True:
        raise AcceptanceError(f"boundary Lock transition mismatch: {after['state']}")
    before_state = dict(before["state"])
    after_state = dict(after["state"])
    for state in (before_state, after_state):
        state.pop("status", None)
        state.pop("recipient_locked", None)
    if before_state != after_state:
        raise AcceptanceError("Lock changed Deal accounting beyond status and routing proof")
    if before["foreign_cw20"] != after["foreign_cw20"]:
        raise AcceptanceError("Lock changed foreign CW20")
    if before["cw20"] != after["cw20"]:
        raise AcceptanceError("Lock distributed CW20")
    for role in ("host", "buyer", "deal"):
        if before["bank_ngonka"][role] != after["bank_ngonka"][role]:
            raise AcceptanceError(f"Lock changed native GNK balance for {role}")

    phase = {
        "name": "lock_exact_e" if offset == 0 else "lock_e_plus_4",
        "caller": caller,
        "expected_epoch": expected_epoch,
        "epoch_bracket": epoch_bracket,
        "recipient_query": recipients,
        "recipient_exact_row": exact_entries[0],
        "tx": filtered_tx(tx),
        "config_before": config_before,
        "config_after": config_after,
        "before": before,
        "after": after,
        "assertions": {
            "distinct_financial_roles": True,
            "caller_differs_from_host": True,
            "immutable_config_preserved": True,
            "buyer_deposit_and_accounting_preserved": True,
            "no_contract_cw20_or_gnk_distribution": True,
        },
    }
    context["scenarios"][args.name]["phases"].append(phase)
    write_object(path, context)
    print(compact_json({"scenario": args.name, "epoch": expected_epoch, "status": "locked"}))


def lock_e_plus_5_rejected_scenario(args: argparse.Namespace) -> None:
    """Prove an included E+5 Lock fails closed before routing can matter."""
    path = Path(args.context)
    context = load_object(path)
    scenario = scenario_record(context, args.name)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)

    target_epoch = require_uint(scenario["terms"]["target_epoch"], "scenario epoch")
    expected_epoch = target_epoch + 5
    accounts = scenario["accounts"]
    host = accounts["host"]
    buyer = accounts.get("buyer")
    fee_recipient = accounts["fee_recipient"]
    if not isinstance(buyer, str) or len({host, buyer, fee_recipient}) != 3:
        raise AcceptanceError("E+5 Lock requires distinct Host, Buyer, and fee recipient")
    caller = gonka.key_address(DEFAULT_NODE, "genesis")
    if host == caller:
        raise AcceptanceError("E+5 Lock caller must differ from Host")

    deal = scenario["contracts"]["deal"]
    before_epoch = epoch_observation(gonka)
    if before_epoch["epoch"] != expected_epoch:
        raise AcceptanceError(
            f"E+5 Lock requires epoch {expected_epoch}, got {before_epoch['epoch']}"
        )
    # At E+5 pruning may already have removed the row, but it may also lag.
    # Keep the raw response as evidence; Lock must reject before consulting it.
    recipients = gonka.query_json("inference", "list-claim-recipients", host)
    entries = recipients.get("entries", [])
    if not isinstance(entries, list):
        raise AcceptanceError("native recipient response has no entries list")
    target_entries = [
        item for item in entries
        if isinstance(item, Mapping)
        and require_uint(item.get("epoch"), "recipient epoch") == target_epoch
    ]

    before = scenario_financial_snapshot(gonka, context, scenario)
    if before["state"].get("status") != "funded" or before["state"].get("recipient_locked"):
        raise AcceptanceError(f"E+5 Lock requires an unlocked Funded Deal: {before['state']}")
    prepared = scenario.get("prepared")
    if not isinstance(prepared, Mapping) or before["state"] != prepared.get("deal_state"):
        raise AcceptanceError("Funded Deal state differs from its prepared immutable snapshot")
    config_before = gonka.smart(deal, {"config": {}})
    if config_before != prepared.get("deal_config"):
        raise AcceptanceError("Deal config differs from its prepared immutable snapshot")
    caller_before = gonka.bank_balance(caller)

    attempt = gonka.tx_attempt(
        DEFAULT_NODE, "genesis", "wasm", "execute", deal, compact_json({"lock": {}}),
        gas=str(args.gas),
    )
    if attempt.get("layer") != "deliver_tx" or attempt.get("code") == 0:
        raise AcceptanceError(f"E+5 Lock was not an included failed DeliverTx: {attempt}")
    after_epoch = epoch_observation(gonka)
    epoch_bracket = assert_tx_epoch_bracket(attempt, before_epoch, after_epoch)
    if epoch_bracket["epoch"] != expected_epoch:
        raise AcceptanceError(f"Lock inclusion epoch is not E+5: {epoch_bracket}")
    raw_log = str(attempt.get("raw_log", "")).lower()
    expected_error = (
        f"lock window is closed: current epoch {expected_epoch}, target epoch {target_epoch}, "
        f"exclusive end {expected_epoch}"
    )
    if expected_error not in raw_log:
        raise AcceptanceError(f"E+5 Lock did not return LockWindowClosed: {attempt}")
    if "out of gas" in raw_log:
        raise AcceptanceError("E+5 Lock failed out of gas instead of LockWindowClosed")
    assert_no_bank_transfer_involving(attempt, deal)

    after = scenario_financial_snapshot(gonka, context, scenario)
    config_after = gonka.smart(deal, {"config": {}})
    if config_after != config_before:
        raise AcceptanceError("rejected E+5 Lock changed immutable Deal config")
    # fee_recipient is a Deal role, not the transaction signer. It must stay
    # unchanged; only the actual genesis caller may pay the failed-tx fee.
    assert_snapshot_unchanged_except_fee_payer(before, after, set())
    caller_after = gonka.bank_balance(caller)
    phase = {
        "name": "lock_e_plus_5_rejected",
        "caller": caller,
        "gas": args.gas,
        "expected_epoch": expected_epoch,
        "epoch_bracket": epoch_bracket,
        "recipient_query": recipients,
        "recipient_target_entries": target_entries,
        "recipient_target_row_observation": "present" if target_entries else "absent",
        "attempt": attempt,
        "config_before": config_before,
        "config_after": config_after,
        "before": before,
        "after": after,
        "caller_native_fee_delta": caller_after - caller_before,
        "assertions": {
            "distinct_financial_roles": True,
            "caller_differs_from_host": True,
            "included_deliver_tx": True,
            "lock_window_closed": True,
            "funded_state_and_recipient_lock_unchanged": True,
            "immutable_config_buyer_deposit_and_accounting_preserved": True,
            "no_contract_cw20_or_gnk_distribution": True,
        },
    }
    context["scenarios"][args.name]["phases"].append(phase)
    write_object(path, context)
    print(compact_json({"scenario": args.name, "epoch": expected_epoch, "status": "rejected"}))


def assert_refund_window_closed(attempt: Mapping[str, Any], *, current: int, target: int) -> None:
    """Accept only the typed Funded-refund boundary rejection, never a transport error."""
    if attempt.get("layer") != "deliver_tx" or attempt.get("code") == 0:
        raise AcceptanceError("E+5 Refund was not an included failed DeliverTx")
    if attempt.get("codespace") != "wasm":
        raise AcceptanceError("E+5 Refund failed outside the wasm contract codespace")
    raw_log = str(attempt.get("raw_log", "")).lower()
    expected = (
        f"refund routing-proof window is closed: current epoch {current}, target epoch {target}, "
        f"exclusive end {target + 5}"
    )
    if expected not in raw_log or "out of gas" in raw_log:
        raise AcceptanceError("E+5 Refund did not return RefundWindowClosed")


def refund_e_plus_5_rejected_scenario(args: argparse.Namespace) -> None:
    """R1.1: an unlocked Funded Deal rejects Refund precisely at E+5."""
    path = Path(args.context)
    context = load_object(path)
    scenario = scenario_record(context, args.name)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    target = require_uint(scenario["terms"]["target_epoch"], "scenario epoch")
    expected_epoch = target + 5
    deal = scenario["contracts"]["deal"]
    caller = gonka.key_address(args.caller_node, args.caller_key)
    roles = {
        scenario["accounts"]["host"],
        scenario["accounts"].get("buyer"),
        scenario["accounts"]["fee_recipient"],
        caller,
    }
    if None in roles or len(roles) != 4:
        raise AcceptanceError("R1.1 requires distinct Host, Buyer, fee recipient, and caller")
    before_epoch = epoch_observation(gonka)
    if before_epoch["epoch"] != expected_epoch:
        raise AcceptanceError(f"R1.1 requires E+5={expected_epoch}, got {before_epoch['epoch']}")
    recipients = gonka.query_json(
        "inference", "list-claim-recipients", scenario["accounts"]["host"]
    )
    before = scenario_financial_snapshot(gonka, context, scenario)
    prepared = scenario.get("prepared")
    if before["state"].get("status") != "funded" or before["state"].get("recipient_locked") is not False:
        raise AcceptanceError("R1.1 requires an unlocked Funded Deal")
    if not isinstance(prepared, Mapping) or before["state"] != prepared.get("deal_state"):
        raise AcceptanceError("R1.1 Deal differs from its immutable funded fixture")
    caller_before = gonka.bank_balance(caller)
    attempt = gonka.tx_attempt(
        args.caller_node,
        args.caller_key,
        "wasm",
        "execute",
        deal,
        compact_json({"refund": {}}),
        gas=str(args.gas),
    )
    after_epoch = epoch_observation(gonka)
    epoch_bracket = assert_tx_epoch_bracket(attempt, before_epoch, after_epoch)
    if epoch_bracket["epoch"] != expected_epoch:
        raise AcceptanceError(f"R1.1 Refund inclusion epoch is not E+5: {epoch_bracket}")
    semantic_error = ""
    try:
        assert_refund_window_closed(attempt, current=expected_epoch, target=target)
        assert_no_bank_transfer_involving(attempt, deal)
    except AcceptanceError as exc:
        semantic_error = str(exc)
    after = scenario_financial_snapshot(gonka, context, scenario)
    invariant_errors: list[str] = []
    try:
        assert_snapshot_unchanged_except_fee_payer(before, after, set())
    except AcceptanceError as exc:
        invariant_errors.append(str(exc))
    phase = {
        "name": "r1_1_refund_e_plus_5_rejected",
        "level": "live_network",
        "status": "PASS" if not semantic_error and not invariant_errors else "FAIL",
        "caller": caller,
        "gas": args.gas,
        "epoch_bracket": epoch_bracket,
        "recipient_query": recipients,
        "attempt": attempt,
        "before": before,
        "after": after,
        "caller_native_fee_delta": gonka.bank_balance(caller) - caller_before,
        "semantic_error": semantic_error,
        "invariant_errors": invariant_errors,
        "expected": {
            "contract_error": "RefundWindowClosed",
            "state": "funded",
            "recipient_locked": False,
            "deposit_on_deal": True,
            "simulation_accepted": False,
            "arbitrary_error_accepted": False,
        },
    }
    context["scenarios"][args.name]["phases"].append(phase)
    write_object(path, context)
    if semantic_error or invariant_errors:
        raise AcceptanceError(semantic_error or compact_json(invariant_errors))
    print(compact_json({"scenario": args.name, "epoch": expected_epoch, "status": "rejected"}))


def lock_rejected_scenario(args: argparse.Namespace) -> None:
    path = Path(args.context)
    context = load_object(path)
    scenario = scenario_record(context, args.name)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    deal = scenario["contracts"]["deal"]
    recipients = gonka.query_json(
        "inference", "list-claim-recipients", scenario["accounts"]["host"]
    )
    entries = recipients.get("entries")
    epoch = require_uint(scenario["terms"]["target_epoch"], "scenario epoch")
    present = isinstance(entries, list) and any(
        isinstance(item, Mapping)
        and require_uint(item.get("epoch"), "recipient epoch") == epoch
        for item in entries
    )
    if args.routing == "present" and not present:
        raise AcceptanceError("expected recipient row is already absent")
    if args.routing == "pruned" and present:
        raise AcceptanceError("recipient row was not pruned at the expected boundary")
    baseline = scenario_financial_snapshot(gonka, context, scenario)
    attempt = gonka.tx_attempt(
        DEFAULT_NODE,
        "genesis",
        "wasm",
        "execute",
        deal,
        compact_json({"lock": {}}),
        gas=str(args.gas),
    )
    if attempt["code"] == 0:
        raise AcceptanceError("late Lock unexpectedly succeeded")
    final = scenario_financial_snapshot(gonka, context, scenario)
    if final != baseline:
        raise AcceptanceError("rejected late Lock changed state or balances")
    context["scenarios"][args.name]["phases"].append(
        {
            "name": "lock_rejected",
            "routing_expectation": args.routing,
            "recipient_query": recipients,
            "attempt": attempt,
            "before": baseline,
            "after": final,
        }
    )
    write_object(path, context)
    print(compact_json({"scenario": args.name, "routing": args.routing, "status": "pass"}))


def claim_scenario(args: argparse.Namespace) -> None:
    path = Path(args.context)
    context = load_object(path)
    scenario = scenario_record(context, args.name)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    epoch = require_uint(scenario["terms"]["target_epoch"], "scenario epoch")
    if args.reward_epoch != epoch:
        raise AcceptanceError("scenario reward seed epoch does not match Deal epoch")
    deal = scenario["contracts"]["deal"]
    before = {
        "deal_bank": gonka.bank_balance(deal),
        "vesting": gonka.query_json("streamvesting", "total-vesting", deal),
    }
    tx = gonka.tx(
        scenario["key_names"]["host_node"],
        scenario["key_names"]["host"],
        "inference",
        "claim-rewards",
        str(args.reward_seed),
        str(args.reward_epoch),
        gas="2000000",
    )
    summary = gonka.query_json(
        "inference",
        "show-epoch-performance-summary-by-participant",
        str(epoch),
        scenario["accounts"]["host"],
    )
    native = summary.get("epochPerformanceSummary")
    if not isinstance(native, Mapping) or native.get("claimed") is not True:
        raise AcceptanceError(f"native scenario claim is not authoritative: {summary}")
    if (
        require_uint(native.get("epoch_index"), "native summary epoch") != epoch
        or native.get("participant_id") != scenario["accounts"]["host"]
    ):
        raise AcceptanceError("native scenario summary identity mismatch")
    after = {
        "deal_bank": gonka.bank_balance(deal),
        "vesting": gonka.query_json("streamvesting", "total-vesting", deal),
    }
    context["scenarios"][args.name]["phases"].append(
        {
            "name": "native_claim",
            "tx": filtered_tx(tx),
            "summary": summary,
            "before": before,
            "after": after,
        }
    )
    write_object(path, context)
    print(compact_json({"scenario": args.name, "claimed": True, "tx_hash": tx_hash(tx)}))


def verify_claimed_scenario(args: argparse.Namespace) -> None:
    path = Path(args.context)
    context = load_object(path)
    scenario = scenario_record(context, args.name)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    epoch = require_uint(scenario["terms"]["target_epoch"], "scenario epoch")
    deadline = time.monotonic() + args.wait_seconds
    summary: dict[str, Any] = {}
    native: Mapping[str, Any] | None = None
    while time.monotonic() < deadline:
        try:
            summary = gonka.query_json(
                "inference",
                "show-epoch-performance-summary-by-participant",
                str(epoch),
                scenario["accounts"]["host"],
            )
        except AcceptanceError:
            time.sleep(1)
            continue
        candidate = summary.get("epochPerformanceSummary")
        if isinstance(candidate, Mapping) and candidate.get("claimed") is True:
            native = candidate
            break
        time.sleep(1)
    if native is None:
        raise AcceptanceError(
            f"native auto-claim did not become authoritative within "
            f"{args.wait_seconds}s: {summary}"
        )
    if (
        require_uint(native.get("epoch_index"), "native summary epoch") != epoch
        or native.get("participant_id") != scenario["accounts"]["host"]
    ):
        raise AcceptanceError("native auto-claim summary identity mismatch")
    total = require_uint(native.get("earned_coins", 0), "native earned") + require_uint(
        native.get("rewarded_coins", 0), "native rewarded"
    )
    if args.require_positive and total <= 0:
        raise AcceptanceError("native auto-claim did not contain positive Work/Reward")
    recipients = gonka.query_json(
        "inference", "list-claim-recipients", scenario["accounts"]["host"]
    )
    entries = recipients.get("entries")
    deal = scenario["contracts"]["deal"]
    routed = isinstance(entries, list) and any(
        isinstance(item, Mapping)
        and require_uint(item.get("epoch"), "recipient epoch") == epoch
        and item.get("recipient") == deal
        for item in entries
    )
    if not routed:
        raise AcceptanceError("native auto-claim lacks exact Deal recipient evidence")
    phase = {
        "name": "native_auto_claim",
        "level": "live_network",
        "summary": summary,
        "recipient_query": recipients,
        "deal_bank": gonka.bank_balance(deal),
        "vesting": gonka.query_json("streamvesting", "total-vesting", deal),
        "expected": {"claimed": True, "positive_total": args.require_positive},
        "actual": {"total_ngonka": total, "exact_recipient": True},
    }
    context["scenarios"][args.name]["phases"].append(phase)
    write_object(path, context)
    print(compact_json({"scenario": args.name, "auto_claimed": True, "total": total}))


def verify_unclaimed_scenario(args: argparse.Namespace) -> None:
    """Require an authoritative claimed=false summary before an expiry test."""
    path = Path(args.context)
    context = load_object(path)
    scenario = scenario_record(context, args.name)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    epoch = require_uint(scenario["terms"]["target_epoch"], "scenario epoch")
    deadline = time.monotonic() + args.wait_seconds
    last: Mapping[str, Any] = {}
    native: Mapping[str, Any] | None = None
    while time.monotonic() < deadline:
        try:
            last = gonka.query_json(
                "inference",
                "show-epoch-performance-summary-by-participant",
                str(epoch),
                scenario["accounts"]["host"],
            )
        except AcceptanceError:
            time.sleep(1)
            continue
        candidate = last.get("epochPerformanceSummary")
        if isinstance(candidate, Mapping):
            claimed = protobuf_bool(candidate, "claimed")
            if claimed:
                raise AcceptanceError(
                    f"claim-expiry precondition invalid: native summary is already claimed: {last}"
                )
            if not claimed:
                native = candidate
                break
        time.sleep(1)
    if native is None:
        raise AcceptanceError(
            f"authoritative claimed=false summary not available within {args.wait_seconds}s: {last}"
        )
    if (
        require_uint(native.get("epoch_index"), "native summary epoch") != epoch
        or native.get("participant_id") != scenario["accounts"]["host"]
    ):
        raise AcceptanceError("native unclaimed summary identity mismatch")
    total = require_uint(native.get("earned_coins", 0), "native earned") + require_uint(
        native.get("rewarded_coins", 0), "native rewarded"
    )
    if args.require_positive and total <= 0:
        raise AcceptanceError("native unclaimed summary has no positive Work/Reward")
    if args.require_zero and total != 0:
        raise AcceptanceError(
            f"native unclaimed summary is not exact zero: total={total}, summary={last}"
        )
    recipients = gonka.query_json(
        "inference", "list-claim-recipients", scenario["accounts"]["host"]
    )
    entries = recipients.get("entries")
    deal = scenario["contracts"]["deal"]
    routed = isinstance(entries, list) and any(
        isinstance(item, Mapping)
        and require_uint(item.get("epoch"), "recipient epoch") == epoch
        and item.get("recipient") == deal
        for item in entries
    )
    if not routed:
        raise AcceptanceError("native unclaimed summary lacks exact Deal recipient evidence")
    observation = epoch_observation(gonka)
    phase = {
        "name": "native_unclaimed_precondition",
        "level": "live_network",
        "summary": last,
        "recipient_query": recipients,
        "target_epoch": epoch,
        "observation": observation,
        "claimed": False,
        "identity_valid": True,
        "positive_total_required": args.require_positive,
        "zero_total_required": args.require_zero,
        "total_ngonka": total,
        "exact_recipient": True,
    }
    context["scenarios"][args.name]["phases"].append(phase)
    write_object(path, context)
    print(
        compact_json(
            {
                "scenario": args.name,
                "claimed": False,
                "total": total,
                "status": "pass",
            }
        )
    )


def verify_missing_summary_scenario(args: argparse.Namespace) -> None:
    """Prove the exact native Host/E record is absent, not merely unreachable."""
    path = Path(args.context)
    context = load_object(path)
    scenario = scenario_record(context, args.name)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    target_epoch = require_uint(scenario["terms"]["target_epoch"], "scenario epoch")
    observation = epoch_observation(gonka)
    expected_epoch = target_epoch + args.expected_offset
    if observation["epoch"] != expected_epoch:
        raise AcceptanceError(
            "missing-summary observation must be at the exact boundary: "
            f"current={observation['epoch']}, expected={expected_epoch}"
        )
    result = gonka.cli(
        DEFAULT_NODE,
        "query",
        "inference",
        "show-epoch-performance-summary-by-participant",
        str(target_epoch),
        scenario["accounts"]["host"],
        "--output",
        "json",
    )
    if result.returncode == 0:
        raise AcceptanceError(
            f"native Host/E summary unexpectedly exists: {result.stdout.strip()}"
        )
    diagnostic = f"{result.stdout}\n{result.stderr}".lower()
    if "code = notfound" not in diagnostic or "desc = not found" not in diagnostic:
        raise AcceptanceError(
            "summary query did not return the exact native NotFound response; "
            f"exit={result.returncode}, stdout={result.stdout!r}, stderr={result.stderr!r}"
        )
    recipients = gonka.query_json(
        "inference", "list-claim-recipients", scenario["accounts"]["host"]
    )
    entries = recipients.get("entries")
    deal = scenario["contracts"]["deal"]
    routed = isinstance(entries, list) and any(
        isinstance(item, Mapping)
        and require_uint(item.get("epoch"), "recipient epoch") == target_epoch
        and item.get("recipient") == deal
        for item in entries
    )
    if not routed:
        raise AcceptanceError("missing-summary scenario lacks exact Deal recipient evidence")
    context["scenarios"][args.name]["phases"].append(
        {
            "name": "native_summary_absent",
            "level": "live_network",
            "target_epoch": target_epoch,
            "host": scenario["accounts"]["host"],
            "expected_offset": args.expected_offset,
            "observation": observation,
            "native_query": {
                "returncode": result.returncode,
                "stdout": result.stdout,
                "stderr": result.stderr,
                "grpc_code": "NotFound",
                "grpc_description": "not found",
                "exact_native_handler_not_found": True,
            },
            "recipient_query": recipients,
            "expected": {
                "summary_absent": True,
                "transport_available": True,
                "exact_recipient": True,
            },
        }
    )
    write_object(path, context)
    print(
        compact_json(
            {
                "scenario": args.name,
                "summary": "absent",
                "epoch": observation["epoch"],
                "status": "pass",
            }
        )
    )


def settle_scenario(args: argparse.Namespace) -> None:
    path = Path(args.context)
    context = load_object(path)
    scenario = scenario_record(context, args.name)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    deal = scenario["contracts"]["deal"]
    cw20 = scenario["contracts"]["cw20"]
    epoch = require_uint(scenario["terms"]["target_epoch"], "scenario epoch")
    summary = gonka.query_json(
        "inference",
        "show-epoch-performance-summary-by-participant",
        str(epoch),
        scenario["accounts"]["host"],
    )
    buyer_control = context["accounts"]["buyer"]
    addresses = {
        "host": scenario["accounts"]["host"],
        "fee_recipient": scenario["accounts"]["fee_recipient"],
        "buyer": buyer_control,
    }
    before = {
        "state": gonka.smart(deal, {"state": {}}),
        "cw20": {
            **{role: gonka.cw20_balance(cw20, address) for role, address in addresses.items()},
            "deal": gonka.cw20_balance(cw20, deal),
        },
    }
    tx = gonka.execute(DEFAULT_NODE, "genesis", deal, {"settle_claim": {}}, gas="auto")
    record_settlement_phase(path, {
        "name": "settlement_committed", "deal": deal, "settle_tx": filtered_tx(tx),
        "summary": summary, "before": before, "addresses": {**addresses, "deal": deal},
    }, args.name)
    payments = gonka.smart(deal, {"usdt_payments": {}})
    withdrawal_txs = recorded_withdrawals(gonka, path, deal, payments, args.name)
    verify_recorded_settlement(gonka, path, args.name)
    context = load_object(path)
    after = {
        "state": gonka.smart(deal, {"state": {}}),
        "cw20": {
            **{role: gonka.cw20_balance(cw20, address) for role, address in addresses.items()},
            "deal": gonka.cw20_balance(cw20, deal),
        },
    }
    actual_deltas = {
        role: after["cw20"][role] - before["cw20"][role]
        for role in ("host", "fee_recipient", "buyer")
    }
    deal_outflow = before["cw20"]["deal"] - after["cw20"]["deal"]
    expected = assert_claim_settlement_matches_oracle(
        scenario, summary, after["state"], actual_deltas, deal_outflow
    )
    context["scenarios"][args.name]["phases"].append(
        {
            "name": "settle_claim",
            "tx": filtered_tx(tx),
            "withdrawal_txs": withdrawal_txs,
            "summary": summary,
            "before": before,
            "after": after,
            "expected": expected,
            "actual": {"cw20_deltas": actual_deltas, "deal_outflow": deal_outflow},
        }
    )
    write_object(path, context)
    print(compact_json({"scenario": args.name, "status": after["state"]["status"]}))


def is_completed_release_noop(state: Mapping[str, Any], deal_balance: int) -> bool:
    return state.get("status") == "completed" and deal_balance == 0


def release_scenario(args: argparse.Namespace) -> None:
    path = Path(args.context)
    context = load_object(path)
    scenario = named_scenario_or_bootstrap(context, args.name)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    deal = scenario["contracts"]["deal"]
    host = scenario["accounts"]["host"]
    buyer = scenario["accounts"].get("buyer") or context["accounts"]["buyer"]
    caller = gonka.key_address(DEFAULT_NODE, "genesis")
    caller_before = gonka.bank_balance(caller)
    epoch_before = epoch_observation(gonka)
    before_state = gonka.smart(deal, {"state": {}})
    before = {
        "deal": gonka.bank_balance(deal),
        "host": gonka.bank_balance(host),
        "buyer": gonka.bank_balance(buyer),
    }
    foreign = context.get("contracts", {}).get("foreign_cw20")
    foreign_before = gonka.cw20_balance(foreign, deal) if isinstance(foreign, str) else None
    if before["deal"] <= 0:
        if is_completed_release_noop(before_state, before["deal"]):
            tx = gonka.execute(
                DEFAULT_NODE, "genesis", deal, {"release_unlocked_gnk": {}}, gas="auto"
            )
            epoch_after = epoch_observation(gonka)
            epoch_bracket = assert_tx_epoch_bracket(tx, epoch_before, epoch_after)
            assert_no_bank_transfer_involving(tx, deal)
            after_state = gonka.smart(deal, {"state": {}})
            after = {
                "deal": gonka.bank_balance(deal),
                "host": gonka.bank_balance(host),
                "buyer": gonka.bank_balance(buyer),
            }
            foreign_after = (
                gonka.cw20_balance(foreign, deal) if isinstance(foreign, str) else None
            )
            if after_state != before_state or after != before or foreign_after != foreign_before:
                raise AcceptanceError("terminal release repeat changed state or balances")
            append_named_phase(context, args.name, {
                    "name": "release_repeat_noop",
                    "level": "live_network",
                    "tx": filtered_tx(tx),
                    "caller": caller,
                    "caller_native_fee_delta": gonka.bank_balance(caller) - caller_before,
                    "epoch_bracket": epoch_bracket,
                    "before_state": before_state,
                    "after_state": after_state,
                    "before": before,
                    "after": after,
                    "foreign_cw20_before": foreign_before,
                    "foreign_cw20_after": foreign_after,
                    "expected": {"native_transfers": 0, "state_unchanged": True},
                })
            write_object(path, context)
            print(compact_json({"scenario": args.name, "release_repeat": "no-op"}))
            return
        raise AcceptanceError("scenario Deal has no spendable ngonka")
    tx = gonka.execute(DEFAULT_NODE, "genesis", deal, {"release_unlocked_gnk": {}}, gas="auto")
    epoch_after = epoch_observation(gonka)
    epoch_bracket = assert_tx_epoch_bracket(tx, epoch_before, epoch_after)
    after_state = gonka.smart(deal, {"state": {}})
    after = {
        "deal": gonka.bank_balance(deal),
        "host": gonka.bank_balance(host),
        "buyer": gonka.bank_balance(buyer),
    }
    foreign_after = gonka.cw20_balance(foreign, deal) if isinstance(foreign, str) else None
    coincident_recipients = host == buyer
    if coincident_recipients:
        address_delta = after["host"] - before["host"]
        buyer_delta = require_uint(
            after_state.get("buyer_released_ngonka"), "buyer released after"
        ) - require_uint(before_state.get("buyer_released_ngonka"), "buyer released before")
        host_delta = require_uint(
            after_state.get("host_released_ngonka"), "host released after"
        ) - require_uint(before_state.get("host_released_ngonka"), "host released before")
    else:
        buyer_delta = after["buyer"] - before["buyer"]
        host_delta = after["host"] - before["host"]
        address_delta = buyer_delta + host_delta
    expected = assert_release_matches_oracle(
        before_state, after_state, before["deal"], buyer_delta, host_delta
    )
    if (
        after["deal"] != 0
        or address_delta != before["deal"]
        or buyer_delta + host_delta != before["deal"]
    ):
        raise AcceptanceError("scenario GNK release does not conserve spendable balance")
    if foreign_after != foreign_before:
        raise AcceptanceError("scenario GNK release spent foreign CW20")
    append_named_phase(context, args.name, {
            "name": "release",
            "tx": filtered_tx(tx),
            "caller": caller,
            "caller_native_fee_delta": gonka.bank_balance(caller) - caller_before,
            "epoch_bracket": epoch_bracket,
            "state_before": before_state,
            "state_after": after_state,
            "bank_before": before,
            "bank_after": after,
            "foreign_cw20_before": foreign_before,
            "foreign_cw20_after": foreign_after,
            "expected": expected,
            "actual": {
                "buyer_delta": buyer_delta,
                "host_delta": host_delta,
                "address_delta": address_delta,
                "coincident_recipients": coincident_recipients,
            },
        })
    write_object(path, context)
    print(compact_json({"scenario": args.name, "released": before["deal"]}))


def bank_release_rollback_scenario(args: argparse.Namespace) -> None:
    """Prove a native BankMsg keeper rejection rolls the whole release back."""
    path = Path(args.context)
    context = load_object(path)
    scenario = named_scenario_or_bootstrap(context, args.name)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    restriction = gonka.transfer_restriction_status()
    if restriction["is_active"] is not True or restriction["remaining_blocks"] <= 0:
        raise AcceptanceError(
            f"native Bank rollback requires active transfer restrictions: {restriction}"
        )
    deal = scenario["contracts"]["deal"]
    buyer = scenario["accounts"].get("buyer") or context["accounts"].get("buyer")
    host = scenario["accounts"]["host"]
    baseline = scenario_financial_snapshot(gonka, context, scenario)
    target = bank_release_fault_target(
        baseline["state"], baseline["bank_ngonka"]["deal"], buyer, host, args.expected_send_index
    )
    live_exemption = None
    if args.expected_send_index == 1:
        if args.allowed_earlier_recipient is not None:
            raise AcceptanceError("Bank send #1 fault cannot allow an earlier recipient")
    else:
        if (
            not isinstance(buyer, str)
            or args.allowed_earlier_recipient != buyer
        ):
            raise AcceptanceError(
                "Bank send #2 fault requires the exact Buyer as its only allowed earlier recipient"
            )
        if not args.exemption_id:
            raise AcceptanceError("Bank send #2 fault requires an exact live exemption id")
        live_exemption = assert_live_bank_second_send_exemption(
            gonka.transfer_restriction_params(),
            restriction["current_block_height"],
            args.exemption_id,
            deal,
            buyer,
            target["plan"]["buyer_delta"],
        )
    if args.rejected_recipient != target["recipient"]:
        raise AcceptanceError(
            "Bank fault selector recipient does not match the production payout position"
        )
    if baseline["bank_ngonka"]["deal"] <= 0:
        raise AcceptanceError("native Bank rollback requires spendable Deal ngonka")
    attempt = gonka.tx_attempt(
        DEFAULT_NODE,
        "genesis",
        "wasm",
        "execute",
        deal,
        compact_json({"release_unlocked_gnk": {}}),
        gas=str(args.gas),
    )
    assert_included_bank_restriction_failure(attempt)
    after = scenario_financial_snapshot(gonka, context, scenario)
    if after != baseline:
        raise AcceptanceError("native BankMsg failure did not roll back Deal atomically")
    append_named_phase(context, args.name, {
            "name": "native_bank_release_rollback",
            "level": "native_keeper_fault",
            "restriction": restriction,
            "attempt": attempt,
            "before": baseline,
            "after": after,
            "expected": {
                "failure_category": "bank_send_restriction",
                "outgoing_transfer_index": args.expected_send_index,
                "allowed_earlier_recipient": args.allowed_earlier_recipient,
                "rejected_recipient": args.rejected_recipient,
                "live_exemption": live_exemption,
                "state_and_tracked_balances_unchanged": True,
                "retry_policy": "retry once restriction_end_block is reached",
            },
            "restriction_proposal_id": args.proposal_id,
        })
    write_object(path, context)
    print(compact_json({"scenario": args.name, "native_bank_rollback": "pass"}))


def assert_included_bank_restriction_failure(attempt: Mapping[str, Any]) -> None:
    """Require the native Bank failure to be an included DeliverTx, never CheckTx/OOG."""
    if attempt.get("layer") != "deliver_tx":
        raise AcceptanceError("Bank rollback attempt was not included as DeliverTx")
    if require_uint(attempt.get("code"), "Bank rollback code") == 0:
        raise AcceptanceError("GNK release unexpectedly succeeded during restrictions")
    tx_hash_value = attempt.get("tx_hash")
    if not isinstance(tx_hash_value, str) or not tx_hash_value:
        raise AcceptanceError("Bank rollback attempt lacks an included transaction hash")
    if require_uint(attempt.get("height"), "Bank rollback height") == 0:
        raise AcceptanceError("Bank rollback attempt lacks an included height")
    if "user-to-user transfers are restricted" not in str(attempt.get("raw_log", "")).lower():
        raise AcceptanceError("GNK release failed outside the native Bank restriction")


def assert_fully_unlocked_vesting(total: Mapping[str, Any], schedule: Mapping[str, Any]) -> None:
    """A governance-latency fixture must not price only the first tranche."""
    remaining = native_coin_amounts(total, "total_amount").get(DEFAULT_DENOM, 0)
    scheduled = sum(coins.get(DEFAULT_DENOM, 0) for coins in vesting_epoch_amounts(schedule))
    if remaining != 0 or scheduled != 0:
        raise AcceptanceError("Bank fault plan requires fully unlocked original GNK vesting")


def bank_release_fault_plan(args: argparse.Namespace) -> None:
    """Expose the exact live R7.1 payout order for the Kotlin restriction plan."""
    path = Path(args.context)
    context = load_object(path)
    scenario = named_scenario_or_bootstrap(context, args.name)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    vesting_proof = None
    if getattr(args, "require_fully_vested", False):
        deal = scenario["contracts"]["deal"]
        total = gonka.query_json("streamvesting", "total-vesting", deal)
        schedule = gonka.query_json("streamvesting", "vesting-schedule", deal)
        assert_fully_unlocked_vesting(total, schedule)
        vesting_proof = {"total": total, "schedule": schedule, "fully_unlocked": True}
    snapshot = scenario_financial_snapshot(gonka, context, scenario)
    available = snapshot["bank_ngonka"]["deal"]
    planned = expected_release(snapshot["state"], available)
    buyer = scenario["accounts"].get("buyer") or context["accounts"].get("buyer")
    host = scenario["accounts"]["host"]
    if (
        not isinstance(buyer, str)
        or buyer == host
        or available <= 0
        or planned["buyer_delta"] <= 0
        or planned["host_delta"] <= 0
    ):
        raise AcceptanceError("R7.1 requires one available proportional release with two non-zero roles")
    plan = {
        "deal": scenario["contracts"]["deal"],
        "buyer": buyer,
        "host": host,
        "buyer_amount": planned["buyer_delta"],
        "host_amount": planned["host_delta"],
        "failing_send_index": 2,
        "allowed_earlier_recipient": buyer,
        "rejected_recipient": host,
    }
    append_named_phase(
        context, args.name, {"name": "native_bank_release_fault_plan", "snapshot": snapshot,
                             "plan": plan, "vesting_proof": vesting_proof}
    )
    write_object(path, context)
    print(compact_json({"scenario": args.name, "bank_fault_plan": plan}))


def bank_release_retry_scenario(args: argparse.Namespace) -> None:
    """Finish a recorded Bank fault with exact retry and no-double-payout proof."""
    path = Path(args.context)
    context = load_object(path)
    scenario = named_scenario_or_bootstrap(context, args.name)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    restriction = gonka.transfer_restriction_status()
    if restriction["is_active"]:
        raise AcceptanceError("Bank retry must run only after the selected restriction expires")
    faults = [
        phase
        for phase in scenario.get("phases", [])
        if phase.get("name") == "native_bank_release_rollback"
        and phase.get("expected", {}).get("outgoing_transfer_index")
        == args.expected_send_index
    ]
    if len(faults) != 1:
        raise AcceptanceError("Bank retry requires exactly one matching recorded fault phase")
    fault = faults[0]
    if fault.get("expected", {}).get("rejected_recipient") != args.rejected_recipient:
        raise AcceptanceError("Bank retry selector differs from the recorded injected failure")

    # Reuse the established release oracle, then repeat on this scenario's
    # Deal. The older terminal helper is intentionally not used: it addresses
    # the bootstrap Deal and additionally assumes status=Completed.
    release_scenario(args)
    scenario_release_repeat(args)
    refreshed = load_object(path)
    refreshed_scenario = named_scenario_or_bootstrap(refreshed, args.name)
    phases = refreshed_scenario.get("phases", [])
    release = next((phase for phase in reversed(phases) if phase.get("name") == "release"), None)
    repeat = next(
        (phase for phase in reversed(phases) if phase.get("name") == "scenario_release_repeat"),
        None,
    )
    if not isinstance(release, Mapping) or not isinstance(repeat, Mapping):
        raise AcceptanceError("Bank retry did not retain release and terminal-repeat evidence")
    append_named_phase(refreshed, args.name, {
            "name": "native_bank_release_retry",
            "level": "native_keeper_fault",
            "restriction_after_fault": restriction,
            "fault": fault,
            "successful_retry": release,
            "repeat": repeat,
            "expected": {
                "outgoing_transfer_index": args.expected_send_index,
                "rejected_recipient": args.rejected_recipient,
                "fault_off_before_retry": True,
                "exact_release_oracle": True,
                "double_payout": False,
            },
        })
    write_object(path, refreshed)
    print(compact_json({"scenario": args.name, "native_bank_retry": "pass"}))


def scenario_release_repeat(args: argparse.Namespace) -> None:
    """Repeat ReleaseUnlockedGnk against the selected scenario Deal, including Refunded."""
    path = Path(args.context)
    context = load_object(path)
    scenario = named_scenario_or_bootstrap(context, args.name)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    deal = scenario["contracts"]["deal"]
    caller = context["accounts"]["inactive_host"]
    caller_key = context["key_names"]["inactive_host"]
    caller_node = context["key_names"]["inactive_host_node"]
    roles = {
        scenario["accounts"]["host"],
        scenario["accounts"].get("buyer") or context["accounts"]["buyer"],
        scenario["accounts"]["fee_recipient"],
        caller,
    }
    if len(roles) != 4:
        raise AcceptanceError("scenario release repeat requires independent financial roles")
    before = scenario_financial_snapshot(gonka, context, scenario)
    if before["bank_ngonka"]["deal"] != 0:
        raise AcceptanceError("scenario release retry left spendable GNK on the selected Deal")
    if before["state"].get("status") not in {"completed", "refunded"}:
        raise AcceptanceError("scenario release repeat requires a terminal Completed or Refunded Deal")
    caller_before = gonka.bank_balance(caller)
    attempt = gonka.tx_attempt(
        caller_node,
        caller_key,
        "wasm",
        "execute",
        deal,
        compact_json({"release_unlocked_gnk": {}}),
        gas=str(args.gas),
    )
    proof = assert_terminal_nothing_to_release(attempt)
    after = scenario_financial_snapshot(gonka, context, scenario)
    caller_after = gonka.bank_balance(caller)
    if after != before:
        raise AcceptanceError("scenario terminal repeat changed selected Deal state or balances")
    if caller_after > caller_before:
        raise AcceptanceError("scenario terminal repeat caller unexpectedly gained GNK")
    append_named_phase(context, args.name, {
            "name": "scenario_release_repeat",
            "level": "native_keeper_fault",
            "deal": deal,
            "caller": caller,
            "before": before,
            "after": after,
            "caller_ngonka_delta": caller_after - caller_before,
            "attempt": attempt,
            "terminal_rejection": proof,
            "expected": {
                "selected_scenario_deal": deal,
                "terminal_status": before["state"].get("status"),
                "repeat_payout": 0,
                "arbitrary_error_accepted": False,
            },
        })
    write_object(path, context)


def scenario_financial_snapshot(
    gonka: DockerGonka, context: Mapping[str, Any], scenario: Mapping[str, Any]
) -> dict[str, Any]:
    deal = scenario["contracts"]["deal"]
    cw20 = scenario["contracts"]["cw20"]
    buyer = scenario["accounts"].get("buyer") or context["accounts"]["buyer"]
    addresses = {
        "host": scenario["accounts"]["host"],
        "buyer": buyer,
        "fee_recipient": scenario["accounts"]["fee_recipient"],
        "deal": deal,
    }
    snapshot = {
        "state": gonka.smart(deal, {"state": {}}),
        "cw20": {
            role: gonka.cw20_balance(cw20, address)
            for role, address in addresses.items()
        },
        "bank_ngonka": {
            role: gonka.bank_balance(address) for role, address in addresses.items()
        },
    }
    foreign = context.get("contracts", {}).get("foreign_cw20")
    if isinstance(foreign, str):
        snapshot["foreign_cw20"] = {
            "deal": gonka.cw20_balance(foreign, deal),
            "buyer": gonka.cw20_balance(foreign, buyer),
        }
    return snapshot


def assert_snapshot_unchanged_except_fee_payer(
    before: Mapping[str, Any],
    after: Mapping[str, Any],
    fee_payer_roles: set[str],
) -> dict[str, int]:
    """Separate unrelated fee-payer activity from contract-ledger mutations."""
    for section in ("state", "cw20", "foreign_cw20"):
        if before.get(section) != after.get(section):
            raise AcceptanceError(f"failed transaction mutated {section}")
    before_bank = before.get("bank_ngonka")
    after_bank = after.get("bank_ngonka")
    if not isinstance(before_bank, Mapping) or not isinstance(after_bank, Mapping):
        raise AcceptanceError("financial snapshot lacks native bank balances")
    deltas: dict[str, int] = {}
    for role, before_value in before_bank.items():
        if role not in after_bank:
            raise AcceptanceError(f"financial snapshot lost bank role {role}")
        before_amount = require_uint(before_value, f"before bank balance {role}")
        after_amount = require_uint(after_bank[role], f"after bank balance {role}")
        delta = after_amount - before_amount
        if role in fee_payer_roles:
            deltas[role] = delta
        elif delta != 0:
            raise AcceptanceError(f"failed transaction moved native balance for role {role}")
    return deltas


def configure_cw20_transfer_failure(
    gonka: DockerGonka,
    context: Mapping[str, Any],
    rejected_recipient: str | None,
) -> dict[str, Any]:
    cw20 = context["contracts"]["cw20"]
    return gonka.execute(
        DEFAULT_NODE,
        "genesis",
        cw20,
        {
            "configure_transfer_failure": {
                "rejected_recipient": rejected_recipient,
            }
        },
        gas="auto",
    )


def assert_injected_cw20_failure(attempt: Mapping[str, Any], operation: str) -> None:
    if attempt.get("code") == 0:
        raise AcceptanceError(f"{operation} unexpectedly succeeded with CW20 fault active")
    raw_log = str(attempt.get("raw_log", "")).lower()
    if "a8 injected cw20 transfer failure" not in raw_log:
        raise AcceptanceError(
            f"{operation} failed outside the configured CW20 fault: {attempt}"
        )


def assert_expected_refund_failure(
    attempt: Mapping[str, Any], expected_reason: str, state_status: str
) -> dict[str, str]:
    """Prove that an expected Refund rejection came from the contract itself."""
    if attempt.get("code") == 0:
        raise AcceptanceError("Refund unexpectedly succeeded")
    if attempt.get("layer") != "deliver_tx":
        raise AcceptanceError(
            f"Refund failed before contract execution instead of for {expected_reason}: {attempt}"
        )
    if expected_reason == "too_early" and state_status == "locked":
        marker = "claim-expiry window has not opened"
    elif expected_reason == "too_early" and state_status == "funded":
        marker = "refund window has not opened"
    elif expected_reason == "claimed" and state_status == "locked":
        marker = "native claim for target epoch"
    elif expected_reason == "network_unconfirmed_too_early" and state_status == "locked":
        marker = "gonka query failed for"
    else:
        raise AcceptanceError(
            f"no expected Refund failure mapping for {expected_reason}/{state_status}"
        )
    raw_log = str(attempt.get("raw_log", "")).lower()
    if marker not in raw_log or (
        expected_reason == "claimed" and "is already confirmed" not in raw_log
    ):
        raise AcceptanceError(
            f"Refund failed for a different reason than {expected_reason}: {attempt}"
        )
    return {
        "reason": expected_reason,
        "state_status": state_status,
        "layer": "deliver_tx",
        "matched_contract_error": marker,
    }


def gas_sweep_scenario(args: argparse.Namespace) -> None:
    path = Path(args.context)
    context = load_object(path)
    scenario = scenario_record(context, args.name)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    deal = scenario["contracts"]["deal"]
    caller = context["contracts"]["caller"]
    baseline = scenario_financial_snapshot(gonka, context, scenario)
    state = baseline["state"]
    if state.get("status") != "locked" or state.get("recipient_locked") is not True:
        raise AcceptanceError("gas sweep requires a pristine Locked Deal")
    if scenario["accounts"].get("buyer") is None:
        raise AcceptanceError("gas sweep requires a funded Buyer")

    target_epoch = require_uint(scenario["terms"]["target_epoch"], "scenario epoch")
    observed_epoch = current_epoch(gonka)
    first_allowed_epoch = target_epoch + NETWORK_UNCONFIRMED_DELAY_EPOCHS
    if observed_epoch < first_allowed_epoch:
        raise AcceptanceError(
            f"gas sweep requires current epoch >= E+3; "
            f"current={observed_epoch}, target={target_epoch}"
        )
    summary = gonka.query_json(
        "inference",
        "show-epoch-performance-summary-by-participant",
        str(target_epoch),
        scenario["accounts"]["host"],
    )
    native = summary.get("epochPerformanceSummary")
    if not isinstance(native, Mapping) or native.get("claimed") is not True:
        raise AcceptanceError(f"gas sweep requires authoritative claimed=true: {summary}")
    if (
        require_uint(native.get("epoch_index"), "gas summary epoch") != target_epoch
        or native.get("participant_id") != scenario["accounts"]["host"]
    ):
        raise AcceptanceError("gas sweep native summary identity mismatch")
    work = require_uint(native.get("earned_coins", 0), "gas summary earned")
    reward = require_uint(native.get("rewarded_coins", 0), "gas summary rewarded")
    total_claim = work + reward
    if total_claim <= 0:
        raise AcceptanceError("gas sweep claimed summary has no positive Work/Reward")

    fee_payer = gonka.key_address(DEFAULT_NODE, "genesis")
    buyer = scenario["accounts"].get("buyer") or context["accounts"]["buyer"]
    tracked_addresses = {
        "host": scenario["accounts"]["host"],
        "buyer": buyer,
        "fee_recipient": scenario["accounts"]["fee_recipient"],
        "deal": deal,
    }
    fee_payer_roles = {
        role for role, address in tracked_addresses.items() if address == fee_payer
    }

    attempts: list[dict[str, Any]] = []
    out_of_gas_observations: list[dict[str, Any]] = []
    for gas_limit in args.gas_limits:
        attempt = gonka.tx_attempt(
            DEFAULT_NODE,
            "genesis",
            "wasm",
            "execute",
            deal,
            compact_json({"refund": {}}),
            gas=str(gas_limit),
        )
        if attempt["code"] == 0:
            raise AcceptanceError(f"direct Refund unexpectedly succeeded with gas {gas_limit}")
        assert_no_bank_transfer_involving(attempt, deal)
        if "out of gas" in attempt["raw_log"].lower():
            out_of_gas_observations.append(
                {"mode": "direct", "gas_limit": gas_limit, "tx": attempt}
            )
        after = scenario_financial_snapshot(gonka, context, scenario)
        fee_deltas = assert_snapshot_unchanged_except_fee_payer(
            baseline, after, fee_payer_roles
        )
        attempts.append(
            {
                "mode": "direct",
                "gas_limit": gas_limit,
                "tx": attempt,
                "cumulative_fee_payer_deltas_ngonka": fee_deltas,
            }
        )

    sufficient = gonka.tx_attempt(
        DEFAULT_NODE,
        "genesis",
        "wasm",
        "execute",
        deal,
        compact_json({"refund": {}}),
        gas=str(args.sufficient_gas),
    )
    sufficient_reason = assert_expected_refund_failure(
        sufficient, "claimed", str(state.get("status"))
    )
    assert_no_bank_transfer_involving(sufficient, deal)
    if "out of gas" in sufficient["raw_log"].lower():
        raise AcceptanceError("sufficient-gas Refund still failed out of gas")
    sufficient_after = scenario_financial_snapshot(gonka, context, scenario)
    sufficient_fee_deltas = assert_snapshot_unchanged_except_fee_payer(
        baseline, sufficient_after, fee_payer_roles
    )
    attempts.append(
        {
            "mode": "direct_sufficient",
            "gas_limit": args.sufficient_gas,
            "tx": sufficient,
            "actual_reason": sufficient_reason,
            "cumulative_fee_payer_deltas_ngonka": sufficient_fee_deltas,
        }
    )

    forwarding = gonka.tx_attempt(
        DEFAULT_NODE,
        "genesis",
        "wasm",
        "execute",
        caller,
        compact_json({"refund": {"deal": deal}}),
        gas=str(args.outer_gas),
    )
    if forwarding["code"] == 0:
        raise AcceptanceError("caller-forwarded Refund unexpectedly succeeded")
    assert_no_bank_transfer_involving(forwarding, deal)
    if "out of gas" in forwarding["raw_log"].lower():
        out_of_gas_observations.append(
            {"mode": "caller_forward", "gas_limit": args.outer_gas, "tx": forwarding}
        )
    forwarding_after = scenario_financial_snapshot(gonka, context, scenario)
    forwarding_fee_deltas = assert_snapshot_unchanged_except_fee_payer(
        baseline, forwarding_after, fee_payer_roles
    )
    attempts.append(
        {
            "mode": "caller_forward",
            "gas_limit": args.outer_gas,
            "tx": forwarding,
            "cumulative_fee_payer_deltas_ngonka": forwarding_fee_deltas,
        }
    )

    for inner_gas in args.gas_limits:
        outer = gonka.tx_attempt(
            DEFAULT_NODE,
            "genesis",
            "wasm",
            "execute",
            caller,
            compact_json(
                {"refund_with_gas": {"deal": deal, "gas_limit": inner_gas}}
            ),
            gas=str(args.outer_gas),
        )
        if outer["code"] != 0:
            raise AcceptanceError(
                f"reply-handling outer transaction failed for inner gas {inner_gas}: {outer}"
            )
        assert_no_bank_transfer_involving(outer, deal)
        reply = gonka.smart(caller, {"last_reply": {}})
        if reply.get("success") is not False or not isinstance(reply.get("error"), str):
            raise AcceptanceError(f"caller did not capture failed Refund reply: {reply}")
        if "out of gas" in reply["error"].lower():
            out_of_gas_observations.append(
                {
                    "mode": "submessage_reply",
                    "gas_limit": inner_gas,
                    "outer_tx": outer,
                    "reply": reply,
                }
            )
        submessage_after = scenario_financial_snapshot(gonka, context, scenario)
        submessage_fee_deltas = assert_snapshot_unchanged_except_fee_payer(
            baseline, submessage_after, fee_payer_roles
        )
        attempts.append(
            {
                "mode": "submessage_reply",
                "gas_limit": inner_gas,
                "outer_tx": outer,
                "reply": reply,
                "cumulative_fee_payer_deltas_ngonka": submessage_fee_deltas,
            }
        )

    if not out_of_gas_observations:
        raise AcceptanceError("gas sweep never produced an actual out-of-gas failure")
    final = scenario_financial_snapshot(gonka, context, scenario)
    context["scenarios"][args.name]["phases"].append(
        {
            "name": "claimed_refund_gas_sweep",
            "level": "live_network",
            "preconditions": {
                "target_epoch": target_epoch,
                "current_epoch": observed_epoch,
                "minimum_epoch": first_allowed_epoch,
                "current_epoch_at_least_e_plus_3": True,
                "summary": summary,
                "claimed": True,
                "summary_identity_valid": True,
                "work_ngonka": work,
                "reward_ngonka": reward,
                "total_claim_ngonka": total_claim,
                "positive_claim": True,
                "fee_payer": fee_payer,
                "fee_payer_roles": sorted(fee_payer_roles),
            },
            "gas_limits": args.gas_limits,
            "sufficient_gas": args.sufficient_gas,
            "outer_gas": args.outer_gas,
            "baseline": baseline,
            "attempts": attempts,
            "out_of_gas": {
                "reached": True,
                "observations": out_of_gas_observations,
            },
            "sufficient_gas_contract_rejection": {
                "reached": True,
                "attempt": sufficient,
                "actual_reason": sufficient_reason,
            },
            "final": final,
        }
    )
    write_object(path, context)
    print(compact_json({"scenario": args.name, "attempts": len(attempts), "status": "pass"}))


def refund_scenario(args: argparse.Namespace) -> None:
    path = Path(args.context)
    context = load_object(path)
    scenario = scenario_record(context, args.name)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    deal = scenario["contracts"]["deal"]
    baseline = scenario_financial_snapshot(gonka, context, scenario)
    before_epoch = epoch_observation(gonka)
    observed_epoch = before_epoch["epoch"]
    target_epoch = require_uint(scenario["terms"]["target_epoch"], "scenario epoch")
    if args.reason == "too_early":
        minimum_epoch = target_epoch + (
            CLAIM_EXPIRY_DELAY_EPOCHS
            if args.reason == "too_early" and scenario["accounts"].get("buyer")
            else NETWORK_UNCONFIRMED_DELAY_EPOCHS
        )
        if observed_epoch >= minimum_epoch:
            raise AcceptanceError(
                f"too_early precondition missed: current={observed_epoch}, "
                f"target={target_epoch}, minimum={minimum_epoch}"
            )
    elif args.reason == "claim_expiry":
        minimum_epoch = target_epoch + CLAIM_EXPIRY_DELAY_EPOCHS
        if observed_epoch < minimum_epoch or observed_epoch >= target_epoch + 5:
            raise AcceptanceError(
                f"claim_expiry window missed: current={observed_epoch}, target={target_epoch}"
            )
    elif args.reason == "network_unconfirmed_too_early":
        if observed_epoch != target_epoch + CLAIM_EXPIRY_DELAY_EPOCHS:
            raise AcceptanceError(
                "network_unconfirmed_too_early requires exact E+2: "
                f"current={observed_epoch}, target={target_epoch}"
            )
        matching_absence = any(
            phase.get("name") == "native_summary_absent"
            and phase.get("expected_offset") == CLAIM_EXPIRY_DELAY_EPOCHS
            and phase.get("observation", {}).get("epoch") == observed_epoch
            for phase in scenario.get("phases", [])
            if isinstance(phase, Mapping)
        )
        if not matching_absence:
            raise AcceptanceError(
                "E+2 emergency rejection requires a same-epoch native NotFound proof"
            )
    elif args.reason == "network_unconfirmed":
        if observed_epoch < target_epoch + NETWORK_UNCONFIRMED_DELAY_EPOCHS:
            raise AcceptanceError(
                f"network_unconfirmed requires E+3: current={observed_epoch}, target={target_epoch}"
            )
        matching_absence = any(
            phase.get("name") == "native_summary_absent"
            and phase.get("expected_offset") == NETWORK_UNCONFIRMED_DELAY_EPOCHS
            and phase.get("observation", {}).get("epoch") == observed_epoch
            for phase in scenario.get("phases", [])
            if isinstance(phase, Mapping)
        )
        if not matching_absence:
            raise AcceptanceError(
                "NetworkUnconfirmed success requires a same-epoch native NotFound proof"
            )
    if args.expect == "failure":
        attempt = gonka.tx_attempt(
            DEFAULT_NODE,
            "genesis",
            "wasm",
            "execute",
            deal,
            compact_json({"refund": {}}),
            gas=str(args.gas),
        )
        after_epoch = epoch_observation(gonka)
        epoch_bracket = assert_tx_epoch_bracket(attempt, before_epoch, after_epoch)
        state_status = str(baseline["state"].get("status"))
        actual_reason = assert_expected_refund_failure(
            attempt, args.reason, state_status
        )
        final = scenario_financial_snapshot(gonka, context, scenario)
        if final != baseline:
            raise AcceptanceError("rejected Refund changed state or balances")
        phase = {
            "name": "refund_rejected",
            "expected_reason": args.reason,
            "actual_reason": actual_reason,
            "attempt": attempt,
            "before": baseline,
            "after": final,
            "observed_epoch": observed_epoch,
            "epoch_bracket": epoch_bracket,
        }
    else:
        fault_evidence = None
        if args.fault_cw20:
            buyer = scenario["accounts"].get("buyer")
            if not isinstance(buyer, str):
                raise AcceptanceError("CW20 refund fault requires a funded Buyer")
            setup_tx = configure_cw20_transfer_failure(gonka, context, buyer)
            fault_baseline = scenario_financial_snapshot(gonka, context, scenario)
            attempt = gonka.tx_attempt(
                DEFAULT_NODE,
                "genesis",
                "wasm",
                "execute",
                deal,
                compact_json({"refund": {}}),
                gas=str(args.gas),
            )
            assert_injected_cw20_failure(attempt, "Refund")
            fault_after = scenario_financial_snapshot(gonka, context, scenario)
            if fault_after != fault_baseline:
                raise AcceptanceError("failed CW20 Refund did not roll back atomically")
            clear_tx = configure_cw20_transfer_failure(gonka, context, None)
            fault_evidence = {
                "level": "native_fault_injection",
                "fault": {
                    "contract": scenario["contracts"]["cw20"],
                    "rejected_recipient": buyer,
                    "outgoing_transfer_index": 1,
                },
                "setup_tx": filtered_tx(setup_tx),
                "attempt": attempt,
                "before": fault_baseline,
                "after": fault_after,
                "clear_tx": filtered_tx(clear_tx),
                "assertion": "Deal state and all tracked CW20/GNK balances are unchanged",
            }
        tx = gonka.execute(DEFAULT_NODE, "genesis", deal, {"refund": {}}, gas="auto")
        after_epoch = epoch_observation(gonka)
        epoch_bracket = assert_tx_epoch_bracket(tx, before_epoch, after_epoch)
        final = scenario_financial_snapshot(gonka, context, scenario)
        expected_status = "refunded" if scenario["accounts"].get("buyer") else "expired"
        if final["state"].get("status") != expected_status:
            raise AcceptanceError(f"Refund terminal status mismatch: {final['state']}")
        if final["state"].get("refund_reason") != args.reason:
            raise AcceptanceError(f"Refund reason mismatch: {final['state']}")
        if args.reason == "network_unconfirmed" and normalize_release_policy(
            final["state"].get("gnk_release_policy")
        ) != "host_only":
            raise AcceptanceError(
                "NetworkUnconfirmed Refund did not freeze HostOnly GNK rights"
            )
        budget = require_uint(scenario["terms"]["budget_micro_usdt"], "scenario budget")
        buyer_delta = final["cw20"]["buyer"] - baseline["cw20"]["buyer"]
        deal_outflow = baseline["cw20"]["deal"] - final["cw20"]["deal"]
        expected_refund = budget if scenario["accounts"].get("buyer") else 0
        if buyer_delta != expected_refund or deal_outflow != expected_refund:
            raise AcceptanceError("Refund CW20 delta differs from exact deposit")
        if final["cw20"]["deal"] != 0:
            raise AcceptanceError("Refund left a non-zero CW20 balance in Deal")
        expected_cw20_deltas = {
            "host": expected_refund
            if scenario["accounts"]["host"] == scenario["accounts"].get("buyer")
            else 0,
            "fee_recipient": 0,
        }
        for role, expected_delta in expected_cw20_deltas.items():
            observed_delta = final["cw20"][role] - baseline["cw20"][role]
            if observed_delta != expected_delta:
                raise AcceptanceError(
                    f"Refund CW20 delta for {role} differs: "
                    f"expected {expected_delta}, got {observed_delta}"
                )
        for role in ("deal", "host", "buyer", "fee_recipient"):
            if final["bank_ngonka"][role] != baseline["bank_ngonka"][role]:
                raise AcceptanceError("Refund unexpectedly moved native GNK")

        terminal_baseline = final
        repeat = gonka.tx_attempt(
            DEFAULT_NODE,
            "genesis",
            "wasm",
            "execute",
            deal,
            compact_json({"refund": {}}),
            gas=str(args.gas),
        )
        settle = gonka.tx_attempt(
            DEFAULT_NODE,
            "genesis",
            "wasm",
            "execute",
            deal,
            compact_json({"settle_claim": {}}),
            gas=str(args.gas),
        )
        if repeat["code"] == 0 or settle["code"] == 0:
            raise AcceptanceError("terminal Refund allowed a repeated or competing payout")
        if scenario_financial_snapshot(gonka, context, scenario) != terminal_baseline:
            raise AcceptanceError("terminal repeat changed state or balances")
        phase = {
            "name": "refund_committed",
            "reason": args.reason,
            "observed_epoch": observed_epoch,
            "epoch_bracket": epoch_bracket,
            "tx": filtered_tx(tx),
            "before": baseline,
            "after": final,
            "expected": {
                "status": expected_status,
                "buyer_refund": expected_refund,
                "host_usdt": 0,
                "fee_usdt": 0,
                "native_transfers": 0,
            },
            "terminal_repeat": repeat,
            "competing_settlement": settle,
            "cw20_fault_rollback": fault_evidence,
        }
    context["scenarios"][args.name]["phases"].append(phase)
    write_object(path, context)
    print(compact_json({"scenario": args.name, "expect": args.expect, "status": "pass"}))


def donate_scenario(args: argparse.Namespace) -> None:
    path = Path(args.context)
    context = load_object(path)
    scenario = scenario_record(context, args.name)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    deal = scenario["contracts"]["deal"]
    buyer = context["accounts"]["buyer"]
    before = {
        "state": gonka.smart(deal, {"state": {}}),
        "deal_bank": gonka.bank_balance(deal),
        "sender_bank": gonka.bank_balance(buyer),
    }
    tx = gonka.tx(
        context["key_names"]["buyer_node"],
        context["key_names"]["buyer"],
        "bank",
        "send",
        buyer,
        deal,
        f"{args.amount}{DEFAULT_DENOM}",
        gas="auto",
    )
    after = {
        "state": gonka.smart(deal, {"state": {}}),
        "deal_bank": gonka.bank_balance(deal),
        "sender_bank": gonka.bank_balance(buyer),
    }
    transfer_event = assert_exact_bank_transfer_event(
        tx, buyer, deal, args.amount, DEFAULT_DENOM
    )
    observed_deal_delta = after["deal_bank"] - before["deal_bank"]
    if observed_deal_delta < args.amount:
        raise AcceptanceError("liquid donation did not reach Deal in full")
    if after["state"] != before["state"]:
        raise AcceptanceError("plain native donation unexpectedly changed Deal state")
    context["scenarios"][args.name]["phases"].append(
        {
            "name": "liquid_donation",
            "label": args.label,
            "amount_ngonka": args.amount,
            "tx": filtered_tx(tx),
            "bank_transfer_event": transfer_event,
            "before": before,
            "after": after,
            "observed_deal_delta_ngonka": observed_deal_delta,
            "concurrent_vesting_unlock_ngonka": observed_deal_delta - args.amount,
        }
    )
    write_object(path, context)
    print(compact_json({"scenario": args.name, "label": args.label, "amount": args.amount}))


def contaminate_scenario(args: argparse.Namespace) -> None:
    path = Path(args.context)
    context = load_object(path)
    scenario = scenario_record(context, args.name)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    foreign = context["contracts"].get("foreign_cw20")
    if not isinstance(foreign, str):
        raise AcceptanceError("foreign CW20 deployment is missing")
    deal = scenario["contracts"]["deal"]
    buyer = context["accounts"]["buyer"]
    before = {
        "deal": gonka.cw20_balance(foreign, deal),
        "buyer": gonka.cw20_balance(foreign, buyer),
        "state": gonka.smart(deal, {"state": {}}),
    }
    tx = gonka.execute(
        context["key_names"]["buyer_node"],
        context["key_names"]["buyer"],
        foreign,
        {"transfer": {"amount": str(args.amount), "recipient": deal}},
        gas="auto",
    )
    after = {
        "deal": gonka.cw20_balance(foreign, deal),
        "buyer": gonka.cw20_balance(foreign, buyer),
        "state": gonka.smart(deal, {"state": {}}),
    }
    if after["deal"] - before["deal"] != args.amount or after["state"] != before["state"]:
        raise AcceptanceError("foreign CW20 contamination was not isolated")
    context["scenarios"][args.name]["phases"].append(
        {
            "name": "foreign_cw20_contamination",
            "amount": args.amount,
            "tx": filtered_tx(tx),
            "before": before,
            "after": after,
        }
    )
    write_object(path, context)
    print(compact_json({"scenario": args.name, "foreign_cw20": args.amount}))


def vesting_epoch_amounts(response: Mapping[str, Any]) -> list[dict[str, int]]:
    schedule = response.get("vesting_schedule")
    if schedule is None:
        return []
    if not isinstance(schedule, Mapping):
        raise AcceptanceError(f"malformed vesting schedule: {response}")
    epoch_amounts = schedule.get("epoch_amounts")
    if epoch_amounts is None:
        return []
    if not isinstance(epoch_amounts, list):
        raise AcceptanceError(f"malformed vesting schedule: {response}")
    normalized: list[dict[str, int]] = []
    for index, epoch in enumerate(epoch_amounts):
        if not isinstance(epoch, Mapping):
            raise AcceptanceError(f"malformed vesting epoch {index}: {epoch}")
        coins = epoch.get("coins")
        if coins is None:
            coins = []
        if not isinstance(coins, list):
            raise AcceptanceError(f"malformed vesting epoch {index}: {epoch}")
        amounts: dict[str, int] = {}
        for coin in coins:
            if not isinstance(coin, Mapping) or not isinstance(coin.get("denom"), str):
                raise AcceptanceError(f"malformed vesting coin at epoch {index}: {coin}")
            denom = coin["denom"]
            amounts[denom] = amounts.get(denom, 0) + require_uint(
                coin.get("amount"), f"vesting amount {index}/{denom}"
            )
        normalized.append(amounts)
    return normalized


def current_epoch(gonka: DockerGonka) -> int:
    response = gonka.query_json("inference", "get-current-epoch")
    return require_uint(response.get("epoch"), "current epoch")


def epoch_observation(gonka: DockerGonka) -> dict[str, int]:
    """Capture the chain height and authoritative epoch next to an E2E action."""
    status = assert_chain(gonka)
    sync = status.get("sync_info")
    if not isinstance(sync, Mapping):
        sync = status.get("SyncInfo")
    if not isinstance(sync, Mapping):
        raise AcceptanceError(f"status lacks sync info: {status}")
    return {
        "height": require_uint(sync.get("latest_block_height"), "epoch observation height"),
        "epoch": current_epoch(gonka),
    }


def assert_tx_epoch_bracket(
    tx: Mapping[str, Any],
    before: Mapping[str, int],
    after: Mapping[str, int],
) -> dict[str, Any]:
    """Prove a transaction was included while the observed epoch stayed fixed."""
    if before["epoch"] != after["epoch"]:
        raise AcceptanceError(
            f"epoch changed around transaction: before={before}, after={after}"
        )
    included_height = require_uint(
        unwrap_tx(tx).get("height"), "transaction inclusion height"
    )
    if not before["height"] <= included_height <= after["height"]:
        raise AcceptanceError(
            "transaction height is outside epoch observation bracket: "
            f"tx={included_height}, before={before}, after={after}"
        )
    return {
        "epoch": before["epoch"],
        "before_height": before["height"],
        "tx_height": included_height,
        "after_height": after["height"],
        "same_epoch": True,
    }


def snapshot_vesting_scenario(args: argparse.Namespace) -> None:
    path = Path(args.context)
    context = load_object(path)
    scenario = scenario_record(context, args.name)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    status = assert_chain(gonka)
    sync = status.get("sync_info")
    if not isinstance(sync, Mapping):
        sync = status.get("SyncInfo")
    if not isinstance(sync, Mapping):
        raise AcceptanceError(f"status lacks sync info: {status}")
    deal = scenario["contracts"]["deal"]
    schedule = gonka.query_json("streamvesting", "vesting-schedule", deal)
    normalized = vesting_epoch_amounts(schedule)
    if args.require_non_empty and not normalized:
        raise AcceptanceError("vesting snapshot requires at least one old tranche")
    phase = {
        "name": "vesting_snapshot",
        "label": args.label,
        "epoch": current_epoch(gonka),
        "height": require_uint(sync.get("latest_block_height"), "vesting snapshot height"),
        "schedule": schedule,
        "normalized_epoch_amounts": normalized,
        "required_non_empty": args.require_non_empty,
    }
    context["scenarios"][args.name]["phases"].append(phase)
    write_object(path, context)
    print(compact_json({"scenario": args.name, "label": args.label, "status": "pass"}))


def verify_vesting_addition_scenario(args: argparse.Namespace) -> None:
    if args.amount <= 0 or args.vesting_epochs <= 0:
        raise AcceptanceError("vesting addition requires positive amount and epoch count")
    path = Path(args.context)
    context = load_object(path)
    scenario = scenario_record(context, args.name)
    snapshots = [
        phase
        for phase in scenario.get("phases", [])
        if phase.get("name") == "vesting_snapshot" and phase.get("label") == args.before_label
    ]
    if len(snapshots) != 1:
        raise AcceptanceError(
            f"expected exactly one vesting snapshot labelled {args.before_label!r}"
        )
    before_phase = snapshots[0]
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    deal = scenario["contracts"]["deal"]
    after_schedule = gonka.query_json("streamvesting", "vesting-schedule", deal)
    after_epoch = current_epoch(gonka)
    if after_epoch != require_uint(before_phase.get("epoch"), "snapshot epoch"):
        raise AcceptanceError(
            "vesting addition crossed an epoch boundary; old-tranche position is ambiguous"
        )

    before = vesting_epoch_amounts(before_phase["schedule"])
    if not before and not args.allow_empty_before:
        raise AcceptanceError("vesting addition proof requires a non-empty old schedule")
    after = vesting_epoch_amounts(after_schedule)
    quotient, remainder = divmod(args.amount, args.vesting_epochs)
    addition = [
        {DEFAULT_DENOM: quotient + (remainder if index == 0 else 0)}
        for index in range(args.vesting_epochs)
    ]
    expected_length = max(len(before), len(addition))
    expected: list[dict[str, int]] = []
    for index in range(expected_length):
        combined: dict[str, int] = {}
        denoms = set(before[index] if index < len(before) else {}) | set(
            addition[index] if index < len(addition) else {}
        )
        for denom in denoms:
            combined[denom] = (
                (before[index].get(denom, 0) if index < len(before) else 0)
                + (addition[index].get(denom, 0) if index < len(addition) else 0)
            )
        expected.append({denom: amount for denom, amount in combined.items() if amount})
    if after != expected:
        raise AcceptanceError(
            "new vesting receipt shifted or changed old tranches: "
            + compact_json({"before": before, "addition": addition, "expected": expected, "after": after})
        )
    if sum(epoch.get(DEFAULT_DENOM, 0) for epoch in after) - sum(
        epoch.get(DEFAULT_DENOM, 0) for epoch in before
    ) != args.amount:
        raise AcceptanceError("vesting addition does not conserve the injected amount")

    funding = gonka.wait_tx(args.fund_tx_hash)
    proposal = gonka.query_json("gov", "proposal", args.proposal_id)
    proposal_record = proposal.get("proposal")
    status = proposal_record.get("status") if isinstance(proposal_record, Mapping) else None
    if "PASSED" not in str(status).upper():
        raise AcceptanceError(f"vesting governance proposal did not pass: {proposal}")
    context["scenarios"][args.name]["phases"].append(
        {
            "name": "vesting_addition",
            "level": "live_network",
            "epoch": after_epoch,
            "amount_ngonka": args.amount,
            "vesting_epochs": args.vesting_epochs,
            "funding_tx": filtered_tx(funding),
            "proposal_id": args.proposal_id,
            "proposal": proposal,
            "before": before_phase["schedule"],
            "addition_by_epoch": addition,
            "expected": expected,
            "after": after_schedule,
            "assertion": "subtracting the new schedule leaves every old tranche unchanged",
            "allow_empty_before": args.allow_empty_before,
        }
    )
    write_object(path, context)
    print(compact_json({"scenario": args.name, "vesting_addition": args.amount, "status": "pass"}))


def verify_factory_isolation(args: argparse.Namespace) -> None:
    path = Path(args.context)
    context = load_object(path)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    factory = context["contracts"]["factory"]
    records: list[tuple[str, Mapping[str, Any]]] = [("primary", context)]
    scenarios = context.get("scenarios", {})
    if not isinstance(scenarios, Mapping):
        raise AcceptanceError("context scenarios must be an object")
    records.extend((name, value) for name, value in scenarios.items() if isinstance(value, Mapping))

    addresses: dict[str, str] = {}
    indexes: dict[str, Any] = {}
    for name, record in records:
        host = record["accounts"]["host"]
        epoch = require_uint(record["terms"]["target_epoch"], f"{name} target epoch")
        deal = record["contracts"]["deal"]
        row = gonka.smart(factory, {"deal_by_host_epoch": {"host": host, "epoch": epoch}})
        if row.get("address") != deal:
            raise AcceptanceError(f"Factory Host/E index changed for {name}: {row}")
        addresses[name] = deal
        indexes[name] = {"host": host, "epoch": epoch, "row": row}
    if len(set(addresses.values())) != len(addresses):
        raise AcceptanceError(f"multiple Factory records resolve to one Deal: {addresses}")

    listed_before = gonka.smart(factory, {"list_deals": {"start_after": None, "limit": 100}})
    listed_addresses = {
        item.get("address")
        for item in listed_before.get("deals", [])
        if isinstance(item, Mapping)
    }
    if not set(addresses.values()).issubset(listed_addresses):
        raise AcceptanceError("Factory list is missing one or more acceptance Deals")

    def deal_snapshot() -> dict[str, Any]:
        return {
            name: {
                "state": gonka.smart(deal, {"state": {}}),
                "cw20": gonka.cw20_balance(context["contracts"]["cw20"], deal),
                "bank_ngonka": gonka.bank_balance(deal),
            }
            for name, deal in addresses.items()
        }

    before = deal_snapshot()
    duplicate = gonka.tx_attempt(
        context["key_names"]["host_node"],
        context["key_names"]["host"],
        "wasm",
        "execute",
        factory,
        compact_json(
            {
                "create_offer": {
                    "target_epoch": context["terms"]["target_epoch"],
                    "price_micro_usdt_per_gnk": context["terms"]["price_micro_usdt_per_gnk"],
                    "buyer_budget_micro_usdt": context["terms"]["budget_micro_usdt"],
                }
            }
        ),
        gas=str(args.gas),
    )
    if duplicate["code"] == 0:
        raise AcceptanceError("Factory allowed a duplicate Host/E Deal")
    after = deal_snapshot()
    listed_after = gonka.smart(factory, {"list_deals": {"start_after": None, "limit": 100}})
    if after != before or listed_after != listed_before:
        raise AcceptanceError("duplicate Factory attempt changed Deal isolation or index state")
    append_phase(
        path,
        {
            "name": "factory_isolation",
            "level": "live_network",
            "indexes": indexes,
            "unique_addresses": addresses,
            "list_before": listed_before,
            "duplicate_attempt": duplicate,
            "deal_snapshot_before": before,
            "deal_snapshot_after": after,
            "list_after": listed_after,
        },
    )
    print(compact_json({"deals": len(addresses), "duplicate_rejected": True, "status": "pass"}))


def lock(args: argparse.Namespace) -> None:
    path = Path(args.context)
    context = load_object(path)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    deal = context["contracts"]["deal"]
    before = gonka.smart(deal, {"state": {}})
    tx = gonka.execute(DEFAULT_NODE, "genesis", deal, {"lock": {}}, gas="auto")
    after = gonka.smart(deal, {"state": {}})
    if before.get("status") != "funded" or after.get("status") != "locked":
        raise AcceptanceError(f"Lock transition mismatch: {before} -> {after}")
    if after.get("recipient_locked") is not True:
        raise AcceptanceError("Lock did not persist routing proof")
    append_phase(
        path,
        {
            "name": "lock",
            "recorded_at_utc": utc_now(),
            "tx": filtered_tx(tx),
            "state_before": before,
            "state_after": after,
        },
    )
    print(compact_json({"status": "locked", "tx_hash": tx_hash(tx)}))


def claim_settle(args: argparse.Namespace) -> None:
    path = Path(args.context)
    context = load_object(path)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    host = context["accounts"]["host"]
    deal = context["contracts"]["deal"]
    cw20 = context["contracts"]["cw20"]
    keys = context["key_names"]
    target_epoch = int(context["terms"]["target_epoch"])
    if args.reward_epoch != target_epoch:
        raise AcceptanceError(
            f"reward seed epoch {args.reward_epoch} does not match Deal epoch {target_epoch}"
        )

    before = {
        "deal_state": gonka.smart(deal, {"state": {}}),
        "deal_bank": gonka.bank_balance(deal),
        "vesting": gonka.query_json("streamvesting", "total-vesting", deal),
        "cw20": {
            **{
                role: gonka.cw20_balance(cw20, address)
                for role, address in context["accounts"].items()
            },
            "deal": gonka.cw20_balance(cw20, deal),
        },
    }
    budget = require_uint(
        context["terms"]["budget_micro_usdt"], "terms.budget_micro_usdt"
    )
    price = require_uint(
        context["terms"]["price_micro_usdt_per_gnk"],
        "terms.price_micro_usdt_per_gnk",
    )
    if price == 0:
        raise AcceptanceError("acceptance price must be positive")
    current_deal_config = gonka.smart(deal, {"config": {}})
    assert_deal_terms(
        current_deal_config,
        {
            "factory": context["contracts"]["factory"],
            "host": host,
            "deal_address": deal,
            "target_epoch": target_epoch,
            "price_micro_usdt_per_gnk": price,
            "buyer_budget_micro_usdt": budget,
            "funded_capacity_ngonka": budget * GNK_SCALE // price,
            "settlement_cw20": cw20,
            "fee_recipient": context["accounts"]["fee_recipient"],
            "fee_bps": PROTOCOL_FEE_BPS,
            "pinned_gonka_sha": EXPECTED_PROTO_SHA,
        },
    )
    if (
        before["deal_state"].get("status") != "locked"
        or before["deal_state"].get("buyer") != context["accounts"]["buyer"]
        or before["cw20"]["deal"] != budget
    ):
        raise AcceptanceError(
            "settlement pre-state does not prove the exact locked funded Deal"
        )
    claim_tx = gonka.tx(
        args.host_node,
        keys["host"],
        "inference",
        "claim-rewards",
        str(args.reward_seed),
        str(args.reward_epoch),
        # Match Testermint's ApplicationCLI: simulation of ClaimRewards lacks
        # historical block hashes on this pinned Gonka build and can panic,
        # while the actual transaction path has the required context.
        gas="2000000",
    )
    summary = gonka.query_json(
        "inference",
        "show-epoch-performance-summary-by-participant",
        str(target_epoch),
        host,
    )
    settle_tx = gonka.execute(
        DEFAULT_NODE, "genesis", deal, {"settle_claim": {}}, gas="auto"
    )
    record_settlement_phase(path, {
        "name": "settlement_committed", "deal": deal,
        "claim_tx": filtered_tx(claim_tx), "settle_tx": filtered_tx(settle_tx),
        "summary": summary, "deal_config": current_deal_config, "before": before,
        "addresses": {**context["accounts"], "deal": deal},
    })
    settlement_payments = gonka.smart(deal, {"usdt_payments": {}})
    fault_evidence = []
    fault_positions = args.cw20_fault_positions
    if args.fault_retry and fault_positions is None:
        # Compatibility with the prior focused scenario: its selected recipient
        # was the protocol fee (production send #2).
        fault_positions = [2]
    if fault_positions:
        for target in cw20_settlement_fault_targets(context, summary, fault_positions):
            role = "fee" if target["role"] == "fee_recipient" else target["role"]
            setup_tx = configure_cw20_transfer_failure(gonka, context, target["recipient"])
            fault_baseline = scenario_financial_snapshot(gonka, context, context)
            pending_before = gonka.smart(deal, {"usdt_payments": {}})
            attempt = gonka.tx_attempt(
                DEFAULT_NODE,
                "genesis",
                "wasm",
                "execute",
                deal,
                compact_json({"withdraw_usdt": {"role": role}}),
                gas="2000000",
            )
            assert_injected_cw20_failure(
                attempt, f"WithdrawUsdt role #{target['outgoing_transfer_index']}"
            )
            fault_after = scenario_financial_snapshot(gonka, context, context)
            pending_after = gonka.smart(deal, {"usdt_payments": {}})
            if fault_after != fault_baseline or pending_after != pending_before:
                raise AcceptanceError(
                    "failed CW20 withdrawal did not roll back atomically for "
                    f"send #{target['outgoing_transfer_index']}"
                )
            clear_tx = configure_cw20_transfer_failure(gonka, context, None)
            fault_evidence.append(
                {
                    "level": "native_fault_injection",
                    "fault": {"contract": cw20, **target},
                    "setup_tx": filtered_tx(setup_tx),
                    "attempt": attempt,
                    "before": fault_baseline,
                    "pending_before": pending_before,
                    "pending_after": pending_after,
                    "after": fault_after,
                    "clear_tx": filtered_tx(clear_tx),
                    "assertion": (
                        "recipient-selected withdrawal failed and preserved all balances, "
                        "settled state, and pending obligations"
                    ),
                }
            )
            record_settlement_phase(path, {
                "name": "usdt_fault_rollback", "evidence": fault_evidence[-1],
            })
    withdrawal_txs = recorded_withdrawals(gonka, path, deal, settlement_payments)
    verify_recorded_settlement(gonka, path)
    after = {
        "deal_state": gonka.smart(deal, {"state": {}}),
        "deal_bank": gonka.bank_balance(deal),
        "vesting": gonka.query_json("streamvesting", "total-vesting", deal),
        "cw20": {
            **{
                role: gonka.cw20_balance(cw20, address)
                for role, address in context["accounts"].items()
            },
            "deal": gonka.cw20_balance(cw20, deal),
        },
    }
    state = after["deal_state"]
    actual_deltas = {
        role: after["cw20"][role] - before["cw20"][role]
        for role in ("host", "fee_recipient", "buyer")
    }
    deal_delta = before["cw20"]["deal"] - after["cw20"]["deal"]
    if after["cw20"]["deal"] != 0:
        raise AcceptanceError("Deal retained settlement CW20 after successful settlement")
    expected = assert_claim_settlement_matches_oracle(
        context, summary, state, actual_deltas, deal_delta
    )
    repeat_attempt = gonka.tx_attempt(
        DEFAULT_NODE,
        "genesis",
        "wasm",
        "execute",
        deal,
        compact_json({"settle_claim": {}}),
        gas="2000000",
    )
    repeat_proof = assert_terminal_settlement_repeat(repeat_attempt)
    repeat_after = {
        "deal_state": gonka.smart(deal, {"state": {}}),
        "cw20": {
            **{
                role: gonka.cw20_balance(cw20, address)
                for role, address in context["accounts"].items()
            },
            "deal": gonka.cw20_balance(cw20, deal),
        },
    }
    if repeat_after != {"deal_state": after["deal_state"], "cw20": after["cw20"]}:
        raise AcceptanceError("repeated settlement changed state or CW20 balances")

    append_phase(
        path,
        {
            "name": "claim_settle",
            "recorded_at_utc": utc_now(),
            "claim_tx": filtered_tx(claim_tx),
            "summary": summary,
            "deal_config": current_deal_config,
            "settle_tx": filtered_tx(settle_tx),
            "settlement_payments": settlement_payments,
            "withdrawal_txs": withdrawal_txs,
            "settle_repeat": {"attempt": repeat_attempt, "proof": repeat_proof},
            "cw20_fault_rollbacks": fault_evidence,
            "before": before,
            "after": after,
            "expected": expected,
            "actual": {
                "cw20_deltas": actual_deltas,
                "deal_outflow": deal_delta,
                "state": {
                    field: state[field]
                    for field in (
                        "status",
                        "work_ngonka",
                        "reward_ngonka",
                        "total_claim_ngonka",
                        "buyer_entitlement_ngonka",
                        "host_entitlement_ngonka",
                        "gnk_release_policy",
                        "gross_usdt",
                        "fee_usdt",
                        "host_net_usdt",
                        "buyer_refund_usdt",
                    )
                },
            },
        },
    )
    print(compact_json({"status": state["status"], "tx_hash": tx_hash(settle_tx)}))


def withdraw_usdt(args: argparse.Namespace) -> None:
    """Retry pending settlement payments without re-running claim or settlement."""
    path = Path(args.context)
    context = load_object(path)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    name = getattr(args, "name", None)
    target = context if name is None else scenario_record(context, name)
    deal = target["contracts"]["deal"]
    before = gonka.smart(deal, {"usdt_payments": {}})
    transactions = recorded_withdrawals(gonka, path, deal, before, name)
    verify_recorded_settlement(gonka, path, name)
    after = gonka.smart(deal, {"usdt_payments": {}})
    record_settlement_phase(path, {
        "name": "withdraw_usdt",
        "recorded_at_utc": utc_now(),
        "before": before,
        "after": after,
        "withdrawal_txs": transactions,
    }, name)
    print(compact_json({"withdrawn_roles": list(transactions), "payments": after}))


def release(args: argparse.Namespace) -> None:
    path = Path(args.context)
    context = load_object(path)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    deal = context["contracts"]["deal"]
    if context["accounts"]["host"] == context["accounts"]["buyer"]:
        raise AcceptanceError("release evidence requires distinct Host and Buyer accounts")
    before_state = gonka.smart(deal, {"state": {}})
    before = {
        "deal": gonka.bank_balance(deal),
        "host": gonka.bank_balance(context["accounts"]["host"]),
        "buyer": gonka.bank_balance(context["accounts"]["buyer"]),
    }
    if before["deal"] <= 0:
        raise AcceptanceError("Deal has no spendable ngonka; release predicate not reached")
    tx = gonka.execute(
        DEFAULT_NODE, "genesis", deal, {"release_unlocked_gnk": {}}, gas="auto"
    )
    after_state = gonka.smart(deal, {"state": {}})
    after = {
        "deal": gonka.bank_balance(deal),
        "host": gonka.bank_balance(context["accounts"]["host"]),
        "buyer": gonka.bank_balance(context["accounts"]["buyer"]),
    }
    buyer_delta = after["buyer"] - before["buyer"]
    host_delta = after["host"] - before["host"]
    if buyer_delta + host_delta != before["deal"] or after["deal"] != 0:
        raise AcceptanceError("GNK release does not conserve the complete spendable balance")
    oracle = assert_release_matches_oracle(
        before_state,
        after_state,
        before["deal"],
        buyer_delta,
        host_delta,
    )
    append_phase(
        path,
        {
            "name": "release",
            "recorded_at_utc": utc_now(),
            "tx": filtered_tx(tx),
            "state_before": before_state,
            "state_after": after_state,
            "bank_before": before,
            "bank_after": after,
            "expected": oracle,
            "actual": {
                "buyer_delta": buyer_delta,
                "host_delta": host_delta,
                "released_total": int(after_state["released_total_ngonka"]),
                "buyer_released": int(after_state["buyer_released_ngonka"]),
                "host_released": int(after_state["host_released_ngonka"]),
            },
        },
    )
    print(compact_json({"released": before["deal"], "tx_hash": tx_hash(tx)}))


def b3_foreign_native_release(args: argparse.Namespace) -> None:
    """Prove a successful GNK release leaves a real foreign native denom on Deal."""
    path = Path(args.context)
    context = load_object(path)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    deal = context["contracts"]["deal"]
    cw20 = context["contracts"]["cw20"]
    foreign_cw20 = context["contracts"]["foreign_cw20"]
    host = context["accounts"]["host"]
    buyer = context["accounts"]["buyer"]
    fee_recipient = context["accounts"]["fee_recipient"]
    caller = gonka.key_address(DEFAULT_NODE, "genesis")
    roles = {
        "deal": deal,
        "host": host,
        "buyer": buyer,
        "fee_recipient": fee_recipient,
        "caller": caller,
    }
    if len({host, buyer, fee_recipient, caller}) != 4:
        raise AcceptanceError(
            "B3 requires distinct Host, Buyer, fee recipient, and fee-paying caller"
        )
    if args.foreign_denom == DEFAULT_DENOM:
        raise AcceptanceError("B3 foreign denom must differ from ngonka")
    if args.foreign_amount <= 0:
        raise AcceptanceError("B3 foreign amount must be positive")
    if gonka.key_address(DEFAULT_NODE, args.foreign_key) != args.foreign_address:
        raise AcceptanceError("B3 foreign key does not match its genesis fixture address")

    foreign_before_funding = {
        role: gonka.bank_balance(address, args.foreign_denom)
        for role, address in roles.items()
    }
    source_before_funding = gonka.bank_balance(args.foreign_address, args.foreign_denom)
    if foreign_before_funding["deal"] != 0 or source_before_funding != args.foreign_amount:
        raise AcceptanceError(
            "B3 genesis fixture is not the exact unspent foreign-denom balance"
        )
    funding_tx = gonka.tx(
        DEFAULT_NODE,
        args.foreign_key,
        "bank",
        "send",
        args.foreign_address,
        deal,
        f"{args.foreign_amount}{args.foreign_denom}",
        gas="auto",
    )
    if tx_code(funding_tx) != 0:
        raise AcceptanceError("B3 foreign-denom Bank funding did not succeed")
    funding_event = assert_exact_bank_transfer_event(
        funding_tx, args.foreign_address, deal, args.foreign_amount, args.foreign_denom
    )
    foreign_after_funding = {
        role: gonka.bank_balance(address, args.foreign_denom)
        for role, address in roles.items()
    }
    source_after_funding = gonka.bank_balance(args.foreign_address, args.foreign_denom)
    if (
        foreign_after_funding["deal"] != args.foreign_amount
        or source_before_funding - source_after_funding != args.foreign_amount
        or any(
            foreign_after_funding[role] != foreign_before_funding[role]
            for role in ("host", "buyer", "fee_recipient", "caller")
        )
    ):
        raise AcceptanceError("B3 foreign-denom Bank funding changed an unexpected balance")

    state_before = gonka.smart(deal, {"state": {}})
    first_native_status = gonka.smart(deal, {"native_status": {}})
    before = {
        "ngonka": {role: gonka.bank_balance(address) for role, address in roles.items()},
        "foreign_native": foreign_after_funding,
        "settlement_cw20": {
            role: gonka.cw20_balance(cw20, address) for role, address in roles.items()
        },
        "foreign_cw20": {
            role: gonka.cw20_balance(foreign_cw20, address)
            for role, address in roles.items()
        },
    }
    second_native_status = gonka.smart(deal, {"native_status": {}})
    if first_native_status != second_native_status:
        raise AcceptanceError("B3 native status changed before release; stable epoch required")
    available = before["ngonka"]["deal"]
    if available <= 0:
        raise AcceptanceError("B3 requires positive available ngonka before release")
    if require_uint(first_native_status.get("liquid_balance_ngonka"), "B3 liquid balance") != available:
        raise AcceptanceError("B3 native status does not match Deal liquid ngonka balance")
    epoch_before = epoch_observation(gonka)
    release_tx = gonka.execute(
        DEFAULT_NODE, "genesis", deal, {"release_unlocked_gnk": {}}, gas="auto"
    )
    epoch_after = epoch_observation(gonka)
    epoch_bracket = assert_tx_epoch_bracket(release_tx, epoch_before, epoch_after)
    if tx_code(release_tx) != 0:
        raise AcceptanceError("B3 ReleaseUnlockedGnk DeliverTx was not successful")
    assert_no_denom_transfer_involving(release_tx, deal, args.foreign_denom)
    state_after = gonka.smart(deal, {"state": {}})
    after = {
        "ngonka": {role: gonka.bank_balance(address) for role, address in roles.items()},
        "foreign_native": {
            role: gonka.bank_balance(address, args.foreign_denom)
            for role, address in roles.items()
        },
        "settlement_cw20": {
            role: gonka.cw20_balance(cw20, address) for role, address in roles.items()
        },
        "foreign_cw20": {
            role: gonka.cw20_balance(foreign_cw20, address)
            for role, address in roles.items()
        },
    }
    buyer_delta = after["ngonka"]["buyer"] - before["ngonka"]["buyer"]
    host_delta = after["ngonka"]["host"] - before["ngonka"]["host"]
    oracle = assert_release_matches_oracle(
        state_before, state_after, available, buyer_delta, host_delta
    )
    if after["ngonka"]["deal"] != 0 or buyer_delta + host_delta != available:
        raise AcceptanceError("B3 successful release did not conserve available ngonka")
    if after["ngonka"]["fee_recipient"] != before["ngonka"]["fee_recipient"]:
        raise AcceptanceError("B3 ReleaseUnlockedGnk charged the fee recipient, not its caller")
    # This local chain can configure a zero transaction fee.  The recipient's
    # unchanged balance proves it was not substituted for the distinct signer;
    # retain the signer's observed delta without requiring it to be negative.
    caller_fee_delta = after["ngonka"]["caller"] - before["ngonka"]["caller"]
    if after["foreign_native"] != before["foreign_native"]:
        raise AcceptanceError("B3 successful release moved the foreign native denom")
    if after["settlement_cw20"] != before["settlement_cw20"]:
        raise AcceptanceError("B3 successful release moved settlement CW20")
    if after["foreign_cw20"] != before["foreign_cw20"]:
        raise AcceptanceError("B3 successful release moved foreign CW20")

    append_phase(
        path,
        {
            "name": "b3_foreign_native_successful_release",
            "recorded_at_utc": utc_now(),
            "level": "live_network",
            "fixture": {
                "denom": args.foreign_denom,
                "amount": args.foreign_amount,
                "genesis_address": args.foreign_address,
                "genesis_source_balance_before": source_before_funding,
                "genesis_source_balance_after": source_after_funding,
            },
            "foreign_native_funding": {
                "tx": filtered_tx(funding_tx),
                "event": funding_event,
                "before": foreign_before_funding,
                "after": foreign_after_funding,
            },
            "release": {
                "caller": caller,
                "caller_ngonka_fee_delta": caller_fee_delta,
                "stable_epoch": epoch_bracket,
                "native_status_before": first_native_status,
                "state_before": state_before,
                "state_after": state_after,
                "tx": filtered_tx(release_tx),
                "before": before,
                "after": after,
                "expected": oracle,
                "actual": {
                    "buyer_delta": buyer_delta,
                    "host_delta": host_delta,
                    "available_ngonka": available,
                    "foreign_native_deal_after": after["foreign_native"]["deal"],
                },
            },
        },
    )
    print(
        compact_json(
            {
                "status": "pass",
                "funding_tx_hash": tx_hash(funding_tx),
                "release_tx_hash": tx_hash(release_tx),
                "denom": args.foreign_denom,
                "amount": args.foreign_amount,
            }
        )
    )


def native_coin_amounts(response: Mapping[str, Any], field: str) -> dict[str, int]:
    coins = response.get(field)
    if coins is None:
        coins = []
    if not isinstance(coins, list):
        raise AcceptanceError(f"native coin response has malformed {field}: {response}")
    amounts: dict[str, int] = {}
    for coin in coins:
        if not isinstance(coin, Mapping) or not isinstance(coin.get("denom"), str):
            raise AcceptanceError(f"native coin response has malformed coin: {coin}")
        denom = coin["denom"]
        amounts[denom] = amounts.get(denom, 0) + require_uint(
            coin.get("amount"), f"{field}/{denom}"
        )
    return amounts


def terminal_release_snapshot(
    gonka: DockerGonka, context: Mapping[str, Any]
) -> dict[str, Any]:
    deal = context["contracts"]["deal"]
    settlement_cw20 = context["contracts"]["cw20"]
    foreign_cw20 = context["contracts"]["foreign_cw20"]
    addresses = {
        "host": context["accounts"]["host"],
        "buyer": context["accounts"]["buyer"],
        "fee_recipient": context["accounts"]["fee_recipient"],
        "caller": context["accounts"]["inactive_host"],
        "deal": deal,
    }
    return {
        "state": gonka.smart(deal, {"state": {}}),
        "config": gonka.smart(deal, {"config": {}}),
        "entitlements": gonka.smart(deal, {"entitlements": {}}),
        "release_status": gonka.smart(deal, {"release_status": {}}),
        "native_status": gonka.smart(deal, {"native_status": {}}),
        "vesting_total": gonka.query_json("streamvesting", "total-vesting", deal),
        "vesting_schedule": gonka.query_json(
            "streamvesting", "vesting-schedule", deal
        ),
        "bank": {
            role: gonka.bank_balances(address) for role, address in addresses.items()
        },
        "settlement_cw20": {
            role: gonka.cw20_balance(settlement_cw20, address)
            for role, address in addresses.items()
        },
        "foreign_cw20": {
            role: gonka.cw20_balance(foreign_cw20, address)
            for role, address in addresses.items()
        },
    }


def r2_gift_snapshot(
    gonka: DockerGonka, context: Mapping[str, Any], scenario: Mapping[str, Any]
) -> dict[str, Any]:
    """Scenario-scoped native snapshot; unlike terminal_snapshot it has no primary-Deal assumption."""
    deal = scenario["contracts"]["deal"]
    cw20 = scenario["contracts"]["cw20"]
    addresses = {
        "host": scenario["accounts"]["host"],
        "buyer": scenario["accounts"]["buyer"],
        "fee_recipient": scenario["accounts"]["fee_recipient"],
        "deal": deal,
    }
    return {
        "state": gonka.smart(deal, {"state": {}}),
        "config": gonka.smart(deal, {"config": {}}),
        "entitlements": gonka.smart(deal, {"entitlements": {}}),
        "release_status": gonka.smart(deal, {"release_status": {}}),
        "native_status": gonka.smart(deal, {"native_status": {}}),
        "vesting_total": gonka.query_json("streamvesting", "total-vesting", deal),
        "vesting_schedule": gonka.query_json("streamvesting", "vesting-schedule", deal),
        "bank": {role: gonka.bank_balances(address) for role, address in addresses.items()},
        "settlement_cw20": {role: gonka.cw20_balance(cw20, address) for role, address in addresses.items()},
    }


def r2_gift_checkpoint(args: argparse.Namespace) -> None:
    """Persist and validate every R2.1 native-vesting boundary."""
    path = Path(args.context)
    if args.stage != "pre_gift" and args.gift_amount <= 0:
        raise AcceptanceError("R2 gift checkpoints after pre_gift require a positive gift amount")
    context = load_object(path)
    scenario = scenario_record(context, args.name)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    snapshot = r2_gift_snapshot(gonka, context, scenario)
    state = snapshot["state"]
    native = snapshot["native_status"]
    schedule = vesting_epoch_amounts(snapshot["vesting_schedule"])
    pending = require_uint(native.get("remaining_vesting_ngonka"), "R2 remaining vesting")
    liquid = require_uint(native.get("liquid_balance_ngonka"), "R2 liquid balance")
    schedule_total = sum(epoch.get(DEFAULT_DENOM, 0) for epoch in schedule)
    checkpoints = [phase for phase in scenario.get("phases", []) if phase.get("name") == "r2_gift_checkpoint"]
    baseline = next((phase for phase in checkpoints if phase.get("stage") == "pre_gift"), None)
    if args.stage == "pre_gift":
        if state.get("status") != "completed" or pending != 0 or liquid != 0 or schedule_total != 0:
            raise AcceptanceError("R2 pre-gift requires Completed with zero liquid and pending vesting")
        if require_uint(state.get("total_claim_ngonka"), "R2 total claim") == 0:
            raise AcceptanceError("R2 requires a positive original claim and proportional shares")
        if not isinstance(state.get("gnk_release_policy"), Mapping):
            raise AcceptanceError("R2 requires frozen proportional release policy")
    else:
        if not isinstance(baseline, Mapping):
            raise AcceptanceError("R2 checkpoint requires the persisted pre_gift baseline")
        before = baseline["snapshot"]
        for section in ("config", "entitlements", "settlement_cw20"):
            if snapshot[section] != before[section]:
                raise AcceptanceError(f"R2 gift changed immutable {section}")
        before_state = before["state"]
        immutable_state = {key: value for key, value in state.items() if key not in {"released_total_ngonka", "buyer_released_ngonka", "host_released_ngonka"}}
        immutable_before = {key: value for key, value in before_state.items() if key not in {"released_total_ngonka", "buyer_released_ngonka", "host_released_ngonka"}}
        if immutable_state != immutable_before or state.get("status") != "completed":
            raise AcceptanceError("R2 gift changed Completed state or frozen shares")
        if args.stage == "fully_locked":
            if pending != args.gift_amount or liquid != 0 or schedule_total != args.gift_amount or len(schedule) < 2:
                raise AcceptanceError("R2 gift was not fully locked in at least two native tranches")
            if state.get("released_total_ngonka") != before_state.get("released_total_ngonka"):
                raise AcceptanceError("R2 gift executed before an unlock")
        elif args.stage == "first_unlocked":
            if not (0 < pending < args.gift_amount and liquid > 0):
                raise AcceptanceError("R2 first checkpoint requires liquid unlock with vesting still remaining")
            if state.get("released_total_ngonka") != before_state.get("released_total_ngonka"):
                raise AcceptanceError("R2 counters changed before first gift release")
        elif args.stage == "final":
            if pending != 0 or liquid != 0 or schedule_total != 0:
                raise AcceptanceError("R2 final checkpoint requires zero pending vesting and liquid balance")
            expected = expected_release(before_state, args.gift_amount)
            actual = {
                "released_total": require_uint(state.get("released_total_ngonka"), "R2 released total"),
                "buyer_released": require_uint(state.get("buyer_released_ngonka"), "R2 Buyer released"),
                "host_released": require_uint(state.get("host_released_ngonka"), "R2 Host released"),
            }
            if actual != {key: expected[key] for key in actual}:
                raise AcceptanceError("R2 final cumulative counters differ from independent gift oracle")
    phase = {"name": "r2_gift_checkpoint", "stage": args.stage, "gift_amount_ngonka": args.gift_amount, "epoch": epoch_observation(gonka), "snapshot": snapshot}
    context["scenarios"][args.name]["phases"].append(phase)
    write_object(path, context)
    print(compact_json({"scenario": args.name, "r2_stage": args.stage, "status": "pass"}))
def assert_terminal_release_preconditions(snapshot: Mapping[str, Any]) -> dict[str, Any]:
    state = snapshot.get("state")
    release_status = snapshot.get("release_status")
    native_status = snapshot.get("native_status")
    if not all(isinstance(value, Mapping) for value in (state, release_status, native_status)):
        raise AcceptanceError("terminal release snapshot lacks contract state queries")
    if state.get("status") != "completed":
        raise AcceptanceError(f"terminal release requires Completed, got {state}")
    total_claim = require_uint(state.get("total_claim_ngonka"), "total claim")
    if total_claim == 0:
        raise AcceptanceError("terminal release requires a positive native claim")
    released_total = require_uint(
        release_status.get("released_total_ngonka"), "released total"
    )
    buyer_remaining = require_uint(
        release_status.get("buyer_original_remaining_ngonka"),
        "Buyer original remaining",
    )
    host_remaining = require_uint(
        release_status.get("host_original_remaining_ngonka"),
        "Host original remaining",
    )
    if released_total < total_claim or buyer_remaining != 0 or host_remaining != 0:
        raise AcceptanceError(
            "Completed Deal has unmet original GNK obligations: "
            + compact_json(dict(release_status))
        )
    liquid = require_uint(
        native_status.get("liquid_balance_ngonka"), "liquid Deal balance"
    )
    remaining_vesting = require_uint(
        native_status.get("remaining_vesting_ngonka"), "remaining Deal vesting"
    )
    deal_bank = snapshot.get("bank", {}).get("deal", {})
    if not isinstance(deal_bank, Mapping):
        raise AcceptanceError("terminal release snapshot lacks Deal bank balances")
    total_vesting = native_coin_amounts(snapshot["vesting_total"], "total_amount")
    scheduled_vesting = vesting_epoch_amounts(snapshot["vesting_schedule"])
    scheduled_ngonka = sum(item.get(DEFAULT_DENOM, 0) for item in scheduled_vesting)
    if (
        liquid != 0
        or remaining_vesting != 0
        or require_uint(deal_bank.get(DEFAULT_DENOM, 0), "Deal bank ngonka") != 0
        or total_vesting.get(DEFAULT_DENOM, 0) != 0
        or scheduled_ngonka != 0
    ):
        raise AcceptanceError(
            "terminal release requires zero liquid and pending Deal ngonka: "
            + compact_json(
                {
                    "native_status": native_status,
                    "deal_bank": deal_bank,
                    "total_vesting": total_vesting,
                    "scheduled_ngonka": scheduled_ngonka,
                }
            )
        )
    return {
        "positive_total_claim_ngonka": total_claim,
        "released_total_ngonka": released_total,
        "buyer_original_remaining_ngonka": buyer_remaining,
        "host_original_remaining_ngonka": host_remaining,
        "liquid_balance_ngonka": liquid,
        "remaining_vesting_ngonka": remaining_vesting,
        "scheduled_vesting_ngonka": scheduled_ngonka,
    }


def assert_terminal_snapshot_unchanged_except_caller_fee(
    before: Mapping[str, Any], after: Mapping[str, Any]
) -> dict[str, int]:
    immutable_sections = (
        "state",
        "config",
        "entitlements",
        "release_status",
        "native_status",
        "vesting_total",
        "vesting_schedule",
        "settlement_cw20",
        "foreign_cw20",
    )
    for section in immutable_sections:
        if before.get(section) != after.get(section):
            raise AcceptanceError(f"terminal release repeat changed {section}")
    before_bank = before.get("bank")
    after_bank = after.get("bank")
    if not isinstance(before_bank, Mapping) or not isinstance(after_bank, Mapping):
        raise AcceptanceError("terminal release snapshot lacks bank balances")
    for role, balances in before_bank.items():
        if role == "caller":
            continue
        if after_bank.get(role) != balances:
            raise AcceptanceError(f"terminal release repeat changed {role} bank balance")
    caller_before = before_bank.get("caller")
    caller_after = after_bank.get("caller")
    if not isinstance(caller_before, Mapping) or not isinstance(caller_after, Mapping):
        raise AcceptanceError("terminal release snapshot lacks caller bank balance")
    denoms = set(caller_before) | set(caller_after)
    deltas = {
        denom: require_uint(caller_after.get(denom, 0), f"caller after {denom}")
        - require_uint(caller_before.get(denom, 0), f"caller before {denom}")
        for denom in denoms
    }
    if any(delta != 0 for denom, delta in deltas.items() if denom != DEFAULT_DENOM):
        raise AcceptanceError("terminal release fee payer changed a non-GNK balance")
    if deltas.get(DEFAULT_DENOM, 0) > 0:
        raise AcceptanceError("terminal release caller unexpectedly gained GNK")
    return deltas


def assert_terminal_nothing_to_release(attempt: Mapping[str, Any]) -> dict[str, Any]:
    """Prove that the terminal repeat reached Deal and failed for the exact reason."""
    marker = "no additional GNK is currently available for release"
    if attempt.get("layer") != "deliver_tx":
        raise AcceptanceError(
            "terminal ReleaseUnlockedGnk was not included as a DeliverTx"
        )
    code = require_uint(attempt.get("code"), "terminal ReleaseUnlockedGnk code")
    if code == 0:
        raise AcceptanceError(
            "terminal ReleaseUnlockedGnk unexpectedly succeeded instead of returning NothingToRelease"
        )
    if attempt.get("codespace") != "wasm":
        raise AcceptanceError(
            "terminal ReleaseUnlockedGnk failed outside the wasm contract codespace"
        )
    tx_hash_value = attempt.get("tx_hash")
    if not isinstance(tx_hash_value, str) or not tx_hash_value:
        raise AcceptanceError("terminal ReleaseUnlockedGnk lacks an included tx hash")
    height = require_uint(attempt.get("height"), "terminal ReleaseUnlockedGnk height")
    if height == 0:
        raise AcceptanceError("terminal ReleaseUnlockedGnk lacks a positive inclusion height")
    raw_log = str(attempt.get("raw_log", ""))
    if marker not in raw_log:
        raise AcceptanceError(
            "terminal ReleaseUnlockedGnk failed for a different reason than NothingToRelease"
        )
    return {
        "contract_error": "NothingToRelease",
        "matched_contract_error": marker,
        "layer": "deliver_tx",
        "code": code,
        "codespace": "wasm",
        "tx_hash": tx_hash_value,
        "height": height,
    }


def assert_terminal_settlement_repeat(attempt: Mapping[str, Any]) -> dict[str, Any]:
    """Only an included contract terminal-state error proves no second settlement."""
    if attempt.get("layer") != "deliver_tx":
        raise AcceptanceError("repeated SettleClaim was not included as DeliverTx")
    if require_uint(attempt.get("code"), "repeated SettleClaim code") == 0:
        raise AcceptanceError("successful settlement unexpectedly paid twice")
    if attempt.get("codespace") != "wasm":
        raise AcceptanceError("repeated SettleClaim failed outside the wasm contract")
    if not isinstance(attempt.get("tx_hash"), str) or not attempt["tx_hash"]:
        raise AcceptanceError("repeated SettleClaim lacks an included transaction hash")
    if require_uint(attempt.get("height"), "repeated SettleClaim height") == 0:
        raise AcceptanceError("repeated SettleClaim lacks an included height")
    marker = "cannot settle claim in state"
    if marker not in str(attempt.get("raw_log", "")).lower():
        raise AcceptanceError("repeated SettleClaim failed for a non-terminal contract reason")
    return {
        "contract_error": "InvalidSettlementState",
        "layer": "deliver_tx",
        "codespace": "wasm",
        "matched_contract_error": marker,
        "tx_hash": attempt["tx_hash"],
        "height": require_uint(attempt["height"], "repeated SettleClaim height"),
    }


def terminal_release_repeat(args: argparse.Namespace) -> None:
    path = Path(args.context)
    context = load_object(path)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    deal = context["contracts"]["deal"]
    caller = context["accounts"]["inactive_host"]
    caller_key = context["key_names"]["inactive_host"]
    caller_node = context["key_names"]["inactive_host_node"]
    independent_roles = {
        context["accounts"]["host"],
        context["accounts"]["buyer"],
        context["accounts"]["fee_recipient"],
        caller,
    }
    if len(independent_roles) != 4:
        raise AcceptanceError("G3 requires distinct Host, Buyer, fee recipient, and caller")

    before = terminal_release_snapshot(gonka, context)
    preconditions = assert_terminal_release_preconditions(before)
    epoch_before = epoch_observation(gonka)
    attempt = gonka.tx_attempt(
        caller_node,
        caller_key,
        "wasm",
        "execute",
        deal,
        compact_json({"release_unlocked_gnk": {}}),
        gas=str(args.gas),
    )
    receipt: Mapping[str, Any] = attempt
    if attempt.get("layer") == "deliver_tx" and attempt.get("tx_hash"):
        receipt = gonka.wait_tx_any(str(attempt["tx_hash"]))
    epoch_after = epoch_observation(gonka)
    after = terminal_release_snapshot(gonka, context)

    invariant_errors: list[str] = []
    try:
        assert_no_bank_transfer_involving(receipt, deal)
    except AcceptanceError as exc:
        invariant_errors.append(str(exc))
    caller_deltas: dict[str, int] = {}
    try:
        caller_deltas = assert_terminal_snapshot_unchanged_except_caller_fee(before, after)
    except AcceptanceError as exc:
        invariant_errors.append(str(exc))

    terminal_rejection: dict[str, Any] = {}
    semantic_error = ""
    try:
        terminal_rejection = assert_terminal_nothing_to_release(attempt)
    except AcceptanceError as exc:
        semantic_error = str(exc)
    accepted_terminal_result = not semantic_error
    phase = {
        "name": "terminal_release_repeat",
        "recorded_at_utc": utc_now(),
        "level": "live_network",
        "status": "PASS" if accepted_terminal_result and not invariant_errors else "FAIL",
        "caller": {"address": caller, "role": "independent_fee_payer"},
        "preconditions": preconditions,
        "epoch_before": epoch_before,
        "epoch_after": epoch_after,
        "tx": dict(attempt),
        "terminal_rejection": terminal_rejection,
        "semantic_error": semantic_error,
        "before": before,
        "after": after,
        "caller_bank_deltas": caller_deltas,
        "invariant_errors": invariant_errors,
        "expected": {
            "deliver_tx_included": True,
            "contract_error": "NothingToRelease",
            "arbitrary_error_accepted": False,
            "state_entitlements_and_counters_unchanged": True,
            "deal_gnk_transfers": 0,
            "all_tracked_cw20_balances_unchanged": True,
            "repeat_payout": 0,
            "only_caller_gnk_fee_may_change": True,
        },
    }
    append_phase(path, phase)
    if semantic_error:
        raise AcceptanceError(
            semantic_error + ": "
            + compact_json(
                {
                    "layer": attempt.get("layer"),
                    "code": attempt.get("code"),
                    "tx_hash": attempt.get("tx_hash"),
                    "raw_log": attempt.get("raw_log"),
                }
            )
        )
    if invariant_errors:
        raise AcceptanceError(
            "terminal ReleaseUnlockedGnk violated terminal no-change invariants: "
            + compact_json(invariant_errors)
        )
    print(compact_json({"status": "pass", "tx_hash": attempt["tx_hash"]}))


def late_donation(args: argparse.Namespace) -> None:
    path = Path(args.context)
    context = load_object(path)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)
    deal = context["contracts"]["deal"]
    caller = context["accounts"]["inactive_host"]
    roles = {
        "host": context["accounts"]["host"],
        "buyer": context["accounts"]["buyer"],
        "fee_recipient": context["accounts"]["fee_recipient"],
        "caller": caller,
        "deal": deal,
    }
    donor_key = f"a8-late-donor-{context['run_id']}"
    donor = gonka.create_key(donor_key)
    if len(set(roles.values()) | {donor}) != 6:
        raise AcceptanceError("late donation requires distinct Deal, donor, Host, Buyer, fee recipient, and caller")
    baseline = terminal_release_snapshot(gonka, context)
    preconditions = assert_terminal_release_preconditions(baseline)
    policy = baseline["state"].get("gnk_release_policy")
    if not isinstance(policy, Mapping) or not isinstance(policy.get("proportional"), Mapping):
        raise AcceptanceError("late donation requires a positive proportional claim policy")
    numerator = require_uint(policy["proportional"].get("buyer_share_numerator"), "late donation buyer numerator")
    denominator = require_uint(policy["proportional"].get("share_denominator"), "late donation denominator")
    if args.amount <= 0 or args.second_amount <= 0:
        raise AcceptanceError("late donation amounts must be positive")
    # Both donations are intentionally equal.  The second cumulative target
    # must differ from independently flooring the same donation, otherwise it
    # would not prove that the contract carries the rounding remainder.
    independent_buyer = args.amount * numerator // denominator
    first_remainder = args.amount * numerator % denominator
    if independent_buyer == 0 or args.amount - independent_buyer == 0 or first_remainder == 0:
        raise AcceptanceError("late donation amount does not produce two non-zero shares and a remainder")
    donor_funding = gonka.tx(
        DEFAULT_NODE,
        "genesis",
        "bank",
        "send",
        gonka.key_address(DEFAULT_NODE, "genesis"),
        donor,
        f"{args.amount + args.second_amount + 1000000}{DEFAULT_DENOM}",
        gas="auto",
    )
    if tx_code(donor_funding) != 0:
        raise AcceptanceError("late donation donor funding failed")

    def release_one(label: str, amount: int) -> dict[str, Any]:
        before = terminal_release_snapshot(gonka, context)
        assert_terminal_release_preconditions(before)
        donor_before = gonka.bank_balance(donor)
        donation_tx = gonka.tx(
            DEFAULT_NODE,
            donor_key,
            "bank",
            "send",
            donor,
            deal,
            f"{amount}{DEFAULT_DENOM}",
            gas="auto",
        )
        if tx_code(donation_tx) != 0:
            raise AcceptanceError(f"late donation {label} Bank send failed")
        assert_exact_bank_transfer_event(donation_tx, donor, deal, amount, DEFAULT_DENOM)
        before_release = terminal_release_snapshot(gonka, context)
        if require_uint(before_release["bank"]["deal"].get(DEFAULT_DENOM, 0), "late donation Deal balance") != amount:
            raise AcceptanceError("late donation did not create the exact liquid Deal balance")
        release_tx = gonka.execute(
            context["key_names"]["inactive_host_node"], context["key_names"]["inactive_host"],
            deal, {"release_unlocked_gnk": {}}, gas="auto"
        )
        if tx_code(release_tx) != 0:
            raise AcceptanceError(f"late donation {label} ReleaseUnlockedGnk was not successful")
        after = terminal_release_snapshot(gonka, context)
        buyer_delta = require_uint(after["bank"]["buyer"].get(DEFAULT_DENOM, 0), "Buyer after") - require_uint(before_release["bank"]["buyer"].get(DEFAULT_DENOM, 0), "Buyer before")
        host_delta = require_uint(after["bank"]["host"].get(DEFAULT_DENOM, 0), "Host after") - require_uint(before_release["bank"]["host"].get(DEFAULT_DENOM, 0), "Host before")
        oracle = assert_release_matches_oracle(before["state"], after["state"], amount, buyer_delta, host_delta)
        if require_uint(after["bank"]["deal"].get(DEFAULT_DENOM, 0), "Deal after") != 0 or buyer_delta + host_delta != amount:
            raise AcceptanceError("late donation release did not conserve exact native amount")
        release_counter_fields = {
            "released_total_ngonka",
            "buyer_released_ngonka",
            "host_released_ngonka",
        }
        before_immutable_state = {
            key: value for key, value in before["state"].items()
            if key not in release_counter_fields
        }
        after_immutable_state = {
            key: value for key, value in after["state"].items()
            if key not in release_counter_fields
        }
        if after_immutable_state != before_immutable_state:
            raise AcceptanceError(
                "late donation changed Deal state outside cumulative GNK counters"
            )
        for section in ("config", "entitlements", "settlement_cw20", "foreign_cw20", "vesting_total", "vesting_schedule"):
            if after[section] != baseline[section]:
                raise AcceptanceError(f"late donation changed immutable {section}")
        if gonka.bank_balance(donor) >= donor_before:
            raise AcceptanceError("late donation donor balance did not decrease")
        return {"label": label, "amount_ngonka": amount, "donation_tx": filtered_tx(donation_tx), "release_tx": filtered_tx(release_tx), "before": before, "before_release": before_release, "after": after, "expected": oracle, "actual": {"buyer_delta": buyer_delta, "host_delta": host_delta}}

    first = release_one("first", args.amount)
    second = release_one("second", args.second_amount)
    if args.second_amount != args.amount or second["expected"]["buyer_delta"] == independent_buyer:
        raise AcceptanceError("late donation scenario did not distinguish cumulative rounding from independent donation rounding")

    append_phase(
        path,
        {
            "name": "late_completed_donation_release",
            "recorded_at_utc": utc_now(),
            "preconditions": preconditions,
            "roles": {**roles, "donor": donor},
            "rounding": {"buyer_numerator": numerator, "denominator": denominator, "independent_buyer_per_donation": independent_buyer, "remainder": first_remainder},
            "donor_funding_tx": filtered_tx(donor_funding),
            "releases": [first, second],
        },
    )
    print(compact_json({"status": "pass", "first_release_tx_hash": tx_hash(first["release_tx"]), "second_release_tx_hash": tx_hash(second["release_tx"])}))


def p0_probe(args: argparse.Namespace) -> None:
    path = Path(args.context)
    context = load_object(path)
    gonka = DockerGonka(Runner(), context["chain"]["chain_id"])
    assert_chain(gonka)

    probe_wasm = Path(args.wasm)
    code_id, store_evidence = gonka.store(probe_wasm, "p0-probe")
    probe, instantiate_evidence = gonka.instantiate(
        code_id,
        {},
        f"a8-p0-probe-{context['run_id']}",
    )
    target_epoch = int(context["terms"]["target_epoch"])
    host = context["accounts"]["host"]
    deal = context["contracts"]["deal"]
    allowed = {
        "get_current_epoch": gonka.smart(probe, {"get_current_epoch": {}}),
        "list_claim_recipients": gonka.smart(
            probe, {"list_claim_recipients": {"participant": host}}
        ),
        "epoch_performance_summary": gonka.smart(
            probe,
            {
                "epoch_performance_summary": {
                    "epoch_index": target_epoch,
                    "participant_id": host,
                }
            },
        ),
        "total_vesting": gonka.smart(
            probe, {"total_vesting": {"participant_address": deal}}
        ),
    }
    denied_path = "/inference.inference.Query/Params"
    denied_message = {"raw_grpc": {"path": denied_path, "data": ""}}
    denied = gonka.cli(
        DEFAULT_NODE,
        "query",
        "wasm",
        "contract-state",
        "smart",
        probe,
        compact_json(denied_message),
        "--output",
        "json",
    )
    if denied.returncode == 0:
        raise AcceptanceError(f"broad gRPC route unexpectedly allowed: {denied_path}")
    denial_text = (denied.stderr or denied.stdout).strip()
    if denied_path not in denial_text:
        raise AcceptanceError(
            "broad gRPC route failed without identifying the denied path"
        )

    append_phase(
        path,
        {
            "name": "p0_wasm_grpc_allowlist",
            "recorded_at_utc": utc_now(),
            "probe": {
                "store": store_evidence,
                "instantiate": instantiate_evidence,
            },
            "allowed_queries": allowed,
            "denied_query": {
                "path": denied_path,
                "returncode": denied.returncode,
                "error": denial_text,
            },
        },
    )
    print(
        compact_json(
            {
                "probe": probe,
                "code_id": code_id,
                "allowed": list(allowed),
                "denied": denied_path,
            }
        )
    )


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)

    bootstrap_parser = commands.add_parser("bootstrap")
    bootstrap_parser.add_argument("--context", required=True)
    bootstrap_parser.add_argument("--run-id", required=True)
    bootstrap_parser.add_argument("--target-epoch", type=int, required=True)
    bootstrap_parser.add_argument("--deal-wasm", required=True)
    bootstrap_parser.add_argument("--factory-wasm", required=True)
    bootstrap_parser.add_argument("--cw20-wasm", required=True)
    bootstrap_parser.add_argument("--caller-wasm", required=True)
    bootstrap_parser.add_argument("--chain-id", default=DEFAULT_CHAIN_ID)
    bootstrap_parser.add_argument("--host-node", default=DEFAULT_HOST_NODE)
    bootstrap_parser.add_argument("--host-key", default=DEFAULT_HOST_KEY)
    bootstrap_parser.add_argument("--buyer-node", default=DEFAULT_BUYER_NODE)
    bootstrap_parser.add_argument("--buyer-key", default=DEFAULT_BUYER_KEY)
    bootstrap_parser.add_argument("--budget", type=int, default=DEFAULT_BUDGET)
    bootstrap_parser.add_argument("--price", type=int, default=DEFAULT_PRICE)
    bootstrap_parser.add_argument("--buyer-tokens", type=int, default=200_000_000)
    bootstrap_parser.add_argument("--expected-initial-epoch-reward", type=int)
    bootstrap_parser.set_defaults(handler=bootstrap)

    create_parser = commands.add_parser("create-deal")
    create_parser.add_argument("--context", required=True)
    create_parser.add_argument("--name", required=True)
    create_parser.add_argument("--target-epoch", type=int, required=True)
    create_parser.add_argument("--host-node", default=DEFAULT_HOST_NODE)
    create_parser.add_argument("--host-key", default=DEFAULT_HOST_KEY)
    create_parser.add_argument("--budget", type=int, default=10_000_000)
    create_parser.add_argument("--price", type=int, default=DEFAULT_PRICE)
    create_parser.add_argument("--route-exact", action="store_true")
    create_parser.add_argument("--fund", action="store_true")
    create_parser.set_defaults(handler=create_deal)

    routing_parser = commands.add_parser("set-scenario-routing")
    routing_parser.add_argument("--context", required=True)
    routing_parser.add_argument("--name", required=True)
    routing_parser.add_argument(
        "--recipient", required=True, choices=("missing", "buyer", "caller")
    )
    routing_parser.set_defaults(handler=set_scenario_routing)

    lock_scenario_parser = commands.add_parser("lock-scenario")
    lock_scenario_parser.add_argument("--context", required=True)
    lock_scenario_parser.add_argument("--name", required=True)
    lock_scenario_parser.set_defaults(handler=lock_scenario)

    exact_parser = commands.add_parser("lock-exact-e-scenario")
    exact_parser.add_argument("--context", required=True)
    exact_parser.add_argument("--name", required=True)
    exact_parser.set_defaults(handler=lock_exact_e_scenario)

    lock_e_plus_4_parser = commands.add_parser("lock-e-plus-4-scenario")
    lock_e_plus_4_parser.add_argument("--context", required=True)
    lock_e_plus_4_parser.add_argument("--name", required=True)
    lock_e_plus_4_parser.set_defaults(handler=lock_e_plus_4_scenario)

    lock_e_plus_5_parser = commands.add_parser("lock-e-plus-5-scenario")
    lock_e_plus_5_parser.add_argument("--context", required=True)
    lock_e_plus_5_parser.add_argument("--name", required=True)
    lock_e_plus_5_parser.add_argument("--gas", type=int, default=2_000_000)
    lock_e_plus_5_parser.set_defaults(handler=lock_e_plus_5_rejected_scenario)

    refund_e_plus_5_parser = commands.add_parser("refund-e-plus-5-scenario")
    refund_e_plus_5_parser.add_argument("--context", required=True)
    refund_e_plus_5_parser.add_argument("--name", required=True)
    refund_e_plus_5_parser.add_argument("--gas", type=int, default=2_000_000)
    refund_e_plus_5_parser.add_argument("--caller-node", default=DEFAULT_NODE)
    refund_e_plus_5_parser.add_argument("--caller-key", default="genesis")
    refund_e_plus_5_parser.set_defaults(handler=refund_e_plus_5_rejected_scenario)

    lock_rejected_parser = commands.add_parser("lock-rejected-scenario")
    lock_rejected_parser.add_argument("--context", required=True)
    lock_rejected_parser.add_argument("--name", required=True)
    lock_rejected_parser.add_argument("--routing", choices=("present", "pruned"), required=True)
    lock_rejected_parser.add_argument("--gas", type=int, default=2_000_000)
    lock_rejected_parser.set_defaults(handler=lock_rejected_scenario)

    claim_scenario_parser = commands.add_parser("claim-scenario")
    claim_scenario_parser.add_argument("--context", required=True)
    claim_scenario_parser.add_argument("--name", required=True)
    claim_scenario_parser.add_argument("--reward-seed", type=int, required=True)
    claim_scenario_parser.add_argument("--reward-epoch", type=int, required=True)
    claim_scenario_parser.set_defaults(handler=claim_scenario)

    verify_claimed_parser = commands.add_parser("verify-claimed-scenario")
    verify_claimed_parser.add_argument("--context", required=True)
    verify_claimed_parser.add_argument("--name", required=True)
    verify_claimed_parser.add_argument("--require-positive", action="store_true")
    verify_claimed_parser.add_argument("--wait-seconds", type=int, default=120)
    verify_claimed_parser.set_defaults(handler=verify_claimed_scenario)

    verify_unclaimed_parser = commands.add_parser("verify-unclaimed-scenario")
    verify_unclaimed_parser.add_argument("--context", required=True)
    verify_unclaimed_parser.add_argument("--name", required=True)
    unclaimed_total = verify_unclaimed_parser.add_mutually_exclusive_group()
    unclaimed_total.add_argument("--require-positive", action="store_true")
    unclaimed_total.add_argument("--require-zero", action="store_true")
    verify_unclaimed_parser.add_argument("--wait-seconds", type=int, default=120)
    verify_unclaimed_parser.set_defaults(handler=verify_unclaimed_scenario)

    verify_missing_parser = commands.add_parser("verify-missing-summary-scenario")
    verify_missing_parser.add_argument("--context", required=True)
    verify_missing_parser.add_argument("--name", required=True)
    verify_missing_parser.add_argument(
        "--expected-offset", type=int, choices=(2, 3), required=True
    )
    verify_missing_parser.set_defaults(handler=verify_missing_summary_scenario)

    settle_scenario_parser = commands.add_parser("settle-scenario")
    settle_scenario_parser.add_argument("--context", required=True)
    settle_scenario_parser.add_argument("--name", required=True)
    settle_scenario_parser.set_defaults(handler=settle_scenario)

    release_scenario_parser = commands.add_parser("release-scenario")
    release_scenario_parser.add_argument("--context", required=True)
    release_scenario_parser.add_argument("--name", required=True)
    release_scenario_parser.set_defaults(handler=release_scenario)

    bank_rollback_parser = commands.add_parser("bank-release-rollback-scenario")
    bank_rollback_parser.add_argument("--context", required=True)
    bank_rollback_parser.add_argument("--name", required=True)
    bank_rollback_parser.add_argument("--gas", type=int, default=2_000_000)
    bank_rollback_parser.add_argument("--proposal-id", required=True)
    bank_rollback_parser.add_argument("--expected-send-index", type=int, choices=(1, 2), required=True)
    bank_rollback_parser.add_argument("--allowed-earlier-recipient")
    bank_rollback_parser.add_argument("--rejected-recipient", required=True)
    bank_rollback_parser.add_argument("--exemption-id")
    bank_rollback_parser.set_defaults(handler=bank_release_rollback_scenario)

    bank_plan_parser = commands.add_parser("bank-release-fault-plan")
    bank_plan_parser.add_argument("--require-fully-vested", action="store_true")
    bank_plan_parser.add_argument("--context", required=True)
    bank_plan_parser.add_argument("--name", required=True)
    bank_plan_parser.set_defaults(handler=bank_release_fault_plan)

    bank_retry_parser = commands.add_parser("bank-release-retry-scenario")
    bank_retry_parser.add_argument("--context", required=True)
    bank_retry_parser.add_argument("--name", required=True)
    bank_retry_parser.add_argument("--gas", type=int, default=2_000_000)
    bank_retry_parser.add_argument("--expected-send-index", type=int, choices=(1, 2), required=True)
    bank_retry_parser.add_argument("--rejected-recipient", required=True)
    bank_retry_parser.set_defaults(handler=bank_release_retry_scenario)

    scenario_repeat_parser = commands.add_parser("scenario-release-repeat")
    scenario_repeat_parser.add_argument("--context", required=True)
    scenario_repeat_parser.add_argument("--name", required=True)
    scenario_repeat_parser.add_argument("--gas", type=int, default=2_000_000)
    scenario_repeat_parser.set_defaults(handler=scenario_release_repeat)

    gas_parser = commands.add_parser("gas-sweep-scenario")
    gas_parser.add_argument("--context", required=True)
    gas_parser.add_argument("--name", required=True)
    gas_parser.add_argument(
        "--gas-limits",
        type=int,
        nargs="+",
        default=[50_000, 100_000, 150_000, 250_000, 500_000, 1_000_000],
    )
    gas_parser.add_argument("--sufficient-gas", type=int, default=2_000_000)
    gas_parser.add_argument("--outer-gas", type=int, default=2_000_000)
    gas_parser.set_defaults(handler=gas_sweep_scenario)

    refund_parser = commands.add_parser("refund-scenario")
    refund_parser.add_argument("--context", required=True)
    refund_parser.add_argument("--name", required=True)
    refund_parser.add_argument("--expect", choices=("success", "failure"), required=True)
    refund_parser.add_argument(
        "--reason",
        choices=(
            "routing_missing",
            "routing_mismatch",
            "claim_expiry",
            "network_unconfirmed",
            "network_unconfirmed_too_early",
            "too_early",
            "claimed",
        ),
        required=True,
    )
    refund_parser.add_argument("--gas", type=int, default=2_000_000)
    refund_parser.add_argument("--fault-cw20", action="store_true")
    refund_parser.set_defaults(handler=refund_scenario)

    donation_parser = commands.add_parser("donate-scenario")
    donation_parser.add_argument("--context", required=True)
    donation_parser.add_argument("--name", required=True)
    donation_parser.add_argument("--label", required=True)
    donation_parser.add_argument("--amount", type=int, required=True)
    donation_parser.set_defaults(handler=donate_scenario)

    contamination_parser = commands.add_parser("contaminate-scenario")
    contamination_parser.add_argument("--context", required=True)
    contamination_parser.add_argument("--name", required=True)
    contamination_parser.add_argument("--amount", type=int, default=7)
    contamination_parser.set_defaults(handler=contaminate_scenario)

    vesting_snapshot_parser = commands.add_parser("snapshot-vesting-scenario")
    vesting_snapshot_parser.add_argument("--context", required=True)
    vesting_snapshot_parser.add_argument("--name", required=True)
    vesting_snapshot_parser.add_argument("--label", required=True)
    vesting_snapshot_parser.add_argument("--require-non-empty", action="store_true")
    vesting_snapshot_parser.set_defaults(handler=snapshot_vesting_scenario)

    vesting_addition_parser = commands.add_parser("verify-vesting-addition-scenario")
    vesting_addition_parser.add_argument("--context", required=True)
    vesting_addition_parser.add_argument("--name", required=True)
    vesting_addition_parser.add_argument("--before-label", required=True)
    vesting_addition_parser.add_argument("--amount", type=int, required=True)
    vesting_addition_parser.add_argument("--vesting-epochs", type=int, required=True)
    vesting_addition_parser.add_argument("--fund-tx-hash", required=True)
    vesting_addition_parser.add_argument("--proposal-id", required=True)
    vesting_addition_parser.add_argument("--allow-empty-before", action="store_true")
    vesting_addition_parser.set_defaults(handler=verify_vesting_addition_scenario)

    r2_checkpoint_parser = commands.add_parser("r2-gift-checkpoint")
    r2_checkpoint_parser.add_argument("--context", required=True)
    r2_checkpoint_parser.add_argument("--name", required=True)
    r2_checkpoint_parser.add_argument(
        "--stage", choices=("pre_gift", "fully_locked", "first_unlocked", "final"), required=True
    )
    r2_checkpoint_parser.add_argument("--gift-amount", type=int, default=0)
    r2_checkpoint_parser.set_defaults(handler=r2_gift_checkpoint)

    isolation_parser = commands.add_parser("verify-factory-isolation")
    isolation_parser.add_argument("--context", required=True)
    isolation_parser.add_argument("--gas", type=int, default=2_000_000)
    isolation_parser.set_defaults(handler=verify_factory_isolation)

    lock_parser = commands.add_parser("lock")
    lock_parser.add_argument("--context", required=True)
    lock_parser.set_defaults(handler=lock)

    settle_parser = commands.add_parser("claim-settle")
    settle_parser.add_argument("--context", required=True)
    settle_parser.add_argument("--reward-seed", type=int, required=True)
    settle_parser.add_argument("--reward-epoch", type=int, required=True)
    settle_parser.add_argument("--host-node", default=DEFAULT_HOST_NODE)
    settle_parser.add_argument("--fault-retry", action="store_true")
    settle_parser.add_argument(
        "--cw20-fault-positions",
        type=lambda raw: [int(part) for part in raw.split(",") if part],
        help="strictly increasing R6.1 payout positions (1=Host, 2=fee, 3=Buyer)",
    )
    settle_parser.set_defaults(handler=claim_settle)

    withdraw_parser = commands.add_parser("withdraw-usdt")
    withdraw_parser.add_argument("--context", required=True)
    withdraw_parser.add_argument("--name", help="named scenario instead of the primary Deal")
    withdraw_parser.set_defaults(handler=withdraw_usdt)

    release_parser = commands.add_parser("release")
    release_parser.add_argument("--context", required=True)
    release_parser.set_defaults(handler=release)

    b3_release_parser = commands.add_parser("b3-foreign-native-release")
    b3_release_parser.add_argument("--context", required=True)
    b3_release_parser.add_argument("--foreign-key", required=True)
    b3_release_parser.add_argument("--foreign-address", required=True)
    b3_release_parser.add_argument("--foreign-denom", required=True)
    b3_release_parser.add_argument("--foreign-amount", type=int, required=True)
    b3_release_parser.set_defaults(handler=b3_foreign_native_release)

    terminal_release_parser = commands.add_parser("terminal-release-repeat")
    terminal_release_parser.add_argument("--context", required=True)
    terminal_release_parser.add_argument("--gas", type=int, default=2_000_000)
    terminal_release_parser.set_defaults(handler=terminal_release_repeat)

    donation_parser = commands.add_parser("late-donation")
    donation_parser.add_argument("--context", required=True)
    donation_parser.add_argument("--amount", type=int, default=1_423)
    donation_parser.add_argument("--second-amount", type=int, default=1_423)
    donation_parser.set_defaults(handler=late_donation)

    p0_parser = commands.add_parser("p0-probe")
    p0_parser.add_argument("--context", required=True)
    p0_parser.add_argument("--wasm", required=True)
    p0_parser.set_defaults(handler=p0_probe)

    run_parser = commands.add_parser("run-live")
    from a8_query_faults import PHASES, run as run_query_faults
    c_parser = commands.add_parser("c-phase")
    c_parser.add_argument("--context", required=True)
    c_parser.add_argument("--phase", choices=PHASES, required=True)
    c_parser.add_argument("--proposal-id")
    c_parser.set_defaults(handler=lambda args: run_query_faults(args, sys.modules[__name__]))
    run_parser.add_argument("--marketplace-dir", default=".")
    run_parser.add_argument("--gonka-dir", required=True)
    run_parser.add_argument(
        "--overlay-dir",
        default=str(DEFAULT_GONKA_OVERLAY_DIR),
        help="path to gonka-overlay directory in smart contract repo",
    )
    run_parser.add_argument(
        "--temp-gonka-dir",
        help="explicit directory for temporary Gonka workspace copy",
    )
    run_parser.add_argument(
        "--keep-temp-gonka",
        action="store_true",
        help="keep temporary Gonka workspace after test run",
    )
    run_parser.add_argument(
        "--no-temp-gonka",
        action="store_true",
        help="run directly in --gonka-dir without creating a temporary copy",
    )
    run_parser.add_argument(
        "--manifest",
        help="verified A9 build manifest for current HEAD; omitted builds one first",
    )
    run_parser.add_argument("--evidence-dir", default="artifacts/a8-evidence")
    run_parser.add_argument("--run-id")
    run_parser.add_argument(
        "--scenario",
        choices=(
            "full",
            "claim-expiry-positive",
            "claim-expiry-zero",
            "network-unconfirmed",
            "terminal-release-repeat",
            "b3-foreign-native",
            "late-donation-after-completed",
            "lock-exact-e",
            "lock-e-plus-4",
            "lock-e-plus-5",
            "package-a-r1-r2",
            "package-b-r6-1",
            "package-b-r7-1",
            "package-c-query-faults",
        ),
        default="full",
        help="run the full matrix or one isolated acceptance scenario",
    )
    run_parser.add_argument("--timeout-minutes", type=int, default=35)
    run_parser.set_defaults(handler=run_live)
    return root


def main() -> int:
    args = parser().parse_args()
    try:
        args.handler(args)
    except (AcceptanceError, subprocess.TimeoutExpired, OSError, KeyError, ValueError) as exc:
        print(f"A8 acceptance failed: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
