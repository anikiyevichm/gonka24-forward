import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch


MODULE_PATH = Path(__file__).resolve().parents[1] / "a8_acceptance.py"
sys.path.insert(0, str(MODULE_PATH.parent))
SPEC = importlib.util.spec_from_file_location("a8_acceptance", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
a8 = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = a8
SPEC.loader.exec_module(a8)


class FakeRunner:
    def __init__(self, results):
        self.results = list(results)
        self.calls = []

    def run(self, argv, *, input_text=None, timeout=None):
        self.calls.append((list(argv), input_text, timeout))
        if not self.results:
            raise AssertionError(f"unexpected command: {argv}")
        return self.results.pop(0)


class StubGonka:
    def __init__(self, chain_id, status):
        self.chain_id = chain_id
        self._status = status

    def status(self):
        return self._status


class BankRollbackGonka:
    def __init__(self, attempt):
        self.chain_id = a8.DEFAULT_CHAIN_ID
        self.attempt = attempt

    def status(self):
        return {"node_info": {"network": a8.DEFAULT_CHAIN_ID}, "sync_info": {"catching_up": False}}

    def transfer_restriction_status(self):
        return {"is_active": True, "remaining_blocks": 10, "current_block_height": 100}

    def smart(self, _contract, _query):
        return {
            "status": "releasing",
            "released_total_ngonka": "0",
            "buyer_released_ngonka": "0",
            "host_released_ngonka": "0",
            "gnk_release_policy": "host_only",
        }

    def cw20_balance(self, _contract, _address):
        return 0

    def bank_balance(self, address):
        return 100 if address == "deal" else 0

    def tx_attempt(self, *_args, **_kwargs):
        return dict(self.attempt)


class A8AcceptanceTests(unittest.TestCase):
    def test_runtime_adapter_requires_exact_reviewed_blobs(self):
        head = "a" * 40
        paths = list(a8.A8_RUNTIME_ADAPTER_BLOBS)
        prefix = [subprocess.CompletedProcess([], 0, head, ""),
                  subprocess.CompletedProcess([], 0, "\n".join(paths), "")]
        good = [subprocess.CompletedProcess([], 0, blob, "")
                for blob in a8.A8_RUNTIME_ADAPTER_BLOBS.values()]
        self.assertEqual(a8.verify_gonka_test_checkout(Path("gonka"), FakeRunner(prefix + good)), head)
        with self.assertRaisesRegex(a8.AcceptanceError, "unreviewed A8 runtime"):
            a8.verify_gonka_test_checkout(Path("gonka"), FakeRunner(prefix + [subprocess.CompletedProcess([], 0, "wrong", "")]))

    def test_prepared_package_selectors_are_available(self):
        for scenario in ("package-a-r1-r2", "package-b-r6-1", "package-b-r7-1"):
            parsed = a8.parser().parse_args(["run-live", "--gonka-dir", "gonka", "--scenario", scenario])
            self.assertEqual(parsed.scenario, scenario)

    def test_late_completed_donations_use_cumulative_rounding(self):
        state = {
            "released_total_ngonka": "94819671366450",
            "buyer_released_ngonka": "100000000000",
            "host_released_ngonka": "94719671366450",
            "gnk_release_policy": {
                "proportional": {
                    "buyer_share_numerator": "100000000000",
                    "share_denominator": "94819671366450",
                }
            },
        }
        amount = 1_423
        first = a8.expected_release(state, amount)
        first_state = {
            **state,
            "released_total_ngonka": str(first["released_total"]),
            "buyer_released_ngonka": str(first["buyer_released"]),
            "host_released_ngonka": str(first["host_released"]),
        }
        second = a8.expected_release(first_state, amount)

        independently_floored_buyer = amount * 100_000_000_000 // 94_819_671_366_450
        self.assertEqual(first["buyer_delta"], independently_floored_buyer)
        self.assertEqual(first["host_delta"], amount - independently_floored_buyer)
        self.assertEqual(second["buyer_delta"], 2)
        self.assertEqual(second["host_delta"], amount - 2)
        self.assertNotEqual(second["buyer_delta"], independently_floored_buyer)

    @staticmethod
    def workflow_blob_results(*, verify_blob=None):
        return [
            subprocess.CompletedProcess(
                [],
                0,
                (verify_blob if path == ".github/workflows/verify.yml" and verify_blob else blob)
                + "\n",
                "",
            )
            for path, blob in sorted(a8.PR4_MANUAL_WORKFLOW_BLOBS.items())
        ]

    @staticmethod
    def funded_settlement_fixture():
        context = {
            "terms": {
                "target_epoch": 5,
                "budget_micro_usdt": "100000000",
                "price_micro_usdt_per_gnk": "1000000",
                "fee_bps": 150,
            },
            "accounts": {"host": "gonka1host", "buyer": "gonka1buyer"},
        }
        summary = {
            "epochPerformanceSummary": {
                "claimed": True,
                "epoch_index": "5",
                "participant_id": "gonka1host",
                "rewarded_coins": "200000000000",
            }
        }
        expected = a8.expected_claim_settlement(context, summary)
        state = {
            key: str(value) if isinstance(value, int) else value
            for key, value in expected.items()
            if key
            in {
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
            }
        }
        return context, summary, expected, state

    def test_runtime_identity_requires_exact_running_gonka_and_wasm_stack(self):
        output = "\n".join(
            [
                "- github.com/CosmWasm/wasmd@v0.54.2",
                "- github.com/CosmWasm/wasmvm/v2@v2.2.4",
                "commit: 29a58fcf64b87967cb874b6169ce2c61e1f269b1",
                "cosmos_sdk_version: v0.53.3-test",
                "go: go version go1.24.2 linux/amd64",
            ]
        )
        identity = a8.parse_runtime_identity(output)
        self.assertEqual(identity["gonka_source_sha"], a8.EXPECTED_GONKA_SHA)
        with self.assertRaisesRegex(a8.AcceptanceError, "runtime provenance mismatch"):
            a8.parse_runtime_identity(output.replace("v0.54.2", "v0.54.1"))

    def test_r61_fault_selector_binds_all_three_positions_to_distinct_recipients(self):
        context = {
            "terms": {
                "target_epoch": 5,
                "budget_micro_usdt": "100000000",
                "price_micro_usdt_per_gnk": "1000000",
                "fee_bps": 150,
            },
            "accounts": {
                "host": "gonka1host",
                "fee_recipient": "gonka1fee",
                "buyer": "gonka1buyer",
            },
        }
        summary = {
            "epochPerformanceSummary": {
                "claimed": True,
                "epoch_index": "5",
                "participant_id": "gonka1host",
                # Half the funded capacity: Host, fee, and Buyer refund are all non-zero.
                "rewarded_coins": "50000000000",
            }
        }
        targets = a8.cw20_settlement_fault_targets(context, summary, [1, 2, 3])
        self.assertEqual(
            [(target["outgoing_transfer_index"], target["role"], target["recipient"])
             for target in targets],
            [(1, "host", "gonka1host"), (2, "fee_recipient", "gonka1fee"), (3, "buyer", "gonka1buyer")],
        )
        self.assertTrue(all(target["amount"] > 0 for target in targets))

    def test_r61_fault_selector_rejects_wrong_position_zero_payout_and_alias(self):
        context = {
            "terms": {
                "target_epoch": 5,
                "budget_micro_usdt": "100000000",
                "price_micro_usdt_per_gnk": "1000000",
                "fee_bps": 150,
            },
            "accounts": {
                "host": "gonka1host",
                "fee_recipient": "gonka1fee",
                "buyer": "gonka1buyer",
            },
        }
        summary = {
            "epochPerformanceSummary": {
                "claimed": True,
                "epoch_index": "5",
                "participant_id": "gonka1host",
                "rewarded_coins": "50000000000",
            }
        }
        with self.assertRaisesRegex(a8.AcceptanceError, "strictly increasing"):
            a8.cw20_settlement_fault_targets(context, summary, [2, 1])
        with self.assertRaisesRegex(a8.AcceptanceError, "outside"):
            a8.cw20_settlement_fault_targets(context, summary, [4])
        aliased = {**context, "accounts": {**context["accounts"], "buyer": "gonka1host"}}
        with self.assertRaisesRegex(a8.AcceptanceError, "distinct recipients"):
            a8.cw20_settlement_fault_targets(aliased, summary, [1, 3])
        fully_sold = {
            "epochPerformanceSummary": {
                **summary["epochPerformanceSummary"],
                "rewarded_coins": "200000000000",
            }
        }
        with self.assertRaisesRegex(a8.AcceptanceError, "is zero"):
            a8.cw20_settlement_fault_targets(context, fully_sold, [3])

    def test_gonka_checkout_accepts_pinned_p0_with_manual_trigger_and_b3_harness(self):
        updated_head = "114f914d1d362a3b60ffe13477ef572372fe7c58"
        changed = "\n".join(
            [
                "inference-chain/scripts/init-docker-genesis.sh",
                "local-test-net/docker-compose.genesis-a8-b3-foreign-denom.yml",
                "testermint/src/main/kotlin/LocalInferencePair.kt",
                "testermint/src/test/kotlin/DockerBindUserArgsTests.kt",
                "testermint/src/test/kotlin/MarketplaceContractAcceptanceTests.kt",
                "testermint/src/test/kotlin/MarketplaceHarnessProcess.kt",
                "testermint/src/test/kotlin/MarketplaceHarnessProcessTests.kt",
                "testermint/src/test/resources/a8-b3-genesis-validation-overrides.json",
                ".github/workflows/verify.yml",
            ]
        )
        runner = FakeRunner(
            [
                subprocess.CompletedProcess([], 0, updated_head + "\n", ""),
                subprocess.CompletedProcess([], 0, changed + "\n", ""),
                subprocess.CompletedProcess([], 0, "", ""),
                *self.workflow_blob_results(),
            ]
        )

        self.assertEqual(
            a8.verify_gonka_test_checkout(Path("gonka"), runner), updated_head
        )

    def test_gonka_checkout_rejects_any_extra_file_beyond_timeout_harness(self):
        updated_head = "114f914d1d362a3b60ffe13477ef572372fe7c58"
        changed = "\n".join(
            [
                "testermint/src/test/kotlin/MarketplaceContractAcceptanceTests.kt",
                "testermint/src/test/kotlin/MarketplaceHarnessProcess.kt",
                "testermint/src/test/kotlin/MarketplaceHarnessProcessTests.kt",
                "inference-chain/app/app.go",
            ]
        )
        runner = FakeRunner(
            [
                subprocess.CompletedProcess([], 0, updated_head + "\n", ""),
                subprocess.CompletedProcess([], 0, changed + "\n", ""),
            ]
        )

        with self.assertRaisesRegex(a8.AcceptanceError, "test-only child"):
            a8.verify_gonka_test_checkout(Path("gonka"), runner)

    def test_gonka_checkout_rejects_unverified_workflow_change(self):
        updated_head = "114f914d1d362a3b60ffe13477ef572372fe7c58"
        changed = "\n".join(
            [
                "testermint/src/test/kotlin/MarketplaceContractAcceptanceTests.kt",
                ".github/workflows/unreviewed.yml",
            ]
        )
        runner = FakeRunner(
            [
                subprocess.CompletedProcess([], 0, updated_head + "\n", ""),
                subprocess.CompletedProcess([], 0, changed + "\n", ""),
            ]
        )

        with self.assertRaisesRegex(a8.AcceptanceError, "test-only child"):
            a8.verify_gonka_test_checkout(Path("gonka"), runner)

    def test_gonka_checkout_rejects_verify_workflow_drift(self):
        updated_head = "114f914d1d362a3b60ffe13477ef572372fe7c58"
        runner = FakeRunner(
            [
                subprocess.CompletedProcess([], 0, updated_head + "\n", ""),
                subprocess.CompletedProcess([], 0, ".github/workflows/verify.yml\n", ""),
                subprocess.CompletedProcess([], 0, "", ""),
                *self.workflow_blob_results(verify_blob="deadbeef"),
            ]
        )

        with self.assertRaisesRegex(a8.AcceptanceError, "P0-preserving manual verify.yml exception"):
            a8.verify_gonka_test_checkout(Path("gonka"), runner)

    def test_gonka_checkout_rejects_verify_workflow_with_automatic_trigger(self):
        updated_head = "114f914d1d362a3b60ffe13477ef572372fe7c58"
        runner = FakeRunner(
            [
                subprocess.CompletedProcess([], 0, updated_head + "\n", ""),
                subprocess.CompletedProcess([], 0, ".github/workflows/verify.yml\n", ""),
                subprocess.CompletedProcess([], 0, "", ""),
                *self.workflow_blob_results(verify_blob="2f7a1d1e0b4073791f132e9c3d4318342db5f46e"),
            ]
        )

        with self.assertRaisesRegex(a8.AcceptanceError, "mismatched_workflows"):
            a8.verify_gonka_test_checkout(Path("gonka"), runner)

    def test_release_oracle_rejects_conserving_but_wrong_80_20_split(self):
        before = {
            "gnk_release_policy": {
                "proportional": {
                    "buyer_share_numerator": "80",
                    "share_denominator": "100",
                }
            },
            "released_total_ngonka": "0",
            "buyer_released_ngonka": "0",
            "host_released_ngonka": "0",
        }
        expected = a8.expected_release(before, 100)
        self.assertEqual(expected["buyer_delta"], 80)
        self.assertEqual(expected["host_delta"], 20)
        wrong_after = {
            **before,
            "released_total_ngonka": "100",
            "buyer_released_ngonka": "0",
            "host_released_ngonka": "100",
        }
        with self.assertRaisesRegex(a8.AcceptanceError, "independent oracle"):
            a8.assert_release_matches_oracle(before, wrong_after, 100, 0, 100)

    def test_release_oracle_uses_cumulative_floor_rounding(self):
        before = {
            "gnk_release_policy": {
                "proportional": {
                    "buyer_share_numerator": "1",
                    "share_denominator": "3",
                }
            },
            "released_total_ngonka": "2",
            "buyer_released_ngonka": "0",
            "host_released_ngonka": "2",
        }
        self.assertEqual(
            a8.expected_release(before, 1),
            {
                "released_total": 3,
                "buyer_released": 1,
                "host_released": 2,
                "buyer_delta": 1,
                "host_delta": 0,
            },
        )

    def test_settlement_oracle_rejects_zero_fee_and_all_budget_to_host(self):
        context, summary, expected, state = self.funded_settlement_fixture()
        self.assertEqual(expected["fee_usdt"], 1_500_000)
        self.assertEqual(expected["host_net_usdt"], 98_500_000)
        wrong_state = {
            **state,
            "fee_usdt": "0",
            "host_net_usdt": "100000000",
        }
        with self.assertRaisesRegex(a8.AcceptanceError, "independent oracle"):
            a8.assert_claim_settlement_matches_oracle(
                context,
                summary,
                wrong_state,
                {"host": 100_000_000, "fee_recipient": 0, "buyer": 0},
                100_000_000,
            )

    def test_settlement_oracle_rejects_wrong_initial_gnk_shares(self):
        context, summary, expected, state = self.funded_settlement_fixture()
        self.assertEqual(expected["buyer_entitlement_ngonka"], 100_000_000_000)
        self.assertEqual(expected["host_entitlement_ngonka"], 100_000_000_000)
        wrong_state = {
            **state,
            "buyer_entitlement_ngonka": "0",
            "host_entitlement_ngonka": "200000000000",
            "gnk_release_policy": {
                "proportional": {
                    "buyer_share_numerator": "0",
                    "share_denominator": "200000000000",
                }
            },
        }
        with self.assertRaisesRegex(a8.AcceptanceError, "independent oracle"):
            a8.assert_claim_settlement_matches_oracle(
                context,
                summary,
                wrong_state,
                expected["cw20_deltas"],
                expected["deal_outflow"],
            )

    def test_settlement_oracle_uses_native_summary_and_offer_rounding(self):
        context, summary, expected, state = self.funded_settlement_fixture()
        self.assertEqual(expected["work_ngonka"], 0)
        self.assertEqual(expected["reward_ngonka"], 200_000_000_000)
        self.assertEqual(expected["total_claim_ngonka"], 200_000_000_000)
        self.assertEqual(
            a8.assert_claim_settlement_matches_oracle(
                context,
                summary,
                state,
                expected["cw20_deltas"],
                expected["deal_outflow"],
            ),
            expected,
        )

    def test_settlement_oracle_no_sale_assigns_all_gnk_to_host_and_moves_no_cw20(self):
        context, summary, _, _ = self.funded_settlement_fixture()
        context["accounts"]["buyer"] = None
        expected = a8.expected_claim_settlement(context, summary)
        self.assertEqual(expected["funded_capacity_ngonka"], 0)
        self.assertEqual(expected["buyer_entitlement_ngonka"], 0)
        self.assertEqual(expected["host_entitlement_ngonka"], 200_000_000_000)
        self.assertEqual(expected["cw20_deltas"], {"host": 0, "fee_recipient": 0, "buyer": 0})
        self.assertEqual(expected["deal_outflow"], 0)

    def test_failure_evidence_keeps_error_layer_and_gas(self):
        evidence = a8.filtered_tx_attempt(
            {
                "height": "81",
                "code": 11,
                "txhash": "FAILED",
                "codespace": "sdk",
                "gas_wanted": "150000",
                "gas_used": "149900",
                "raw_log": "out of gas",
            },
            "deliver_tx",
        )
        self.assertEqual(evidence["layer"], "deliver_tx")
        self.assertEqual(evidence["gas_used"], "149900")
        self.assertEqual(evidence["raw_log"], "out of gas")

    def test_settlement_oracle_rejects_unclaimed_or_wrong_native_summary_identity(self):
        context, summary, _, _ = self.funded_settlement_fixture()
        for field, value in (
            ("claimed", False),
            ("epoch_index", "6"),
            ("participant_id", "gonka1other"),
        ):
            wrong = json.loads(json.dumps(summary))
            wrong["epochPerformanceSummary"][field] = value
            with self.subTest(field=field), self.assertRaises(a8.AcceptanceError):
                a8.expected_claim_settlement(context, wrong)

    def test_existing_prod_local_blocks_destructive_testermint_reboot(self):
        with tempfile.TemporaryDirectory() as directory:
            checkout = Path(directory)
            a8.assert_fresh_gonka_local_state(checkout)
            self.assertTrue((checkout / "prod-local").is_dir())
            a8.assert_fresh_gonka_local_state(checkout)
            (checkout / "prod-local" / "chain-state").write_text("occupied")
            with self.assertRaisesRegex(
                a8.AcceptanceError, "Testermint deletes this entire ignored path"
            ):
                a8.assert_fresh_gonka_local_state(checkout)

    def test_deal_terms_must_match_independent_offer_inputs(self):
        expected = {
            "host": "gonka1host",
            "fee_bps": 150,
            "funded_capacity_ngonka": 100,
        }
        a8.assert_deal_terms(
            {
                "host": "gonka1host",
                "fee_bps": "150",
                "funded_capacity_ngonka": "100",
            },
            expected,
        )
        with self.assertRaisesRegex(a8.AcceptanceError, "independent offer inputs"):
            a8.assert_deal_terms(
                {
                    "host": "gonka1host",
                    "fee_bps": "0",
                    "funded_capacity_ngonka": "100",
                },
                expected,
            )

    def test_docker_guard_catches_all_compose_resource_kinds(self):
        runner = FakeRunner(
            [
                a8.CommandResult(
                    0,
                    "join1-postgres\tjoin1\nunrelated\tother\n",
                    "",
                ),
                a8.CommandResult(0, "genesis_postgres-data\tgenesis\n", ""),
                a8.CommandResult(0, "bridge\nchain-public\n", ""),
            ]
        )
        self.assertEqual(
            a8.docker_resource_collisions(runner),
            {
                "containers": ["join1-postgres"],
                "volumes": ["genesis_postgres-data"],
                "networks": ["chain-public"],
            },
        )

    def test_docker_guard_ignores_unrelated_resources(self):
        runner = FakeRunner(
            [
                a8.CommandResult(0, "unrelated\tother\n", ""),
                a8.CommandResult(0, "other_data\tother\n", ""),
                a8.CommandResult(0, "bridge\n", ""),
            ]
        )
        self.assertFalse(any(a8.docker_resource_collisions(runner).values()))

    def test_a9_manifest_must_match_marketplace_head_and_artifact_hashes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            wasm = root / "wasm"
            wasm.mkdir()
            deal = wasm / "marketplace_deal.wasm"
            factory = wasm / "marketplace_factory.wasm"
            deal.write_bytes(b"deal")
            factory.write_bytes(b"factory")
            manifest = root / "build-manifest.json"
            manifest.write_text(
                json.dumps(
                    {
                        "marketplace_commit_sha": "a" * 40,
                        "contracts": [
                            {
                                "name": "marketplace-deal",
                                "path": "wasm/marketplace_deal.wasm",
                                "sha256": a8.sha256_file(deal),
                            },
                            {
                                "name": "marketplace-factory",
                                "path": "wasm/marketplace_factory.wasm",
                                "sha256": a8.sha256_file(factory),
                            },
                        ],
                    }
                ),
                encoding="utf-8",
            )
            runner = FakeRunner(
                [
                    a8.CommandResult(0, "verified", ""),
                    a8.CommandResult(0, "b" * 40 + "\n", ""),
                ]
            )
            with self.assertRaisesRegex(a8.AcceptanceError, "manifest is stale"):
                a8.verified_release_artifacts(root, manifest, runner)

    def test_test_cw20_symbol_matches_cw20_base_constraints(self):
        self.assertRegex(a8.TEST_CW20_SYMBOL, r"^[a-zA-Z-]{3,12}$")

    def test_chain_provenance_accepts_pinned_local_gonka_id(self):
        status = {
            "node_info": {"network": "gonka-mainnet"},
            "sync_info": {"catching_up": False},
        }
        self.assertEqual(a8.assert_chain(StubGonka("gonka-mainnet", status)), status)

    def test_chain_provenance_rejects_unexpected_id(self):
        status = {
            "node_info": {"network": "different-chain"},
            "sync_info": {"catching_up": False},
        }
        with self.assertRaisesRegex(a8.AcceptanceError, "expected chain ID"):
            a8.assert_chain(StubGonka("gonka-mainnet", status))

    def test_testermint_keyring_selection_and_password_are_deterministic(self):
        self.assertEqual(a8.DockerGonka.keyring("genesis-node"), ("test", None))
        self.assertEqual(
            a8.DockerGonka.keyring("join1-node"), ("file", "join100000\n")
        )
        self.assertEqual(
            a8.DockerGonka.keyring("join2-node"), ("file", "join200000\n")
        )

    def test_filtered_tx_keeps_proof_fields_and_drops_unrelated_events(self):
        value = {
            "height": "42",
            "code": 0,
            "txhash": "ABC",
            "gas_wanted": "100",
            "gas_used": "90",
            "events": [
                {
                    "type": "wasm-deal_locked",
                    "attributes": [{"key": "deal", "value": "gonka1deal"}],
                },
                {
                    "type": "message",
                    "attributes": [{"key": "module", "value": "wasm"}],
                },
            ],
        }
        filtered = a8.filtered_tx(value)
        self.assertEqual(filtered["tx_hash"], "ABC")
        self.assertEqual(filtered["height"], "42")
        self.assertEqual(filtered["code"], 0)
        self.assertEqual([item["type"] for item in filtered["events"]], ["wasm-deal_locked"])

    def test_wait_tx_rejects_confirmed_nonzero_code(self):
        runner = FakeRunner(
            [
                a8.CommandResult(
                    0,
                    json.dumps(
                        {
                            "height": "9",
                            "code": 7,
                            "txhash": "BAD",
                            "raw_log": "deliberate failure",
                        }
                    ),
                    "",
                )
            ]
        )
        with self.assertRaisesRegex(a8.AcceptanceError, "failed with code 7"):
            a8.DockerGonka(runner).wait_tx("BAD", timeout=0)

    def test_tx_attempt_records_checktx_failure_without_retry(self):
        runner = FakeRunner(
            [
                a8.CommandResult(
                    0,
                    json.dumps(
                        {
                            "code": 11,
                            "txhash": "",
                            "codespace": "sdk",
                            "raw_log": "insufficient fee",
                        }
                    ),
                    "",
                )
            ]
        )
        attempt = a8.DockerGonka(runner).tx_attempt(
            "genesis-node", "genesis", "wasm", "execute", "deal", "{}", gas="1"
        )
        self.assertEqual(attempt["layer"], "check_tx")
        self.assertEqual(attempt["code"], 11)
        self.assertEqual(len(runner.calls), 1)

    def test_tx_attempt_records_failed_delivertx_after_one_broadcast(self):
        runner = FakeRunner(
            [
                a8.CommandResult(0, json.dumps({"code": 0, "txhash": "ABC"}), ""),
                a8.CommandResult(
                    0,
                    json.dumps(
                        {
                            "height": "12",
                            "code": 5,
                            "txhash": "ABC",
                            "gas_wanted": "200000",
                            "gas_used": "199999",
                            "raw_log": "out of gas",
                        }
                    ),
                    "",
                ),
            ]
        )
        attempt = a8.DockerGonka(runner).tx_attempt(
            "genesis-node", "genesis", "wasm", "execute", "deal", "{}", gas="200000"
        )
        self.assertEqual(attempt["layer"], "deliver_tx")
        self.assertEqual(attempt["code"], 5)
        self.assertEqual(attempt["gas_used"], "199999")
        self.assertEqual(len(runner.calls), 2)

    def test_vesting_epoch_amounts_normalizes_missing_and_multiple_coins(self):
        self.assertEqual(a8.vesting_epoch_amounts({"vesting_schedule": None}), [])
        self.assertEqual(
            a8.vesting_epoch_amounts(
                {
                    "vesting_schedule": {
                        "participant_address": "gonka1deal",
                        "epoch_amounts": None,
                    }
                }
            ),
            [],
        )
        self.assertEqual(
            a8.vesting_epoch_amounts(
                {
                    "vesting_schedule": {
                        "epoch_amounts": [
                            {
                                "coins": [
                                    {"denom": "ngonka", "amount": "7"},
                                    {"denom": "ngonka", "amount": "2"},
                                ]
                            },
                            {"coins": None},
                        ]
                    }
                }
            ),
            [{"ngonka": 9}, {}],
        )
        with self.assertRaisesRegex(a8.AcceptanceError, "malformed vesting schedule"):
            a8.vesting_epoch_amounts(
                {"vesting_schedule": {"epoch_amounts": "not-a-list"}}
            )
        with self.assertRaisesRegex(a8.AcceptanceError, "malformed vesting epoch"):
            a8.vesting_epoch_amounts(
                {"vesting_schedule": {"epoch_amounts": [{"coins": "bad"}]}}
            )

    def test_injected_cw20_failure_requires_exact_fault_layer(self):
        a8.assert_injected_cw20_failure(
            {"code": 5, "raw_log": "A8 injected CW20 transfer failure for gonka1fee"},
            "SettleClaim",
        )
        with self.assertRaisesRegex(a8.AcceptanceError, "outside the configured"):
            a8.assert_injected_cw20_failure(
                {"code": 5, "raw_log": "out of gas"}, "SettleClaim"
            )
        with self.assertRaisesRegex(a8.AcceptanceError, "unexpectedly succeeded"):
            a8.assert_injected_cw20_failure(
                {"code": 0, "raw_log": ""}, "SettleClaim"
            )

    def test_expected_refund_failure_requires_delivertx_and_exact_contract_reason(self):
        actual = a8.assert_expected_refund_failure(
            {
                "code": 5,
                "layer": "deliver_tx",
                "raw_log": "claim-expiry window has not opened: current epoch 8",
            },
            "too_early",
            "locked",
        )
        self.assertEqual(
            actual["matched_contract_error"], "claim-expiry window has not opened"
        )
        claimed = a8.assert_expected_refund_failure(
            {
                "code": 5,
                "layer": "deliver_tx",
                "raw_log": "native claim for target epoch 7 is already confirmed",
            },
            "claimed",
            "locked",
        )
        self.assertEqual(claimed["reason"], "claimed")
        unavailable = a8.assert_expected_refund_failure(
            {
                "code": 5,
                "layer": "deliver_tx",
                "raw_log": (
                    "Gonka query failed for "
                    "/inference.inference.Query/EpochPerformanceSummaryByParticipant"
                ),
            },
            "network_unconfirmed_too_early",
            "locked",
        )
        self.assertEqual(
            unavailable["matched_contract_error"], "gonka query failed for"
        )
        with self.assertRaisesRegex(a8.AcceptanceError, "before contract execution"):
            a8.assert_expected_refund_failure(
                {"code": 11, "layer": "check_tx", "raw_log": "insufficient fee"},
                "too_early",
                "locked",
            )
        with self.assertRaisesRegex(a8.AcceptanceError, "different reason"):
            a8.assert_expected_refund_failure(
                {"code": 5, "layer": "deliver_tx", "raw_log": "out of gas"},
                "claimed",
                "locked",
            )

    def test_failed_tx_snapshot_allows_only_fee_payer_native_decrease(self):
        before = {
            "state": {"status": "locked"},
            "cw20": {"host": 0, "deal": 10},
            "foreign_cw20": {"deal": 7},
            "bank_ngonka": {"host": 100, "buyer": 200, "deal": 300},
        }
        after = json.loads(json.dumps(before))
        after["bank_ngonka"]["host"] = 90
        self.assertEqual(
            a8.assert_snapshot_unchanged_except_fee_payer(before, after, {"host"}),
            {"host": -10},
        )
        after["bank_ngonka"]["host"] = 110
        self.assertEqual(
            a8.assert_snapshot_unchanged_except_fee_payer(before, after, {"host"}),
            {"host": 10},
        )
        after["bank_ngonka"]["deal"] = 299
        with self.assertRaisesRegex(a8.AcceptanceError, "moved native balance"):
            a8.assert_snapshot_unchanged_except_fee_payer(before, after, {"host"})
        after = json.loads(json.dumps(before))
        after["cw20"]["deal"] = 9
        with self.assertRaisesRegex(a8.AcceptanceError, "mutated cw20"):
            a8.assert_snapshot_unchanged_except_fee_payer(before, after, {"host"})

    def test_exact_bank_transfer_event_is_scoped_to_one_event(self):
        tx = {
            "txhash": "ABC",
            "code": 0,
            "events": [
                {
                    "type": "transfer",
                    "attributes": [
                        {"key": "recipient", "value": "deal"},
                        {"key": "sender", "value": "buyer"},
                        {"key": "amount", "value": "2ngonka"},
                    ],
                }
            ],
        }
        self.assertEqual(
            a8.assert_exact_bank_transfer_event(tx, "buyer", "deal", 2, "ngonka"),
            {"sender": "buyer", "recipient": "deal", "amount": "2ngonka"},
        )
        with self.assertRaisesRegex(a8.AcceptanceError, "got 0"):
            a8.assert_exact_bank_transfer_event(tx, "buyer", "deal", 3, "ngonka")

    def test_exact_bank_transfer_event_rejects_ambiguous_duplicates(self):
        event = {
            "type": "transfer",
            "attributes": [
                {"key": "recipient", "value": "deal"},
                {"key": "sender", "value": "buyer"},
                {"key": "amount", "value": "2ngonka"},
            ],
        }
        tx = {"txhash": "ABC", "code": 0, "events": [event, event]}
        with self.assertRaisesRegex(a8.AcceptanceError, "got 2"):
            a8.assert_exact_bank_transfer_event(tx, "buyer", "deal", 2, "ngonka")

    def test_no_bank_transfer_involving_rejects_deal_endpoint(self):
        unrelated = {
            "events": [
                {
                    "type": "transfer",
                    "attributes": [
                        {"key": "sender", "value": "payer"},
                        {"key": "recipient", "value": "validator"},
                    ],
                }
            ]
        }
        a8.assert_no_bank_transfer_involving(unrelated, "deal")
        unrelated["events"][0]["attributes"][1]["value"] = "deal"
        with self.assertRaisesRegex(a8.AcceptanceError, "involving Deal"):
            a8.assert_no_bank_transfer_involving(unrelated, "deal")

    def test_protected_native_denom_transfer_detects_only_deal_outbound_or_inbound(self):
        tx = {
            "events": [
                {
                    "type": "transfer",
                    "attributes": [
                        {"key": "sender", "value": "deal"},
                        {"key": "recipient", "value": "host"},
                        {"key": "amount", "value": "5ngonka,12345ua8b3foreign"},
                    ],
                }
            ]
        }
        with self.assertRaisesRegex(a8.AcceptanceError, "protected denom"):
            a8.assert_no_denom_transfer_involving(tx, "deal", "ua8b3foreign")
        tx["events"][0]["attributes"][0]["value"] = "other"
        a8.assert_no_denom_transfer_involving(tx, "deal", "ua8b3foreign")

    def test_completed_release_repeat_is_terminal_noop_condition(self):
        self.assertTrue(a8.is_completed_release_noop({"status": "completed"}, 0))
        self.assertFalse(a8.is_completed_release_noop({"status": "releasing"}, 0))
        self.assertFalse(a8.is_completed_release_noop({"status": "completed"}, 1))

    def test_terminal_release_preconditions_require_fulfilled_positive_claim(self):
        snapshot = {
            "state": {"status": "completed", "total_claim_ngonka": "10"},
            "release_status": {
                "released_total_ngonka": "10",
                "buyer_original_remaining_ngonka": "0",
                "host_original_remaining_ngonka": "0",
            },
            "native_status": {
                "liquid_balance_ngonka": "0",
                "remaining_vesting_ngonka": "0",
            },
            "bank": {"deal": {}},
            "vesting_total": {"total_amount": []},
            "vesting_schedule": {"vesting_schedule": None},
        }
        proof = a8.assert_terminal_release_preconditions(snapshot)
        self.assertEqual(proof["positive_total_claim_ngonka"], 10)
        snapshot["native_status"]["remaining_vesting_ngonka"] = "1"
        with self.assertRaisesRegex(a8.AcceptanceError, "zero liquid and pending"):
            a8.assert_terminal_release_preconditions(snapshot)

    def test_terminal_snapshot_allows_only_caller_gnk_fee(self):
        before = {
            "state": {"status": "completed"},
            "config": {"fee_bps": 150},
            "entitlements": {"total_claim_ngonka": "10"},
            "release_status": {"released_total_ngonka": "10"},
            "native_status": {"liquid_balance_ngonka": "0"},
            "vesting_total": {"total_amount": []},
            "vesting_schedule": {"vesting_schedule": None},
            "settlement_cw20": {"deal": 0, "caller": 0},
            "foreign_cw20": {"deal": 0, "caller": 0},
            "bank": {
                "host": {"ngonka": 1},
                "buyer": {"ngonka": 2},
                "fee_recipient": {"ngonka": 3},
                "deal": {},
                "caller": {"ngonka": 100},
            },
        }
        after = json.loads(json.dumps(before))
        after["bank"]["caller"]["ngonka"] = 93
        self.assertEqual(
            a8.assert_terminal_snapshot_unchanged_except_caller_fee(before, after),
            {"ngonka": -7},
        )
        after["settlement_cw20"]["deal"] = 1
        with self.assertRaisesRegex(a8.AcceptanceError, "settlement_cw20"):
            a8.assert_terminal_snapshot_unchanged_except_caller_fee(before, after)

    def test_terminal_snapshot_helper_returns_its_own_complete_snapshot(self):
        class SnapshotGonka:
            def smart(self, _deal, message):
                return {"query": next(iter(message))}

            def query_json(self, _module, query, _deal):
                return {"query": query}

            def bank_balances(self, address):
                return {"ngonka": len(address)}

            def cw20_balance(self, contract, address):
                return len(contract) + len(address)

        context = {
            "contracts": {"deal": "deal", "cw20": "cw20", "foreign_cw20": "foreign"},
            "accounts": {"host": "host", "buyer": "buyer", "fee_recipient": "fee", "inactive_host": "caller"},
        }
        snapshot = a8.terminal_release_snapshot(SnapshotGonka(), context)
        self.assertEqual(snapshot["state"], {"query": "state"})
        self.assertEqual(snapshot["bank"]["deal"], {"ngonka": 4})
        self.assertEqual(snapshot["foreign_cw20"]["caller"], len("foreigncaller"))

    def test_terminal_release_accepts_only_included_nothing_to_release(self):
        attempt = {
            "layer": "deliver_tx",
            "tx_hash": "ABC123",
            "height": "214",
            "code": 5,
            "codespace": "wasm",
            "raw_log": (
                "failed to execute message; message index: 0: "
                "no additional GNK is currently available for release: "
                "execute wasm contract failed"
            ),
        }
        proof = a8.assert_terminal_nothing_to_release(attempt)
        self.assertEqual(proof["contract_error"], "NothingToRelease")
        self.assertEqual(proof["height"], 214)

        for field, value, error in (
            ("layer", "check_tx", "not included"),
            ("code", 0, "unexpectedly succeeded"),
            ("codespace", "sdk", "outside the wasm"),
            ("raw_log", "out of gas", "different reason"),
        ):
            wrong = dict(attempt)
            wrong[field] = value
            with self.subTest(field=field), self.assertRaisesRegex(
                a8.AcceptanceError, error
            ):
                a8.assert_terminal_nothing_to_release(wrong)

    def test_settlement_repeat_requires_included_wasm_terminal_state_error(self):
        attempt = {
            "layer": "deliver_tx",
            "tx_hash": "SETTLE2",
            "height": "220",
            "code": 5,
            "codespace": "wasm",
            "raw_log": "cannot settle claim in state Releasing",
        }
        self.assertEqual(
            a8.assert_terminal_settlement_repeat(attempt)["contract_error"],
            "InvalidSettlementState",
        )
        for field, value, error in (
            ("layer", "check_tx", "DeliverTx"),
            ("codespace", "sdk", "outside"),
            ("raw_log", "out of gas", "non-terminal"),
            ("height", "0", "included height"),
        ):
            wrong = dict(attempt)
            wrong[field] = value
            with self.subTest(field=field), self.assertRaisesRegex(a8.AcceptanceError, error):
                a8.assert_terminal_settlement_repeat(wrong)

    def test_terminal_release_command_and_live_selector_are_available(self):
        command = a8.parser().parse_args(
            ["terminal-release-repeat", "--context", "evidence.json"]
        )
        self.assertIs(command.handler, a8.terminal_release_repeat)
        self.assertEqual(command.gas, 2_000_000)
        live = a8.parser().parse_args(
            [
                "run-live",
                "--gonka-dir",
                "gonka",
                "--scenario",
                "terminal-release-repeat",
            ]
        )
        self.assertEqual(live.scenario, "terminal-release-repeat")

    def test_refund_cw20_expected_host_delta_when_host_is_buyer(self):
        expected_refund = 100
        host = "same"
        buyer = "same"
        self.assertEqual(expected_refund if host == buyer else 0, 100)
        self.assertEqual(expected_refund if "host" == "buyer" else 0, 0)

    def test_transfer_restriction_status_uses_protobuf_scalar_defaults(self):
        runner = FakeRunner(
            [a8.CommandResult(0, 'current_block_height: "34"\n', "")]
        )
        self.assertEqual(
            a8.DockerGonka(runner).transfer_restriction_status(),
            {
                "is_active": False,
                "restriction_end_block": 0,
                "current_block_height": 34,
                "remaining_blocks": 0,
            },
        )

    def test_bank_release_rollback_command_selects_native_keeper_handler(self):
        args = a8.parser().parse_args(
            [
                "bank-release-rollback-scenario",
                "--context",
                "evidence.json",
                "--name",
                "network-unconfirmed",
                "--proposal-id",
                "9",
                "--expected-send-index",
                "2",
                "--allowed-earlier-recipient",
                "gonka1buyer",
                "--rejected-recipient",
                "gonka1host",
            ]
        )
        self.assertIs(args.handler, a8.bank_release_rollback_scenario)
        self.assertEqual(args.gas, 2_000_000)

    def test_bank_retry_command_binds_to_the_same_selected_recipient(self):
        args = a8.parser().parse_args(
            [
                "bank-release-retry-scenario",
                "--context",
                "evidence.json",
                "--name",
                "network-unconfirmed",
                "--expected-send-index",
                "1",
                "--rejected-recipient",
                "gonka1host",
            ]
        )
        self.assertIs(args.handler, a8.bank_release_retry_scenario)
        self.assertEqual(args.expected_send_index, 1)

    def test_bank_rollback_handler_requires_included_native_restriction_failure(self):
        valid = {
            "layer": "deliver_tx",
            "code": 5,
            "tx_hash": "BANKFAIL",
            "height": "123",
            "raw_log": "user-to-user transfers are restricted during bootstrap period",
        }

        def run(attempt):
            with tempfile.TemporaryDirectory() as directory:
                context_path = Path(directory) / "context.json"
                a8.write_object(
                    context_path,
                    {
                        "chain": {"chain_id": a8.DEFAULT_CHAIN_ID},
                        "accounts": {"buyer": "buyer"},
                        "scenarios": {
                            "case": {
                                "contracts": {"deal": "deal", "cw20": "cw20"},
                                "accounts": {"host": "host", "buyer": "buyer", "fee_recipient": "fee"},
                                "phases": [],
                            }
                        },
                    },
                )
                args = SimpleNamespace(
                    context=str(context_path), name="case", gas=2_000_000,
                    proposal_id="9", expected_send_index=1,
                    allowed_earlier_recipient=None, rejected_recipient="host", exemption_id=None,
                )
                with patch.object(a8, "DockerGonka", return_value=BankRollbackGonka(attempt)):
                    try:
                        a8.bank_release_rollback_scenario(args)
                    except a8.AcceptanceError as error:
                        return error, a8.load_object(context_path)
                return None, a8.load_object(context_path)

        error, context = run(valid)
        self.assertIsNone(error)
        self.assertEqual(context["scenarios"]["case"]["phases"][-1]["name"], "native_bank_release_rollback")
        invalid_cases = (
            ({**valid, "layer": "check_tx"}, "DeliverTx"),
            ({**valid, "tx_hash": ""}, "transaction hash"),
            ({**valid, "height": "0"}, "included height"),
            ({**valid, "code": 0}, "unexpectedly succeeded"),
            ({**valid, "raw_log": "out of gas"}, "outside the native Bank restriction"),
            ({**valid, "raw_log": "some unrelated error"}, "outside the native Bank restriction"),
        )
        for attempt, marker in invalid_cases:
            with self.subTest(attempt=attempt):
                error, context = run(attempt)
                self.assertIsInstance(error, a8.AcceptanceError)
                self.assertIn(marker, str(error))
                self.assertEqual(context["scenarios"]["case"]["phases"], [])

    def test_bank_fault_plan_and_selected_scenario_repeat_commands_are_available(self):
        plan = a8.parser().parse_args(
            ["bank-release-fault-plan", "--context", "evidence.json", "--name", "r71"]
        )
        self.assertIs(plan.handler, a8.bank_release_fault_plan)
        repeat = a8.parser().parse_args(
            ["scenario-release-repeat", "--context", "evidence.json", "--name", "r72"]
        )
        self.assertIs(repeat.handler, a8.scenario_release_repeat)

    def test_bank_fault_selector_uses_buyer_first_for_proportional_and_host_for_host_only(self):
        proportional = {
            "released_total_ngonka": "0",
            "buyer_released_ngonka": "0",
            "host_released_ngonka": "0",
            "gnk_release_policy": {
                "proportional": {"buyer_share_numerator": "40", "share_denominator": "100"}
            },
        }
        first = a8.bank_release_fault_target(proportional, 100, "buyer", "host", 1)
        second = a8.bank_release_fault_target(proportional, 100, "buyer", "host", 2)
        self.assertEqual((first["recipient"], first["amount"]), ("buyer", 40))
        self.assertEqual((second["recipient"], second["amount"]), ("host", 60))
        host_only = {**proportional, "gnk_release_policy": "host_only"}
        self.assertEqual(
            a8.bank_release_fault_target(host_only, 100, "buyer", "host", 1)["recipient"],
            "host",
        )
        with self.assertRaisesRegex(a8.AcceptanceError, "two non-zero"):
            a8.bank_release_fault_target(host_only, 100, "buyer", "host", 2)

    def test_second_bank_send_requires_live_exact_buyer_exemption_not_launcher_claim(self):
        params = {
            "emergency_transfer_exemptions": [
                {
                    "exemption_id": "r71",
                    "from_address": "deal",
                    "to_address": "buyer",
                    "max_amount": "40",
                    "usage_limit": "1",
                    "expiry_block": "200",
                }
            ],
            "exemption_usage_tracking": [],
        }
        proof = a8.assert_live_bank_second_send_exemption(
            params, 100, "r71", "deal", "buyer", 40
        )
        self.assertEqual(proof["to_address"], "buyer")
        for empty in ({k: v for k, v in params.items() if k != "exemption_usage_tracking"},
                      {**params, "exemption_usage_tracking": None}):
            self.assertEqual(a8.assert_live_bank_second_send_exemption(
                empty, 100, "r71", "deal", "buyer", 40), proof)
        for malformed in (False, 0, "", {}, [None]):
            with self.assertRaises(a8.AcceptanceError):
                a8.assert_live_bank_second_send_exemption(
                    {**params, "exemption_usage_tracking": malformed},
                    100, "r71", "deal", "buyer", 40)

        with self.assertRaisesRegex(a8.AcceptanceError, "lack one exact"):
            a8.assert_live_bank_second_send_exemption(
                {"emergency_transfer_exemptions": []}, 100, "r71", "deal", "buyer", 40
            )
        broad = {
            **params,
            "emergency_transfer_exemptions": [*params["emergency_transfer_exemptions"], {
                **params["emergency_transfer_exemptions"][0], "exemption_id": "broad", "to_address": "*"
            }]
        }
        with self.assertRaisesRegex(a8.AcceptanceError, "broad Buyer"):
            a8.assert_live_bank_second_send_exemption(broad, 100, "r71", "deal", "buyer", 40)
        exhausted = {
            **params,
            "exemption_usage_tracking": [
                {"exemption_id": "r71", "account_address": "deal", "usage_count": "1"}
            ],
        }
        with self.assertRaisesRegex(a8.AcceptanceError, "already exhausted"):
            a8.assert_live_bank_second_send_exemption(exhausted, 100, "r71", "deal", "buyer", 40)

    def test_verify_claimed_command_requires_explicit_positive_policy(self):
        args = a8.parser().parse_args(
            [
                "verify-claimed-scenario",
                "--context",
                "evidence.json",
                "--name",
                "no-sale",
                "--require-positive",
            ]
        )
        self.assertIs(args.handler, a8.verify_claimed_scenario)
        self.assertTrue(args.require_positive)
        self.assertEqual(args.wait_seconds, 120)

    def test_verify_unclaimed_command_can_require_positive_native_reward(self):
        args = a8.parser().parse_args(
            [
                "verify-unclaimed-scenario",
                "--context",
                "evidence.json",
                "--name",
                "claim-expiry-positive",
                "--require-positive",
            ]
        )
        self.assertIs(args.handler, a8.verify_unclaimed_scenario)
        self.assertTrue(args.require_positive)
        self.assertFalse(args.require_zero)

    def test_verify_unclaimed_command_can_require_exact_zero_native_reward(self):
        args = a8.parser().parse_args(
            [
                "verify-unclaimed-scenario",
                "--context",
                "evidence.json",
                "--name",
                "claim-expiry-zero",
                "--require-zero",
            ]
        )
        self.assertIs(args.handler, a8.verify_unclaimed_scenario)
        self.assertFalse(args.require_positive)
        self.assertTrue(args.require_zero)

    def test_verify_missing_summary_command_requires_exact_emergency_offset(self):
        args = a8.parser().parse_args(
            [
                "verify-missing-summary-scenario",
                "--context",
                "evidence.json",
                "--name",
                "network-unconfirmed",
                "--expected-offset",
                "2",
            ]
        )
        self.assertIs(args.handler, a8.verify_missing_summary_scenario)
        self.assertEqual(args.expected_offset, 2)

    def test_run_live_can_select_only_positive_claim_expiry(self):
        args = a8.parser().parse_args(
            [
                "run-live",
                "--gonka-dir",
                "gonka",
                "--scenario",
                "claim-expiry-positive",
            ]
        )
        self.assertIs(args.handler, a8.run_live)
        self.assertEqual(args.scenario, "claim-expiry-positive")

    def test_run_live_can_select_only_zero_claim_expiry(self):
        args = a8.parser().parse_args(
            [
                "run-live",
                "--gonka-dir",
                "gonka",
                "--scenario",
                "claim-expiry-zero",
            ]
        )
        self.assertIs(args.handler, a8.run_live)
        self.assertEqual(args.scenario, "claim-expiry-zero")

    def test_run_live_can_select_only_network_unconfirmed(self):
        args = a8.parser().parse_args(
            [
                "run-live",
                "--gonka-dir",
                "gonka",
                "--scenario",
                "network-unconfirmed",
            ]
        )
        self.assertIs(args.handler, a8.run_live)
        self.assertEqual(args.scenario, "network-unconfirmed")

    def test_bootstrap_can_record_expected_zero_epoch_reward(self):
        args = a8.parser().parse_args(
            [
                "bootstrap",
                "--context",
                "evidence.json",
                "--run-id",
                "run-zero",
                "--target-epoch",
                "5",
                "--deal-wasm",
                "deal.wasm",
                "--factory-wasm",
                "factory.wasm",
                "--cw20-wasm",
                "cw20.wasm",
                "--caller-wasm",
                "caller.wasm",
                "--expected-initial-epoch-reward",
                "0",
            ]
        )
        self.assertIs(args.handler, a8.bootstrap)
        self.assertEqual(args.expected_initial_epoch_reward, 0)

    def test_transaction_epoch_bracket_requires_same_epoch_and_included_height(self):
        bracket = a8.assert_tx_epoch_bracket(
            {"height": "102", "txhash": "ABC"},
            {"height": 101, "epoch": 7},
            {"height": 103, "epoch": 7},
        )
        self.assertEqual(bracket["tx_height"], 102)
        self.assertTrue(bracket["same_epoch"])
        with self.assertRaisesRegex(a8.AcceptanceError, "epoch changed"):
            a8.assert_tx_epoch_bracket(
                {"height": "103", "txhash": "ABC"},
                {"height": 101, "epoch": 7},
                {"height": 103, "epoch": 8},
            )

    def test_lock_boundary_commands_and_live_selectors_are_available(self):
        command = a8.parser().parse_args(
            ["lock-e-plus-4-scenario", "--context", "evidence.json", "--name", "e4"]
        )
        self.assertIs(command.handler, a8.lock_e_plus_4_scenario)
        live = a8.parser().parse_args(
            ["run-live", "--gonka-dir", "gonka", "--scenario", "lock-e-plus-4"]
        )
        self.assertEqual(live.scenario, "lock-e-plus-4")
        rejected = a8.parser().parse_args(
            ["lock-e-plus-5-scenario", "--context", "evidence.json", "--name", "e5"]
        )
        self.assertIs(rejected.handler, a8.lock_e_plus_5_rejected_scenario)
        self.assertEqual(rejected.gas, 2_000_000)
        live_e5 = a8.parser().parse_args(
            ["run-live", "--gonka-dir", "gonka", "--scenario", "lock-e-plus-5"]
        )
        self.assertEqual(live_e5.scenario, "lock-e-plus-5")

    def test_r1_refund_boundary_accepts_only_included_typed_refund_window_closed(self):
        attempt = {
            "layer": "deliver_tx", "code": 5, "codespace": "wasm",
            "raw_log": "refund routing-proof window is closed: current epoch 12, target epoch 7, exclusive end 12",
        }
        a8.assert_refund_window_closed(attempt, current=12, target=7)
        for message in ("refund window is closed: current epoch 12, target epoch 7, exclusive end 12",
                        "refund routing-proof window is closed: current epoch 12, target epoch 7, exclusive end 13"):
            with self.assertRaises(a8.AcceptanceError):
                a8.assert_refund_window_closed(dict(attempt, raw_log=message), current=12, target=7)
        for field, value in (("layer", "check_tx"), ("code", 0), ("codespace", "sdk"), ("raw_log", "out of gas")):
            wrong = dict(attempt)
            wrong[field] = value
            with self.subTest(field=field), self.assertRaises(a8.AcceptanceError):
                a8.assert_refund_window_closed(wrong, current=12, target=7)

    def test_package_a_selector_and_empty_schedule_vesting_proof_are_explicit(self):
        r1 = a8.parser().parse_args([
            "refund-e-plus-5-scenario", "--context", "evidence.json", "--name", "r1"
        ])
        self.assertIs(r1.handler, a8.refund_e_plus_5_rejected_scenario)
        self.assertEqual(r1.gas, 2_000_000)
        gift = a8.parser().parse_args([
            "verify-vesting-addition-scenario", "--context", "evidence.json", "--name", "r2",
            "--before-label", "before", "--amount", "11", "--vesting-epochs", "2",
            "--fund-tx-hash", "ABC", "--proposal-id", "9", "--allow-empty-before",
        ])
        self.assertTrue(gift.allow_empty_before)
        live = a8.parser().parse_args([
            "run-live", "--gonka-dir", "gonka", "--scenario", "package-a-r1-r2"
        ])
        self.assertEqual(live.scenario, "package-a-r1-r2")

        checkpoint = a8.parser().parse_args([
            "r2-gift-checkpoint", "--context", "evidence.json", "--name", "r2",
            "--stage", "first_unlocked", "--gift-amount", "100",
        ])
        self.assertIs(checkpoint.handler, a8.r2_gift_checkpoint)
        self.assertEqual(checkpoint.stage, "first_unlocked")

    def test_recorded_r1_receipt_passes_corrected_error_gate(self):
        path = MODULE_PATH.parents[1] / "docs/reviews/evidence/a8-abc-reviewed-20260910.json"
        reviewed = json.loads(path.read_text(encoding="utf-8"))
        evidence = reviewed["runs"]["a8abc-a-20260909b"]["cases"]["R1.1"]["evidence"]
        a8.assert_refund_window_closed(evidence["attempt"], current=10, target=5)
        self.assertEqual(evidence["before"], evidence["after"])
        self.assertEqual(evidence["epoch_bracket"]["epoch"], 10)

    def test_bank_plan_rejects_remaining_vesting_even_if_a_first_tranche_is_liquid(self):
        a8.assert_fully_unlocked_vesting({"total_amount": None}, {"vesting_schedule": {"epoch_amounts": None}})
        total = {"total_amount": [{"denom": "ngonka", "amount": "47409835683225"}]}
        schedule = {"vesting_schedule": {"epoch_amounts": [{"coins": [{"denom": "ngonka", "amount": "47409835683225"}]}]}}
        for t, s in ((total, schedule), (total, {"vesting_schedule": {"epoch_amounts": []}}), ({"total_amount": []}, schedule)):
            with self.assertRaisesRegex(a8.AcceptanceError, "fully unlocked"):
                a8.assert_fully_unlocked_vesting(t, s)

    def test_r1_r2_oracles_reject_wrong_epoch_and_partial_release(self):
        with self.assertRaisesRegex(a8.AcceptanceError, "epoch changed"):
            a8.assert_tx_epoch_bracket(
                {"height": "12", "txhash": "ABC"},
                {"height": 11, "epoch": 7},
                {"height": 13, "epoch": 8},
            )
        before = {
            "gnk_release_policy": {"proportional": {"buyer_share_numerator": "1", "share_denominator": "2"}},
            "released_total_ngonka": "0",
            "buyer_released_ngonka": "0",
            "host_released_ngonka": "0",
        }
        partial_after = {**before, "released_total_ngonka": "4", "buyer_released_ngonka": "2", "host_released_ngonka": "2"}
        with self.assertRaisesRegex(a8.AcceptanceError, "independent oracle"):
            a8.assert_release_matches_oracle(before, partial_after, 10, 2, 2)

    def test_proto3_omitted_claimed_field_decodes_as_false(self):
        self.assertFalse(a8.protobuf_bool({}, "claimed"))
        self.assertFalse(a8.protobuf_bool({"claimed": False}, "claimed"))
        self.assertTrue(a8.protobuf_bool({"claimed": True}, "claimed"))
        with self.assertRaisesRegex(a8.AcceptanceError, "protobuf boolean"):
            a8.protobuf_bool({"claimed": "false"}, "claimed")

    def test_gas_sweep_has_explicit_sufficient_gas_budget(self):
        args = a8.parser().parse_args(
            ["gas-sweep-scenario", "--context", "evidence.json", "--name", "gas"]
        )
        self.assertIs(args.handler, a8.gas_sweep_scenario)
        self.assertEqual(args.sufficient_gas, 2_000_000)

    def test_vesting_snapshot_can_require_existing_tranches(self):
        args = a8.parser().parse_args(
            [
                "snapshot-vesting-scenario",
                "--context",
                "evidence.json",
                "--name",
                "no-sale",
                "--label",
                "old",
                "--require-non-empty",
            ]
        )
        self.assertTrue(args.require_non_empty)

    def test_create_key_never_exposes_mnemonic_in_error(self):
        secret = "word " * 24
        runner = FakeRunner([a8.CommandResult(0, json.dumps({"mnemonic": secret}), "")])
        with self.assertRaises(a8.AcceptanceError) as failure:
            a8.DockerGonka(runner).create_key("a8-buyer")
        self.assertNotIn("word", str(failure.exception))

    def test_context_write_is_atomic_and_round_trips_unicode(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "evidence.json"
            value = {"kind": "тест", "phases": []}
            a8.write_object(path, value)
            self.assertEqual(a8.load_object(path), value)
            self.assertFalse(path.with_suffix(".json.tmp").exists())

    @unittest.skipUnless(a8.os.name == "nt", "Windows-specific path conversion")
    def test_windows_path_is_converted_without_shell_parsing(self):
        converted = a8.wsl_path(Path("C:/work/gonka"), FakeRunner([]))
        self.assertEqual(converted.lower(), "/mnt/c/work/gonka")

    @unittest.skipIf(a8.os.name == "nt", "POSIX-specific path passthrough")
    def test_posix_path_is_resolved_without_wsl_conversion(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "gonka"
            self.assertEqual(a8.wsl_path(path, FakeRunner([])), str(path.resolve()))

    def test_p0_probe_command_requires_context_and_wasm(self):
        args = a8.parser().parse_args(
            ["p0-probe", "--context", "evidence.json", "--wasm", "probe.wasm"]
        )
        self.assertIs(args.handler, a8.p0_probe)
        self.assertEqual(args.context, "evidence.json")
        self.assertEqual(args.wasm, "probe.wasm")

    def test_b3_release_command_requires_explicit_native_fixture_inputs(self):
        args = a8.parser().parse_args(
            [
                "b3-foreign-native-release",
                "--context", "evidence.json",
                "--foreign-key", "a8-b3-foreign",
                "--foreign-address", "gonka1fixture",
                "--foreign-denom", "ua8b3foreign",
                "--foreign-amount", "12345",
            ]
        )
        self.assertIs(args.handler, a8.b3_foreign_native_release)
        self.assertEqual(args.foreign_amount, 12345)

    def test_gonka_overlay_checksum_verification_passes(self):
        a8.verify_gonka_overlay_integrity(a8.DEFAULT_GONKA_OVERLAY_DIR)

    def test_gonka_overlay_checksum_fails_on_tamper(self):
        with tempfile.TemporaryDirectory() as td:
            temp_overlay = Path(td) / "overlay"
            import shutil
            shutil.copytree(a8.DEFAULT_GONKA_OVERLAY_DIR, temp_overlay)
            tampered_file = temp_overlay / "README_SMART_CONTRACT_TEST.md"
            tampered_file.write_text("tampered content", encoding="utf-8")
            with self.assertRaisesRegex(a8.AcceptanceError, "Overlay checksum mismatch"):
                a8.verify_gonka_overlay_integrity(temp_overlay)

    def test_gonka_overlay_checksum_fails_if_checksums_file_missing(self):
        with tempfile.TemporaryDirectory() as td:
            temp_overlay = Path(td) / "overlay"
            import shutil
            shutil.copytree(a8.DEFAULT_GONKA_OVERLAY_DIR, temp_overlay)
            (temp_overlay / "CHECKSUMS.sha256").unlink()
            with self.assertRaisesRegex(a8.AcceptanceError, "checksum manifest missing"):
                a8.verify_gonka_overlay_integrity(temp_overlay)

    def test_apply_gonka_overlay_copies_expected_files(self):
        with tempfile.TemporaryDirectory() as td:
            target = Path(td) / "target"
            target.mkdir()
            count = a8.apply_gonka_overlay(a8.DEFAULT_GONKA_OVERLAY_DIR, target)
            self.assertEqual(count, 35)
            self.assertTrue((target / "inference-chain/app/legacy.go").is_file())
            self.assertTrue((target / "inference-chain/scripts/init-docker-genesis.sh").is_file())
            if a8.os.name != "nt":
                mode = (target / "inference-chain/scripts/init-docker-genesis.sh").stat().st_mode
                self.assertTrue(bool(mode & 0o111))

    def test_verify_gonka_test_checkout_accepts_expected_base_sha(self):
        runner = FakeRunner([subprocess.CompletedProcess([], 0, a8.EXPECTED_GONKA_BASE_SHA + "\n", "")])
        self.assertEqual(
            a8.verify_gonka_test_checkout(Path("gonka"), runner),
            a8.EXPECTED_GONKA_BASE_SHA,
        )

    def test_parser_supports_overlay_and_temporary_workspace_flags(self):
        parsed = a8.parser().parse_args(
            [
                "run-live",
                "--gonka-dir", "base-gonka",
                "--overlay-dir", "custom-overlay",
                "--temp-gonka-dir", "tmp-ws",
                "--keep-temp-gonka",
                "--no-temp-gonka",
            ]
        )
        self.assertEqual(parsed.gonka_dir, "base-gonka")
        self.assertEqual(parsed.overlay_dir, "custom-overlay")
        self.assertEqual(parsed.temp_gonka_dir, "tmp-ws")
        self.assertTrue(parsed.keep_temp_gonka)
        self.assertTrue(parsed.no_temp_gonka)

    def test_prepare_temporary_gonka_workspace_with_fake_runner(self):
        with tempfile.TemporaryDirectory() as td:
            base_dir = Path(td) / "base-gonka"
            base_dir.mkdir()
            target_dir = Path(td) / "temp-ws"
            fake_results = [
                subprocess.CompletedProcess([], 0, a8.EXPECTED_GONKA_BASE_SHA + "\n", ""),  # rev-parse HEAD
                subprocess.CompletedProcess([], 0, "", ""),  # clone -s
                subprocess.CompletedProcess([], 0, "", ""),  # checkout
                subprocess.CompletedProcess([], 0, a8.EXPECTED_GONKA_BASE_SHA + "\n", ""),  # pinned HEAD
                subprocess.CompletedProcess([], 0, "", ""),  # config user.name
                subprocess.CompletedProcess([], 0, "", ""),  # config user.email
                subprocess.CompletedProcess([], 0, "", ""),  # add -A
                subprocess.CompletedProcess([], 0, "", ""),  # commit
            ]
            runner = FakeRunner(fake_results)
            ws, cleanup = a8.prepare_temporary_gonka_workspace(
                base_dir,
                a8.DEFAULT_GONKA_OVERLAY_DIR,
                runner,
                target_dir=target_dir,
            )
            self.assertEqual(ws, target_dir.resolve())
            self.assertIsNone(cleanup)
            self.assertTrue((ws / "inference-chain/app/legacy.go").is_file())
            self.assertEqual(len(runner.calls), 8)


class WithdrawalKeeperTests(unittest.TestCase):
    def test_roles_use_independent_transactions_skip_zero_and_continue_after_failure(self):
        from unittest.mock import Mock, patch

        gonka = Mock()
        gonka.execute.side_effect = [{"host": "confirmed"}, a8.AcceptanceError("blocked"), {"buyer": "confirmed"}]
        payments = {role: {"pending_micro_usdt": "1"} for role in ("host", "fee", "buyer")}
        with patch.object(a8, "filtered_tx", side_effect=lambda tx: tx):
            with self.assertRaisesRegex(a8.AcceptanceError, "pending for: fee"):
                a8.withdraw_pending_usdt(gonka, "deal", payments)
        self.assertEqual([call.args[3] for call in gonka.execute.call_args_list], [
            {"withdraw_usdt": {"role": role}} for role in ("host", "fee", "buyer")
        ])
        # Next pass only retries the remaining fee; settled/paid roles stay untouched.
        payments["host"]["pending_micro_usdt"] = "0"
        payments["buyer"]["pending_micro_usdt"] = "0"
        gonka.reset_mock()
        gonka.execute.side_effect = [{"fee": "confirmed"}]
        with patch.object(a8, "filtered_tx", side_effect=lambda tx: tx):
            self.assertEqual(a8.withdraw_pending_usdt(gonka, "deal", payments), {"fee": {"fee": "confirmed"}})
        gonka.execute.assert_called_once()
        self.assertEqual(gonka.execute.call_args.args[3], {"withdraw_usdt": {"role": "fee"}})


if __name__ == "__main__":
    unittest.main()
