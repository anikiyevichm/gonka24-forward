import com.google.gson.JsonParser
import com.productscience.EpochStage
import com.productscience.GENESIS_KEY_NAME
import com.productscience.data.Coin
import com.productscience.data.AppState
import com.productscience.data.BitcoinRewardParams
import com.productscience.data.EpochParams
import com.productscience.data.GovParams
import com.productscience.data.GovState
import com.productscience.data.InferenceParams
import com.productscience.data.InferenceState
import com.productscience.data.MsgTransferWithVesting
import com.productscience.data.RestrictionsParams
import com.productscience.data.RestrictionsState
import com.productscience.data.TokenomicsParams
import com.productscience.data.UpdateRestrictionsParams
import com.productscience.data.UnfundedInferenceParticipant
import com.productscience.data.spec
import com.productscience.inferenceConfig
import com.productscience.initCluster
import com.productscience.logSection
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.Timeout
import java.io.File
import java.time.Duration
import java.util.concurrent.TimeUnit

@Timeout(value = 35, unit = TimeUnit.MINUTES)
class MarketplaceContractAcceptanceTests : TestermintTest() {
    @Test
    @Timeout(value = 65, unit = TimeUnit.MINUTES)
    fun `marketplace package C proves query faults and recovery`() {
        // The immutable C binary is built by run-live before this cluster starts.
        // Longer explicit test epochs leave room for the finite E+2/E+3 batches.
        requiredEnv("A8_C_BINARY")
        val config = fastMarketplaceConfig(enableCQueryFaults = true, epochLength = 75L)
        val (cluster, genesis) = initCluster(config = config, reboot = true)
        cluster.allPairs.forEach { it.waitForMlNodesToLoad() }
        fun phase(name: String, vararg extra: String) = runHarness(
            "c-phase", "--context", requiredEnv("A8_CONTEXT"), "--phase", name, *extra,
        )
        val kinds = listOf("handler_error", "malformed_protobuf", "oversized_response",
            "missing_nested_summary", "wrong_host", "wrong_epoch",
            "invalid_participant_address", "unsupported_request")
        val r4 = kinds.map { "r4-$it" }
        val routing = listOf("handler_error", "malformed_protobuf", "duplicate_routing")
            .flatMap { kind -> listOf("r3-$kind-lock", "r3-$kind-refund") }
        val inactive = r4 + routing + listOf("r3-epoch-lock", "r3-epoch-refund")
        val hosts = inactive.associateWith { createInactiveParticipant(genesis, it) }
        val buyer = genesis.node.createKey("a8-c-buyer")
        val funding = genesis.submitTransaction(listOf("bank", "send",
            genesis.node.getColdAddress(), buyer.address, "1000000ngonka"))
        check(funding.code == 0) { "C Buyer fee funding failed: ${funding.rawLog}" }
        // Compute E AFTER registrations, so fixture creation cannot consume the
        // original target window. Every offer has its own unique Host/E pair.
        val targetEpoch = genesis.getEpochData().latestEpoch.index + 2
        runHarness(
            "bootstrap", "--context", requiredEnv("A8_CONTEXT"), "--run-id", requiredEnv("A8_RUN_ID"),
            "--target-epoch", targetEpoch.toString(), "--deal-wasm", requiredEnv("A8_DEAL_WASM"),
            "--factory-wasm", requiredEnv("A8_FACTORY_WASM"), "--cw20-wasm", requiredEnv("A8_CW20_WASM"),
            "--caller-wasm", requiredEnv("A8_CALLER_WASM"), "--buyer-node", "genesis-node",
            "--buyer-key", buyer.name, "--buyer-tokens", "1000000000",
        )
        inactive.forEach { prepareDeal(it, targetEpoch, true, "genesis-node", hosts.getValue(it)) }
        prepareDeal("r5-cancel", targetEpoch, true, "join2-node", "join2")
        phase("prepare") // Explicitly aliases the primary join1/E Deal as r5-recover.
        genesis.markNeedsReboot()
        while (genesis.getEpochData().latestEpoch.index < targetEpoch) genesis.waitForNextEpoch()
        (r4 + listOf("r3-epoch-refund", "r5-recover", "r5-cancel")).forEach {
            runHarness("lock-scenario", "--context", requiredEnv("A8_CONTEXT"), "--name", it)
        }
        val participants = cluster.joinPairs
        check(participants.size == 2)
        val seeds = participants.map { it.api.getConfig().currentSeed }
        seeds.forEach { check(it.epochIndex == targetEpoch) { "C requires exact real claim seed E" } }
        participants.forEach { it.stopApiContainer() }
        genesis.waitForStage(EpochStage.CLAIM_REWARDS, offset = 2)
        listOf("r5-recover", "r5-cancel").forEachIndexed { index, name ->
            runHarness("claim-scenario", "--context", requiredEnv("A8_CONTEXT"), "--name", name,
                "--reward-seed", seeds[index].seed.toString(), "--reward-epoch", targetEpoch.toString())
        }
        phase("activate")
        genesis.node.waitForNextBlock(2)
        phase("routing")
        while (genesis.getEpochData().latestEpoch.index < targetEpoch + 2) genesis.waitForNextEpoch()
        // R5 must recover BEFORE the E+3 deadline. Do it before spending the
        // rest of E+2 on the independent R4 matrix (whose faults remain active).
        phase("early-r5")
        phase("recover-before")
        genesis.node.waitForNextBlock(2)
        phase("early-r4")
        while (genesis.getEpochData().latestEpoch.index < targetEpoch + 3) genesis.waitForNextEpoch()
        phase("late")
        phase("recover-after")
        genesis.node.waitForNextBlock(2)
        phase("terminal")
        // R7.2 is a real native keeper restriction, independent of query faults.
        // Its helper requires original vesting fully unlocked before fault/retry.
        val restrictionEnd = genesis.node.queryRestrictionsStatus().currentBlockHeight + 50
        val proposal = genesis.runProposal(cluster, UpdateRestrictionsParams(params = RestrictionsParams(
            restrictionEndBlock = restrictionEnd,
            emergencyTransferExemptions = emptyList(), exemptionUsageTracking = emptyList(),
        )))
        phase("bank-fault", "--proposal-id", proposal)
        genesis.node.waitForMinimumBlock(restrictionEnd + 1, "C R7.2 restriction expiry")
        phase("bank-retry")
        phase("report")
    }

    @Test
    fun `marketplace package A preserves R1 refund boundary and releases a new vested gift`() {
        // One cluster is intentional: R1 and R2 get independent Host/E and Deal
        // fixtures, but share a monotonic epoch schedule and one bootstrap.
        val config = fastMarketplaceConfig()
        val (cluster, genesis) = initCluster(config = config, reboot = true)
        cluster.allPairs.forEach { it.waitForMlNodesToLoad() }
        val targetEpoch = genesis.getEpochData().latestEpoch.index + 3
        // bootstrap owns join1/targetEpoch; R2 deliberately uses the next
        // epoch, so its Factory (Host,E) key cannot collide with bootstrap.
        val r2Epoch = targetEpoch + 1
        val r1HostKey = createInactiveParticipant(genesis, "a8-package-a-r1")

        runHarness(
            "bootstrap", "--context", requiredEnv("A8_CONTEXT"), "--run-id", requiredEnv("A8_RUN_ID"),
            "--target-epoch", targetEpoch.toString(), "--deal-wasm", requiredEnv("A8_DEAL_WASM"),
            "--factory-wasm", requiredEnv("A8_FACTORY_WASM"), "--cw20-wasm", requiredEnv("A8_CW20_WASM"),
            "--caller-wasm", requiredEnv("A8_CALLER_WASM"),
        )
        prepareDeal("r1-refund-e-plus-5", targetEpoch, funded = true, hostNode = "genesis-node", hostKey = r1HostKey)
        prepareDeal("r2-vested-gift", r2Epoch, funded = true, hostNode = "join1-node", hostKey = "join1")
        genesis.markNeedsReboot()

        while (genesis.getEpochData().latestEpoch.index < r2Epoch) genesis.waitForNextEpoch()
        runHarness("lock-scenario", "--context", requiredEnv("A8_CONTEXT"), "--name", "r2-vested-gift")
        val rewardSeed = cluster.joinPairs.first().api.getConfig().currentSeed
        check(rewardSeed.epochIndex == r2Epoch) { "R2 reward seed epoch must equal its Deal epoch" }
        cluster.joinPairs.first().stopApiContainer()
        genesis.waitForStage(EpochStage.CLAIM_REWARDS, offset = 2)
        runHarness("claim-scenario", "--context", requiredEnv("A8_CONTEXT"), "--name", "r2-vested-gift", "--reward-seed", rewardSeed.seed.toString(), "--reward-epoch", rewardSeed.epochIndex.toString())
        runHarness("settle-scenario", "--context", requiredEnv("A8_CONTEXT"), "--name", "r2-vested-gift")
        cluster.joinPairs.first().restartApiContainer()

        // Two original native tranches complete R2 before any new gift is made.
        genesis.waitForStage(EpochStage.CLAIM_REWARDS, offset = -1)
        genesis.waitForStage(EpochStage.CLAIM_REWARDS, offset = 2)
        genesis.node.waitForNextBlock(2)
        runHarness("release-scenario", "--context", requiredEnv("A8_CONTEXT"), "--name", "r2-vested-gift")
        genesis.waitForStage(EpochStage.CLAIM_REWARDS, offset = 2)
        genesis.node.waitForNextBlock(2)
        runHarness("release-scenario", "--context", requiredEnv("A8_CONTEXT"), "--name", "r2-vested-gift")
        runHarness("r2-gift-checkpoint", "--context", requiredEnv("A8_CONTEXT"), "--name", "r2-vested-gift", "--stage", "pre_gift")

        while (genesis.getEpochData().latestEpoch.index < targetEpoch + 5) genesis.waitForNextEpoch()
        // Isolated case evidence is written before a failed assertion is surfaced.
        val r1 = runCatching {
            runHarness("refund-e-plus-5-scenario", "--context", requiredEnv("A8_CONTEXT"), "--name", "r1-refund-e-plus-5", "--gas", "2000000")
        }

        val gift = 10_000_000_001L
        runHarness("snapshot-vesting-scenario", "--context", requiredEnv("A8_CONTEXT"), "--name", "r2-vested-gift", "--label", "before-gift")
        val governanceAddress = genesis.node.getModuleAccount("gov").account.value.address
        val genesisAddress = genesis.node.getColdAddress()
        genesis.ensureGenesisSpendableForDevshard(gift)
        val fundingTx = genesis.submitTransaction(listOf("bank", "send", genesisAddress, governanceAddress, "$gift${genesis.config.denom}"))
        check(fundingTx.code == 0) { "R2 governance funding failed: ${fundingTx.rawLog}" }
        val proposalId = genesis.runProposal(cluster, MsgTransferWithVesting(
            sender = governanceAddress, recipient = scenarioDeal("r2-vested-gift"),
            amount = listOf(Coin(genesis.config.denom, gift)), vestingEpochs = 2,
        ))
        runHarness("verify-vesting-addition-scenario", "--context", requiredEnv("A8_CONTEXT"), "--name", "r2-vested-gift", "--before-label", "before-gift", "--amount", gift.toString(), "--vesting-epochs", "2", "--fund-tx-hash", fundingTx.txhash, "--proposal-id", proposalId, "--allow-empty-before")
        runHarness("r2-gift-checkpoint", "--context", requiredEnv("A8_CONTEXT"), "--name", "r2-vested-gift", "--stage", "fully_locked", "--gift-amount", gift.toString())
        genesis.waitForNextEpoch()
        runHarness("r2-gift-checkpoint", "--context", requiredEnv("A8_CONTEXT"), "--name", "r2-vested-gift", "--stage", "first_unlocked", "--gift-amount", gift.toString())
        runHarness("release-scenario", "--context", requiredEnv("A8_CONTEXT"), "--name", "r2-vested-gift")
        genesis.waitForNextEpoch()
        runHarness("release-scenario", "--context", requiredEnv("A8_CONTEXT"), "--name", "r2-vested-gift")
        runHarness("r2-gift-checkpoint", "--context", requiredEnv("A8_CONTEXT"), "--name", "r2-vested-gift", "--stage", "final", "--gift-amount", gift.toString())
        r1.getOrThrow()
    }

    @Test
    fun `marketplace funded lock succeeds exactly at E`() {
        val config = fastMarketplaceConfig()
        val (cluster, genesis) = initCluster(config = config, reboot = true)
        cluster.allPairs.forEach { it.waitForMlNodesToLoad() }

        val targetEpoch = genesis.getEpochData().latestEpoch.index + 3
        // bootstrap deliberately occupies join1/E. Register a separate native
        // Host so this focused Deal cannot conflict with that permanent pair.
        val hostKey = createInactiveParticipant(genesis, "a8-lock-exact-e")
        runHarness(
            "bootstrap",
            "--context", requiredEnv("A8_CONTEXT"),
            "--run-id", requiredEnv("A8_RUN_ID"),
            "--target-epoch", targetEpoch.toString(),
            "--deal-wasm", requiredEnv("A8_DEAL_WASM"),
            "--factory-wasm", requiredEnv("A8_FACTORY_WASM"),
            "--cw20-wasm", requiredEnv("A8_CW20_WASM"),
            "--caller-wasm", requiredEnv("A8_CALLER_WASM"),
        )
        // Host=the dedicated participant, Buyer=join2, and Lock fee payer=genesis
        // are distinct addresses.
        prepareDeal(
            "lock-exact-e",
            targetEpoch,
            funded = true,
            hostNode = "genesis-node",
            hostKey = hostKey,
        )

        genesis.markNeedsReboot()
        logSection("Leave the funded Deal unlocked until its exact E boundary")
        while (genesis.getEpochData().latestEpoch.index < targetEpoch) {
            genesis.waitForNextEpoch()
        }
        runHarness(
            "lock-exact-e-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "lock-exact-e",
        )
    }

    @Test
    fun `marketplace funded lock succeeds exactly at E plus 4`() {
        val config = fastMarketplaceConfig()
        val (cluster, genesis) = initCluster(config = config, reboot = true)
        cluster.allPairs.forEach { it.waitForMlNodesToLoad() }

        val targetEpoch = genesis.getEpochData().latestEpoch.index + 3
        // bootstrap deliberately occupies join1/E. Register a separate native
        // Host so this focused Deal cannot conflict with that permanent pair.
        val hostKey = createInactiveParticipant(genesis, "a8-lock-e-plus-4")
        runHarness(
            "bootstrap",
            "--context", requiredEnv("A8_CONTEXT"),
            "--run-id", requiredEnv("A8_RUN_ID"),
            "--target-epoch", targetEpoch.toString(),
            "--deal-wasm", requiredEnv("A8_DEAL_WASM"),
            "--factory-wasm", requiredEnv("A8_FACTORY_WASM"),
            "--cw20-wasm", requiredEnv("A8_CW20_WASM"),
            "--caller-wasm", requiredEnv("A8_CALLER_WASM"),
        )
        // Host=the dedicated participant, Buyer=join2, and Lock fee payer=genesis
        // are distinct addresses.
        prepareDeal(
            "lock-e-plus-4",
            targetEpoch,
            funded = true,
            hostNode = "genesis-node",
            hostKey = hostKey,
        )

        genesis.markNeedsReboot()
        logSection("Leave the funded Deal unlocked until its exact E+4 boundary")
        while (genesis.getEpochData().latestEpoch.index < targetEpoch + 4) {
            genesis.waitForNextEpoch()
        }
        runHarness(
            "lock-e-plus-4-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "lock-e-plus-4",
        )
    }

    @Test
    fun `marketplace funded lock rejects exactly at E plus 5`() {
        val config = fastMarketplaceConfig()
        val (cluster, genesis) = initCluster(config = config, reboot = true)
        cluster.allPairs.forEach { it.waitForMlNodesToLoad() }

        val targetEpoch = genesis.getEpochData().latestEpoch.index + 3
        // A dedicated native Host avoids the bootstrap join1/E fixture pair.
        val hostKey = createInactiveParticipant(genesis, "a8-lock-e-plus-5")
        runHarness(
            "bootstrap",
            "--context", requiredEnv("A8_CONTEXT"),
            "--run-id", requiredEnv("A8_RUN_ID"),
            "--target-epoch", targetEpoch.toString(),
            "--deal-wasm", requiredEnv("A8_DEAL_WASM"),
            "--factory-wasm", requiredEnv("A8_FACTORY_WASM"),
            "--cw20-wasm", requiredEnv("A8_CW20_WASM"),
            "--caller-wasm", requiredEnv("A8_CALLER_WASM"),
        )
        // Host, Buyer, and genesis (the fee-paying Lock caller) are distinct.
        prepareDeal(
            "lock-e-plus-5",
            targetEpoch,
            funded = true,
            hostNode = "genesis-node",
            hostKey = hostKey,
        )

        genesis.markNeedsReboot()
        logSection("Leave the funded Deal unlocked until its exact E+5 boundary")
        while (genesis.getEpochData().latestEpoch.index < targetEpoch + 5) {
            genesis.waitForNextEpoch()
        }
        runHarness(
            "lock-e-plus-5-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "lock-e-plus-5",
            "--gas", "2000000",
        )
    }

    @Test
    fun `marketplace zero unclaimed summary refunds only at claim expiry`() {
        // This is a special test-network genesis setting, not a production default.
        // Native Params.Validate accepts uint64 zero, and the unchanged settlement
        // keeper still writes an EpochPerformanceSummary for the active participant.
        val config = fastMarketplaceConfig(initialEpochReward = 0L)
        val (cluster, genesis) = initCluster(config = config, reboot = true)
        cluster.allPairs.forEach { it.waitForMlNodesToLoad() }

        val unclaimedHost = cluster.joinPairs.first()
        val targetEpoch = genesis.getEpochData().latestEpoch.index + 3

        logSection("Deploy Marketplace under the zero-subsidy test genesis")
        runHarness(
            "bootstrap",
            "--context", requiredEnv("A8_CONTEXT"),
            "--run-id", requiredEnv("A8_RUN_ID"),
            "--target-epoch", targetEpoch.toString(),
            "--deal-wasm", requiredEnv("A8_DEAL_WASM"),
            "--factory-wasm", requiredEnv("A8_FACTORY_WASM"),
            "--cw20-wasm", requiredEnv("A8_CW20_WASM"),
            "--caller-wasm", requiredEnv("A8_CALLER_WASM"),
            "--host-node", "genesis-node",
            "--host-key", "genesis",
            "--expected-initial-epoch-reward", "0",
        )
        prepareDeal(
            "claim-expiry-zero",
            targetEpoch,
            funded = true,
            hostNode = "join1-node",
            hostKey = "join1",
        )

        genesis.markNeedsReboot()
        logSection("Reach E-1 and preserve the ordinary active Host snapshot")
        while (genesis.getEpochData().latestEpoch.index < targetEpoch - 1) {
            genesis.waitForNextEpoch()
        }
        genesis.waitForStage(EpochStage.END_OF_POC_VALIDATION, offset = 0)
        unclaimedHost.stopApiContainer()

        logSection("Enter E=$targetEpoch and lock the exact native recipient")
        while (genesis.getEpochData().latestEpoch.index < targetEpoch) {
            genesis.waitForNextEpoch()
        }
        runHarness(
            "lock-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "claim-expiry-zero",
        )

        logSection("At E+1 the exact zero summary exists but Refund is too early")
        while (genesis.getEpochData().latestEpoch.index < targetEpoch + 1) {
            genesis.waitForNextEpoch()
        }
        runHarness(
            "verify-unclaimed-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "claim-expiry-zero",
            "--require-zero",
        )
        runHarness(
            "refund-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "claim-expiry-zero",
            "--expect", "failure",
            "--reason", "too_early",
        )

        logSection("At E+2 the same exact zero summary remains unclaimed")
        while (genesis.getEpochData().latestEpoch.index < targetEpoch + 2) {
            genesis.waitForNextEpoch()
        }
        runHarness(
            "verify-unclaimed-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "claim-expiry-zero",
            "--require-zero",
        )
        runHarness(
            "refund-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "claim-expiry-zero",
            "--expect", "success",
            "--reason", "claim_expiry",
        )
    }

    @Test
    fun `marketplace positive unclaimed summary refunds only at claim expiry`() {
        val config = fastMarketplaceConfig()
        val (cluster, genesis) = initCluster(config = config, reboot = true)
        cluster.allPairs.forEach { it.waitForMlNodesToLoad() }

        // join1 is a real active participant with PoC weight. Its DAPI stays up
        // through target-E PoC validation so native settlement can assign a
        // positive reward, then stops before the following CLAIM_REWARDS stage.
        val unclaimedHost = cluster.joinPairs.first()
        val targetEpoch = genesis.getEpochData().latestEpoch.index + 3

        logSection("Deploy Marketplace and fund an isolated claim-expiry Deal")
        runHarness(
            "bootstrap",
            "--context", requiredEnv("A8_CONTEXT"),
            "--run-id", requiredEnv("A8_RUN_ID"),
            "--target-epoch", targetEpoch.toString(),
            "--deal-wasm", requiredEnv("A8_DEAL_WASM"),
            "--factory-wasm", requiredEnv("A8_FACTORY_WASM"),
            "--cw20-wasm", requiredEnv("A8_CW20_WASM"),
            "--caller-wasm", requiredEnv("A8_CALLER_WASM"),
            "--host-node", "genesis-node",
            "--host-key", "genesis",
        )
        prepareDeal(
            "claim-expiry-positive",
            targetEpoch,
            funded = true,
            hostNode = "join1-node",
            hostKey = "join1",
        )

        genesis.markNeedsReboot()
        logSection("Reach E-1 and keep Host active through target-E settlement")
        while (genesis.getEpochData().latestEpoch.index < targetEpoch - 1) {
            genesis.waitForNextEpoch()
        }
        genesis.waitForStage(EpochStage.END_OF_POC_VALIDATION, offset = 0)
        unclaimedHost.stopApiContainer()

        logSection("Enter E=$targetEpoch and lock the exact native recipient")
        while (genesis.getEpochData().latestEpoch.index < targetEpoch) {
            genesis.waitForNextEpoch()
        }
        runHarness(
            "lock-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "claim-expiry-positive",
        )

        logSection("At E+1 the settled summary must be positive and Refund must fail closed")
        while (genesis.getEpochData().latestEpoch.index < targetEpoch + 1) {
            genesis.waitForNextEpoch()
        }
        runHarness(
            "verify-unclaimed-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "claim-expiry-positive",
            "--require-positive",
        )
        runHarness(
            "refund-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "claim-expiry-positive",
            "--expect", "failure",
            "--reason", "too_early",
        )

        logSection("At E+2 the same positive summary must remain unclaimed")
        while (genesis.getEpochData().latestEpoch.index < targetEpoch + 2) {
            genesis.waitForNextEpoch()
        }
        runHarness(
            "verify-unclaimed-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "claim-expiry-positive",
            "--require-positive",
        )
        runHarness(
            "refund-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "claim-expiry-positive",
            "--expect", "success",
            "--reason", "claim_expiry",
        )
    }

    @Test
    fun `marketplace absent native summary refunds only at emergency deadline`() {
        val config = fastMarketplaceConfig()
        val (cluster, genesis) = initCluster(config = config, reboot = true)
        cluster.allPairs.forEach { it.waitForMlNodesToLoad() }

        // Configure a real recipient while join2 is registered, then keep its DAPI
        // offline for five complete epochs before E. Native settlement only writes
        // summaries for the active participant snapshot, so exact Host/E is absent.
        val absentSummaryHost = cluster.joinPairs[1]
        val targetEpoch = genesis.getEpochData().latestEpoch.index + 5

        logSection("Deploy and fund an isolated emergency-refund Deal")
        runHarness(
            "bootstrap",
            "--context", requiredEnv("A8_CONTEXT"),
            "--run-id", requiredEnv("A8_RUN_ID"),
            "--target-epoch", targetEpoch.toString(),
            "--deal-wasm", requiredEnv("A8_DEAL_WASM"),
            "--factory-wasm", requiredEnv("A8_FACTORY_WASM"),
            "--cw20-wasm", requiredEnv("A8_CW20_WASM"),
            "--caller-wasm", requiredEnv("A8_CALLER_WASM"),
            "--host-node", "genesis-node",
            "--host-key", "genesis",
        )
        prepareDeal(
            "network-unconfirmed",
            targetEpoch,
            funded = true,
            hostNode = "join2-node",
            hostKey = "join2",
        )

        genesis.markNeedsReboot()
        absentSummaryHost.stopApiContainer()
        logSection("Keep Host offline through E=$targetEpoch and lock its exact recipient")
        while (genesis.getEpochData().latestEpoch.index < targetEpoch) {
            genesis.waitForNextEpoch()
        }
        runHarness(
            "lock-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "network-unconfirmed",
        )

        logSection("At E+2 exact native NotFound still fails closed")
        while (genesis.getEpochData().latestEpoch.index < targetEpoch + 2) {
            genesis.waitForNextEpoch()
        }
        runHarness(
            "verify-missing-summary-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "network-unconfirmed",
            "--expected-offset", "2",
        )
        runHarness(
            "refund-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "network-unconfirmed",
            "--expect", "failure",
            "--reason", "network_unconfirmed_too_early",
        )

        logSection("At E+3 the same native NotFound permits NetworkUnconfirmed")
        while (genesis.getEpochData().latestEpoch.index < targetEpoch + 3) {
            genesis.waitForNextEpoch()
        }
        runHarness(
            "verify-missing-summary-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "network-unconfirmed",
            "--expected-offset", "3",
        )
        runHarness(
            "refund-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "network-unconfirmed",
            "--expect", "success",
            "--reason", "network_unconfirmed",
        )
    }

    @Test
    fun `marketplace terminal release repeat is a native no-op`() {
        val config = fastMarketplaceConfig()
        val (cluster, genesis) = initCluster(config = config, reboot = true)
        cluster.allPairs.forEach { it.waitForMlNodesToLoad() }

        val participant = cluster.joinPairs.first()
        val targetEpoch = genesis.getEpochData().latestEpoch.index + 3

        logSection("Deploy one funded G3 Deal with independent financial roles")
        runHarness(
            "bootstrap",
            "--context", requiredEnv("A8_CONTEXT"),
            "--run-id", requiredEnv("A8_RUN_ID"),
            "--target-epoch", targetEpoch.toString(),
            "--deal-wasm", requiredEnv("A8_DEAL_WASM"),
            "--factory-wasm", requiredEnv("A8_FACTORY_WASM"),
            "--cw20-wasm", requiredEnv("A8_CW20_WASM"),
            "--caller-wasm", requiredEnv("A8_CALLER_WASM"),
        )

        genesis.markNeedsReboot()
        logSection("Reach E=$targetEpoch and lock the exact native recipient")
        while (genesis.getEpochData().latestEpoch.index < targetEpoch) {
            genesis.waitForNextEpoch()
        }
        runHarness("lock", "--context", requiredEnv("A8_CONTEXT"))
        val rewardSeed = participant.api.getConfig().currentSeed
        check(rewardSeed.epochIndex == targetEpoch) {
            "Testermint reward seed epoch ${rewardSeed.epochIndex} != Deal epoch $targetEpoch"
        }

        participant.stopApiContainer()
        logSection("Claim the positive native reward and settle the funded Deal")
        genesis.waitForStage(EpochStage.CLAIM_REWARDS, offset = 2)
        runHarness(
            "claim-settle",
            "--context", requiredEnv("A8_CONTEXT"),
            "--reward-seed", rewardSeed.seed.toString(),
            "--reward-epoch", rewardSeed.epochIndex.toString(),
        )

        participant.restartApiContainer()
        genesis.node.waitForNextBlock(2)
        genesis.waitForStage(EpochStage.CLAIM_REWARDS, offset = -1)
        logSection("Wait for both native vesting tranches and release through Completed")
        genesis.waitForStage(EpochStage.CLAIM_REWARDS, offset = 2)
        genesis.node.waitForNextBlock(2)
        while (genesis.getEpochData().latestEpoch.index < targetEpoch + 2) {
            genesis.waitForNextEpoch()
        }
        genesis.node.waitForNextBlock(2)
        runHarness("release", "--context", requiredEnv("A8_CONTEXT"))

        logSection("Unlock and release the second native tranche through Completed")
        genesis.waitForStage(EpochStage.CLAIM_REWARDS, offset = 2)
        genesis.node.waitForNextBlock(2)
        runHarness("release", "--context", requiredEnv("A8_CONTEXT"))

        logSection("Broadcast a real zero-balance ReleaseUnlockedGnk from an independent caller")
        runHarness("terminal-release-repeat", "--context", requiredEnv("A8_CONTEXT"))
    }

    @Test
    fun `marketplace R7 dot 1 rejects selected second Bank send then retries once`() {
        val config = fastMarketplaceConfig()
        val (cluster, genesis) = initCluster(config = config, reboot = true)
        cluster.allPairs.forEach { it.waitForMlNodesToLoad() }
        val participant = cluster.joinPairs.first()
        val targetEpoch = genesis.getEpochData().latestEpoch.index + 3

        runHarness(
            "bootstrap", "--context", requiredEnv("A8_CONTEXT"), "--run-id", requiredEnv("A8_RUN_ID"),
            "--target-epoch", targetEpoch.toString(), "--deal-wasm", requiredEnv("A8_DEAL_WASM"),
            "--factory-wasm", requiredEnv("A8_FACTORY_WASM"), "--cw20-wasm", requiredEnv("A8_CW20_WASM"),
            "--caller-wasm", requiredEnv("A8_CALLER_WASM"),
        )
        genesis.markNeedsReboot()
        while (genesis.getEpochData().latestEpoch.index < targetEpoch) genesis.waitForNextEpoch()
        runHarness("lock", "--context", requiredEnv("A8_CONTEXT"))
        val rewardSeed = participant.api.getConfig().currentSeed
        check(rewardSeed.epochIndex == targetEpoch)
        participant.stopApiContainer()
        genesis.waitForStage(EpochStage.CLAIM_REWARDS, offset = 2)
        runHarness(
            "claim-settle", "--context", requiredEnv("A8_CONTEXT"),
            "--reward-seed", rewardSeed.seed.toString(), "--reward-epoch", rewardSeed.epochIndex.toString(),
        )
        participant.restartApiContainer()
        genesis.node.waitForNextBlock(2)
        genesis.waitForStage(EpochStage.CLAIM_REWARDS, offset = -1)
        genesis.waitForStage(EpochStage.CLAIM_REWARDS, offset = 2)
        genesis.node.waitForNextBlock(2)
        while (genesis.getEpochData().latestEpoch.index < targetEpoch + 2) genesis.waitForNextEpoch()
        genesis.node.waitForNextBlock(2)

        // Governance may cross an epoch. Freeze the payout calculation only
        // after BOTH original vesting tranches are liquid, and prove it natively.
        while (genesis.getEpochData().latestEpoch.index < targetEpoch + 3) genesis.waitForNextEpoch()
        genesis.waitForStage(EpochStage.CLAIM_REWARDS, offset = 2)
        runHarness("bank-release-fault-plan", "--context", requiredEnv("A8_CONTEXT"),
            "--name", "bootstrap", "--require-fully-vested")
        val phases = JsonParser.parseString(File(requiredEnv("A8_CONTEXT")).readText())
            .asJsonObject["phases"].asJsonArray
        val plan = phases.last { it.asJsonObject["name"].asString == "native_bank_release_fault_plan" }
            .asJsonObject["plan"].asJsonObject
        val restrictionEndBlock = genesis.node.queryRestrictionsStatus().currentBlockHeight + 50
        val firstProposalId = genesis.runProposal(
            cluster,
            UpdateRestrictionsParams(
                params = RestrictionsParams(
                    restrictionEndBlock = restrictionEndBlock,
                    emergencyTransferExemptions = emptyList(),
                    exemptionUsageTracking = emptyList(),
                )
            ),
        )
        check(genesis.node.queryRestrictionsStatus().isActive)
        runHarness(
            "bank-release-rollback-scenario", "--context", requiredEnv("A8_CONTEXT"), "--name", "bootstrap",
            "--proposal-id", firstProposalId, "--expected-send-index", "1",
            "--rejected-recipient", plan["buyer"].asString,
        )
        val restrictions = a8BankSendFaultPlan(
            restrictionEndBlock, plan["deal"].asString, plan["buyer"].asString, plan["host"].asString,
            plan["buyer_amount"].asLong, plan["host_amount"].asLong, "a8-r7-1-buyer-first",
        )
        val proposalId = genesis.runProposal(cluster, UpdateRestrictionsParams(params = restrictions.params))
        check(genesis.node.queryRestrictionsStatus().isActive)
        runHarness(
            "bank-release-rollback-scenario", "--context", requiredEnv("A8_CONTEXT"), "--name", "bootstrap",
            "--proposal-id", proposalId, "--expected-send-index", "2",
            "--allowed-earlier-recipient", restrictions.allowedEarlierRecipient!!,
            "--rejected-recipient", restrictions.rejectedRecipient,
            "--exemption-id", "a8-r7-1-buyer-first",
        )
        genesis.node.waitForMinimumBlock(restrictionEndBlock + 1, "A8 R7.1 restriction expiry")
        check(!genesis.node.queryRestrictionsStatus().isActive)
        runHarness(
            "bank-release-retry-scenario", "--context", requiredEnv("A8_CONTEXT"), "--name", "bootstrap",
            "--expected-send-index", "2", "--rejected-recipient", restrictions.rejectedRecipient,
        )
    }

    @Test
    fun `marketplace R6 dot 1 rejects all three selected CW20 sends then settles once`() {
        val config = fastMarketplaceConfig()
        val (cluster, genesis) = initCluster(config = config, reboot = true)
        cluster.allPairs.forEach { it.waitForMlNodesToLoad() }
        val participant = cluster.joinPairs.first()
        val targetEpoch = genesis.getEpochData().latestEpoch.index + 3
        // Price below the observed positive reward makes Host net, fee, and
        // Buyer refund all non-zero; the Python oracle rejects another shape.
        runHarness(
            "bootstrap", "--context", requiredEnv("A8_CONTEXT"), "--run-id", requiredEnv("A8_RUN_ID"),
            "--target-epoch", targetEpoch.toString(), "--price", "1000",
            "--deal-wasm", requiredEnv("A8_DEAL_WASM"), "--factory-wasm", requiredEnv("A8_FACTORY_WASM"),
            "--cw20-wasm", requiredEnv("A8_CW20_WASM"), "--caller-wasm", requiredEnv("A8_CALLER_WASM"),
        )
        genesis.markNeedsReboot()
        while (genesis.getEpochData().latestEpoch.index < targetEpoch) genesis.waitForNextEpoch()
        runHarness("lock", "--context", requiredEnv("A8_CONTEXT"))
        val rewardSeed = participant.api.getConfig().currentSeed
        check(rewardSeed.epochIndex == targetEpoch)
        participant.stopApiContainer()
        genesis.waitForStage(EpochStage.CLAIM_REWARDS, offset = 2)
        runHarness(
            "claim-settle", "--context", requiredEnv("A8_CONTEXT"),
            "--reward-seed", rewardSeed.seed.toString(), "--reward-epoch", rewardSeed.epochIndex.toString(),
            "--cw20-fault-positions", "1,2,3",
        )
    }

    @Test
    fun `marketplace late liquid donations after Completed use cumulative GNK rounding`() {
        val config = fastMarketplaceConfig()
        val (cluster, genesis) = initCluster(config = config, reboot = true)
        cluster.allPairs.forEach { it.waitForMlNodesToLoad() }

        val participant = cluster.joinPairs.first()
        val targetEpoch = genesis.getEpochData().latestEpoch.index + 3

        logSection("Deploy one funded B2 Deal with independent financial roles")
        runHarness(
            "bootstrap",
            "--context", requiredEnv("A8_CONTEXT"),
            "--run-id", requiredEnv("A8_RUN_ID"),
            "--target-epoch", targetEpoch.toString(),
            "--deal-wasm", requiredEnv("A8_DEAL_WASM"),
            "--factory-wasm", requiredEnv("A8_FACTORY_WASM"),
            "--cw20-wasm", requiredEnv("A8_CW20_WASM"),
            "--caller-wasm", requiredEnv("A8_CALLER_WASM"),
        )

        genesis.markNeedsReboot()
        while (genesis.getEpochData().latestEpoch.index < targetEpoch) {
            genesis.waitForNextEpoch()
        }
        runHarness("lock", "--context", requiredEnv("A8_CONTEXT"))
        val rewardSeed = participant.api.getConfig().currentSeed
        check(rewardSeed.epochIndex == targetEpoch) {
            "Testermint reward seed ${rewardSeed.epochIndex} != B2 Deal epoch $targetEpoch"
        }

        participant.stopApiContainer()
        logSection("Claim the positive native reward and settle the funded Deal")
        genesis.waitForStage(EpochStage.CLAIM_REWARDS, offset = 2)
        runHarness(
            "claim-settle",
            "--context", requiredEnv("A8_CONTEXT"),
            "--reward-seed", rewardSeed.seed.toString(),
            "--reward-epoch", rewardSeed.epochIndex.toString(),
        )

        participant.restartApiContainer()
        genesis.node.waitForNextBlock(2)
        genesis.waitForStage(EpochStage.CLAIM_REWARDS, offset = -1)
        logSection("Release both original native tranches through Completed")
        genesis.waitForStage(EpochStage.CLAIM_REWARDS, offset = 2)
        genesis.node.waitForNextBlock(2)
        while (genesis.getEpochData().latestEpoch.index < targetEpoch + 2) {
            genesis.waitForNextEpoch()
        }
        genesis.node.waitForNextBlock(2)
        runHarness("release", "--context", requiredEnv("A8_CONTEXT"))

        genesis.waitForStage(EpochStage.CLAIM_REWARDS, offset = 2)
        genesis.node.waitForNextBlock(2)
        runHarness("release", "--context", requiredEnv("A8_CONTEXT"))

        logSection("Donate liquid GNK twice after Completed and prove cumulative rounding")
        runHarness(
            "late-donation",
            "--context", requiredEnv("A8_CONTEXT"),
            "--amount", "1423",
            "--second-amount", "1423",
        )
    }

    @Test
    fun `marketplace successful release preserves foreign native denom`() {
        val config = fastMarketplaceConfig(enableB3ForeignNativeFixture = true)
        val (cluster, genesis) = initCluster(config = config, reboot = true)
        cluster.allPairs.forEach { it.waitForMlNodesToLoad() }

        val participant = cluster.joinPairs.first()
        val targetEpoch = genesis.getEpochData().latestEpoch.index + 3
        importB3ForeignNativeKey(genesis)
        check(genesis.node.getBalance(B3_FOREIGN_ADDRESS, B3_FOREIGN_DENOM).balance.amount == B3_FOREIGN_AMOUNT) {
            "B3 genesis fixture did not preserve its exact foreign native balance"
        }

        logSection("Deploy one funded B3 Deal with independent financial roles")
        runHarness(
            "bootstrap",
            "--context", requiredEnv("A8_CONTEXT"),
            "--run-id", requiredEnv("A8_RUN_ID"),
            "--target-epoch", targetEpoch.toString(),
            "--deal-wasm", requiredEnv("A8_DEAL_WASM"),
            "--factory-wasm", requiredEnv("A8_FACTORY_WASM"),
            "--cw20-wasm", requiredEnv("A8_CW20_WASM"),
            "--caller-wasm", requiredEnv("A8_CALLER_WASM"),
        )

        genesis.markNeedsReboot()
        while (genesis.getEpochData().latestEpoch.index < targetEpoch) {
            genesis.waitForNextEpoch()
        }
        runHarness("lock", "--context", requiredEnv("A8_CONTEXT"))
        val rewardSeed = participant.api.getConfig().currentSeed
        check(rewardSeed.epochIndex == targetEpoch) {
            "Testermint reward seed ${rewardSeed.epochIndex} != B3 Deal epoch $targetEpoch"
        }

        participant.stopApiContainer()
        genesis.waitForStage(EpochStage.CLAIM_REWARDS, offset = 2)
        runHarness(
            "claim-settle",
            "--context", requiredEnv("A8_CONTEXT"),
            "--reward-seed", rewardSeed.seed.toString(),
            "--reward-epoch", rewardSeed.epochIndex.toString(),
        )

        participant.restartApiContainer()
        genesis.node.waitForNextBlock(2)
        genesis.waitForStage(EpochStage.CLAIM_REWARDS, offset = -1)
        genesis.waitForStage(EpochStage.CLAIM_REWARDS, offset = 2)
        genesis.node.waitForNextBlock(2)
        while (genesis.getEpochData().latestEpoch.index < targetEpoch + 2) {
            genesis.waitForNextEpoch()
        }
        genesis.node.waitForNextBlock(2)
        runHarness(
            "b3-foreign-native-release",
            "--context", requiredEnv("A8_CONTEXT"),
            "--foreign-key", B3_FOREIGN_KEY,
            "--foreign-address", B3_FOREIGN_ADDRESS,
            "--foreign-denom", B3_FOREIGN_DENOM,
            "--foreign-amount", B3_FOREIGN_AMOUNT.toString(),
        )
    }

    @Test
    fun `marketplace funded claim settles and releases on real Gonka`() {
        val config = fastMarketplaceConfig()
        val (cluster, genesis) = initCluster(config = config, reboot = true)
        cluster.allPairs.forEach { it.waitForMlNodesToLoad() }

        val participant = cluster.joinPairs.first()
        val absentSummaryParticipant = cluster.joinPairs[1]
        val lockEPlus4Participant = createInactiveParticipant(genesis, "a8-lock-e-plus-4")
        val lockEPlus5Participant = createInactiveParticipant(genesis, "a8-lock-e-plus-5")
        val targetEpoch = genesis.getEpochData().latestEpoch.index + 3

        logSection("Deploy Marketplace, configure exact Deal recipient, and fund CW20")
        runHarness(
            "bootstrap",
            "--context", requiredEnv("A8_CONTEXT"),
            "--run-id", requiredEnv("A8_RUN_ID"),
            "--target-epoch", targetEpoch.toString(),
            "--deal-wasm", requiredEnv("A8_DEAL_WASM"),
            "--factory-wasm", requiredEnv("A8_FACTORY_WASM"),
            "--cw20-wasm", requiredEnv("A8_CW20_WASM"),
            "--caller-wasm", requiredEnv("A8_CALLER_WASM"),
        )

        val routingMissingEpoch = targetEpoch + 1
        val gasEpoch = targetEpoch + 2
        val noSaleEpoch = gasEpoch
        // Claim-expiry is prepared at E so its E+1/E+2 boundaries can be
        // exercised before the long settlement/vesting sequence advances time.
        val expiryEpoch = targetEpoch
        val emergencyEpoch = targetEpoch + 4
        prepareDeal("no-sale", noSaleEpoch, funded = false)
        prepareDeal(
            "gas-claimed",
            gasEpoch,
            funded = true,
            hostNode = "genesis-node",
            hostKey = "genesis",
        )
        // Bootstrap already occupies the default join1/E pair. Use the live
        // genesis participant for the unclaimed claim-expiry fixture so the
        // authoritative native summary remains available after join2 is
        // stopped for the NetworkUnconfirmed scenario.
        prepareDeal(
            "claim-expiry",
            expiryEpoch,
            funded = true,
            hostNode = "genesis-node",
            hostKey = lockEPlus4Participant,
        )
        prepareDeal(
            "no-buyer-expired",
            emergencyEpoch,
            funded = false,
        )
        prepareDeal(
            "network-unconfirmed",
            emergencyEpoch,
            funded = true,
            hostNode = "join2-node",
            hostKey = "join2",
        )
        prepareDeal(
            "routing-mismatch",
            targetEpoch,
            funded = true,
            hostNode = "genesis-node",
            hostKey = "genesis",
        )
        runHarness(
            "set-scenario-routing",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "routing-mismatch",
            "--recipient", "buyer",
        )
        prepareDeal(
            "routing-missing",
            routingMissingEpoch,
            funded = true,
            hostNode = "genesis-node",
            hostKey = "genesis",
        )
        runHarness(
            "set-scenario-routing",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "routing-missing",
            "--recipient", "missing",
        )
        prepareDeal(
            "lock-e-plus-4",
            gasEpoch,
            funded = false,
            hostNode = "genesis-node",
            hostKey = lockEPlus4Participant,
        )
        prepareDeal(
            "lock-e-plus-5",
            gasEpoch,
            funded = false,
            hostNode = "genesis-node",
            hostKey = lockEPlus5Participant,
        )
        // The Host must exist when the native routing row is configured. Stop
        // its off-chain API afterwards so the future epoch has no reward
        // summary and exercises the real NetworkUnconfirmed path.
        genesis.markNeedsReboot()
        logSection("Wait for target epoch $targetEpoch and lock the exact routing proof")
        while (genesis.getEpochData().latestEpoch.index < targetEpoch) {
            genesis.waitForNextEpoch()
        }
        runHarness("lock", "--context", requiredEnv("A8_CONTEXT"))
        runHarness(
            "lock-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "claim-expiry",
        )
        runHarness(
            "refund-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "routing-mismatch",
            "--expect", "success",
            "--reason", "routing_mismatch",
            "--fault-cw20",
        )

        runHarness(
            "verify-unclaimed-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "claim-expiry",
        )
        val rewardSeed = participant.api.getConfig().currentSeed
        check(rewardSeed.epochIndex == targetEpoch) {
            "Testermint reward seed epoch ${rewardSeed.epochIndex} != Deal epoch $targetEpoch"
        }
        participant.stopApiContainer()
        runHarness(
            "refund-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "claim-expiry",
            "--expect", "failure",
            "--reason", "too_early",
        )
        while (genesis.getEpochData().latestEpoch.index < targetEpoch + 2) {
            genesis.waitForNextEpoch()
        }
        runHarness(
            "refund-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "claim-expiry",
            "--expect", "success",
            "--reason", "claim_expiry",
        )
        // Keep the inactive host's native summary unclaimed through E/E+2;
        // stop join2 only before the later NetworkUnconfirmed scenario.
        absentSummaryParticipant.stopApiContainer()
        logSection("Auto-claim stopped; wait for native claim window")
        genesis.waitForStage(EpochStage.CLAIM_REWARDS, offset = 2)

        runHarness(
            "claim-settle",
            "--context", requiredEnv("A8_CONTEXT"),
            "--reward-seed", rewardSeed.seed.toString(),
            "--reward-epoch", rewardSeed.epochIndex.toString(),
            "--fault-retry",
        )

        participant.restartApiContainer()
        genesis.node.waitForNextBlock(2)
        genesis.waitForStage(EpochStage.CLAIM_REWARDS, offset = -1)
        runHarness(
            "refund-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "routing-missing",
            "--expect", "success",
            "--reason", "routing_missing",
        )

        logSection("Advance to $gasEpoch: first funded unlock and no-sale native claim")
        genesis.waitForStage(EpochStage.CLAIM_REWARDS, offset = 2)
        genesis.node.waitForNextBlock(2)
        runHarness(
            "verify-claimed-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "no-sale",
            "--require-positive",
        )
        while (genesis.getEpochData().latestEpoch.index < gasEpoch) {
            genesis.waitForNextEpoch()
        }
        genesis.node.waitForNextBlock(2)
        runHarness("lock-scenario", "--context", requiredEnv("A8_CONTEXT"), "--name", "no-sale")
        runHarness(
            "verify-claimed-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "gas-claimed",
            "--require-positive",
        )
        runHarness("release", "--context", requiredEnv("A8_CONTEXT"))
        runHarness("lock-scenario", "--context", requiredEnv("A8_CONTEXT"), "--name", "gas-claimed")
        runHarness(
            "donate-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "no-sale",
            "--label", "before_settlement",
            "--amount", "3",
        )
        runHarness(
            "contaminate-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "no-sale",
            "--amount", "7",
        )
        runHarness("settle-scenario", "--context", requiredEnv("A8_CONTEXT"), "--name", "no-sale")
        val vestingDonation = 10_000_000_001L
        val governanceAddress = genesis.node.getModuleAccount("gov").account.value.address
        val genesisAddress = genesis.node.getColdAddress()
        genesis.ensureGenesisSpendableForDevshard(vestingDonation)
        val vestingFundingTx = genesis.submitTransaction(
            listOf(
                "bank", "send", genesisAddress, governanceAddress,
                "$vestingDonation${genesis.config.denom}",
            )
        )
        check(vestingFundingTx.code == 0) { "governance funding failed: ${vestingFundingTx.rawLog}" }
        runHarness(
            "snapshot-vesting-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "no-sale",
            "--label", "before-additional-vesting",
            "--require-non-empty",
        )
        val vestingProposalId = genesis.runProposal(
            cluster,
            MsgTransferWithVesting(
                sender = governanceAddress,
                recipient = scenarioDeal("no-sale"),
                amount = listOf(Coin(genesis.config.denom, vestingDonation)),
                vestingEpochs = 2,
            ),
        )
        runHarness(
            "verify-vesting-addition-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "no-sale",
            "--before-label", "before-additional-vesting",
            "--amount", vestingDonation.toString(),
            "--vesting-epochs", "2",
            "--fund-tx-hash", vestingFundingTx.txhash,
            "--proposal-id", vestingProposalId,
        )
        runHarness(
            "donate-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "no-sale",
            "--label", "after_settlement",
            "--amount", "2",
        )
        logSection("Complete funded Deal and claim gas-regression Deal")
        genesis.waitForStage(EpochStage.CLAIM_REWARDS, offset = 2)
        genesis.node.waitForNextBlock(2)
        runHarness("release-scenario", "--context", requiredEnv("A8_CONTEXT"), "--name", "no-sale")
        runHarness("late-donation", "--context", requiredEnv("A8_CONTEXT"))
        logSection("Advance to $emergencyEpoch: finish no-sale vesting and lock absent-summary Deal")
        genesis.waitForStage(EpochStage.CLAIM_REWARDS, offset = 2)
        genesis.node.waitForNextBlock(2)
        runHarness("release-scenario", "--context", requiredEnv("A8_CONTEXT"), "--name", "no-sale")
        runHarness(
            "lock-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "network-unconfirmed",
        )
        runHarness(
            "lock-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "no-buyer-expired",
        )
        logSection("Continue at absolute E+3 gas boundary")
        runHarness(
            "gas-sweep-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "gas-claimed",
        )
        runHarness("release-scenario", "--context", requiredEnv("A8_CONTEXT"), "--name", "no-sale")

        logSection("Advance to emergency E+2: unavailable summary must still fail closed")
        genesis.waitForNextEpoch()
        runHarness(
            "verify-missing-summary-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "network-unconfirmed",
            "--expected-offset", "2",
        )
        runHarness(
            "refund-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "network-unconfirmed",
            "--expect", "failure",
            "--reason", "network_unconfirmed_too_early",
        )
        runHarness(
            "refund-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "no-buyer-expired",
            "--expect", "failure",
            "--reason", "too_early",
        )
        runHarness(
            "lock-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "lock-e-plus-4",
        )

        logSection("Advance to emergency E+3 and recipient pruning boundary E+5")
        genesis.waitForNextEpoch()
        runHarness(
            "verify-missing-summary-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "network-unconfirmed",
            "--expected-offset", "3",
        )
        runHarness(
            "refund-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "network-unconfirmed",
            "--expect", "success",
            "--reason", "network_unconfirmed",
        )
        runHarness(
            "refund-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "no-buyer-expired",
            "--expect", "success",
            "--reason", "claim_expiry",
        )
        runHarness(
            "donate-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "network-unconfirmed",
            "--label", "after_terminal_emergency_refund",
            "--amount", "11",
        )
        val restrictionStatus = genesis.node.queryRestrictionsStatus()
        val restrictionEndBlock = restrictionStatus.currentBlockHeight + 50
        val restrictionProposalId = genesis.runProposal(
            cluster,
            UpdateRestrictionsParams(
                params = RestrictionsParams(
                    restrictionEndBlock = restrictionEndBlock,
                    emergencyTransferExemptions = emptyList(),
                    exemptionUsageTracking = emptyList(),
                ),
            ),
        )
        check(genesis.node.queryRestrictionsStatus().isActive) {
            "transfer restrictions did not become active after proposal $restrictionProposalId"
        }
        val networkScenario = JsonParser.parseString(File(requiredEnv("A8_CONTEXT")).readText())
            .asJsonObject["scenarios"].asJsonObject["network-unconfirmed"].asJsonObject
        val networkHost = networkScenario["accounts"].asJsonObject["host"].asString
        runHarness(
            "bank-release-rollback-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "network-unconfirmed",
            "--proposal-id", restrictionProposalId,
            "--expected-send-index", "1",
            "--rejected-recipient", networkHost,
        )
        genesis.node.waitForMinimumBlock(restrictionEndBlock + 1, "A8 restriction expiry")
        check(!genesis.node.queryRestrictionsStatus().isActive) {
            "transfer restrictions remained active after block $restrictionEndBlock"
        }
        runHarness(
            "bank-release-retry-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "network-unconfirmed",
            "--expected-send-index", "1",
            "--rejected-recipient", networkHost,
        )
        runHarness(
            "lock-rejected-scenario",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", "lock-e-plus-5",
            "--routing", "pruned",
        )
        runHarness("verify-factory-isolation", "--context", requiredEnv("A8_CONTEXT"))
    }

    private fun fastMarketplaceConfig(
        initialEpochReward: Long? = null,
        enableB3ForeignNativeFixture: Boolean = false,
        enableCQueryFaults: Boolean = false,
        epochLength: Long = 25L,
    ): com.productscience.ApplicationConfig {
        val fastSpec = spec {
            this[AppState::inference] = spec<InferenceState> {
                this[InferenceState::params] = spec<InferenceParams> {
                    if (initialEpochReward != null) {
                        this[InferenceParams::bitcoinRewardParams] = spec<BitcoinRewardParams> {
                            this[BitcoinRewardParams::initialEpochReward] = initialEpochReward
                        }
                    }
                    this[InferenceParams::tokenomicsParams] = spec<TokenomicsParams> {
                        this[TokenomicsParams::workVestingPeriod] = 2L
                        this[TokenomicsParams::rewardVestingPeriod] = 2L
                    }
                    this[InferenceParams::epochParams] = spec<EpochParams> {
                        this[EpochParams::epochLength] = epochLength
                    }
                }
            }
            this[AppState::restrictions] = spec<RestrictionsState> {
                this[RestrictionsState::params] = spec<RestrictionsParams> {
                    this[RestrictionsParams::restrictionEndBlock] = 0L
                }
            }
            if (enableB3ForeignNativeFixture || enableCQueryFaults) {
                // The stock test override has a 24h expedited voting period
                // alongside a 30s regular period.  B3 does not exercise
                // governance, but its genesis preflight must validate.
                this[AppState::gov] = spec<GovState> {
                    this[GovState::params] = spec<GovParams> {
                        this[GovParams::expeditedVotingPeriod] = Duration.ofSeconds(15)
                    }
                }
            }
        }
        val config = inferenceConfig.copy(
            genesisSpec = inferenceConfig.genesisSpec?.merge(fastSpec) ?: fastSpec,
            additionalDockerFilesByKeyName = if (enableCQueryFaults) {
                listOf(GENESIS_KEY_NAME, "join1", "join2").associateWith {
                    listOf("docker-compose.a8-query-faults.yml")
                }
            } else if (enableB3ForeignNativeFixture) {
                mapOf(GENESIS_KEY_NAME to listOf("docker-compose.genesis-a8-b3-foreign-denom.yml"))
            } else {
                emptyMap()
            },
        )
        return config
    }

    private fun importB3ForeignNativeKey(genesis: com.productscience.LocalInferencePair) {
        genesis.node.exec(
            listOf(
                genesis.node.config.execName,
                "keys",
                "add",
                B3_FOREIGN_KEY,
                "--recover",
                "--output",
                "json",
            ) + genesis.node.config.keychainParams,
            stdin = B3_FOREIGN_MNEMONIC + "\n",
        )
    }

    private fun runHarness(vararg args: String) {
        val command = listOf(requiredEnv("A8_PYTHON"), requiredEnv("A8_HARNESS")) + args
        val output = runMarketplaceHarnessProcess(
            command = command,
            directory = File(requiredEnv("A8_MARKETPLACE_DIR")),
            phase = args.firstOrNull(),
            timeout = 8,
            timeoutUnit = TimeUnit.MINUTES,
        )
        println(output.trim())
    }

    private fun prepareDeal(
        name: String,
        targetEpoch: Long,
        funded: Boolean,
        hostNode: String = "join1-node",
        hostKey: String = "join1",
    ) {
        val args = mutableListOf(
            "create-deal",
            "--context", requiredEnv("A8_CONTEXT"),
            "--name", name,
            "--target-epoch", targetEpoch.toString(),
            "--host-node", hostNode,
            "--host-key", hostKey,
            "--route-exact",
        )
        if (funded) args += "--fund"
        runHarness(*args.toTypedArray())
    }

    private fun scenarioDeal(name: String): String {
        val root = JsonParser.parseString(File(requiredEnv("A8_CONTEXT")).readText()).asJsonObject
        return root.getAsJsonObject("scenarios")
            .getAsJsonObject(name)
            .getAsJsonObject("contracts")
            .get("deal")
            .asString
    }

    private fun createInactiveParticipant(genesis: com.productscience.LocalInferencePair, prefix: String): String {
        val key = genesis.node.createKey("$prefix-${System.currentTimeMillis()}")
        genesis.api.addUnfundedInferenceParticipant(
            UnfundedInferenceParticipant(
                url = "",
                models = listOf(),
                validatorKey = "",
                pubKey = key.pubkey.key,
                address = key.address,
            ),
        )
        genesis.node.waitForNextBlock(2)
        return key.name
    }

    private fun requiredEnv(name: String): String =
        System.getenv(name)?.takeIf { it.isNotBlank() }
            ?: error("Required environment variable $name is missing")

    private companion object {
        const val B3_FOREIGN_KEY = "a8-b3-foreign"
        const val B3_FOREIGN_MNEMONIC = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
        const val B3_FOREIGN_ADDRESS = "gonka1k4swv40ur28fvu54p8mskjj4lxkgsj07u9f8ny"
        const val B3_FOREIGN_DENOM = "ua8b3foreign"
        const val B3_FOREIGN_AMOUNT = 12_345L
    }
}
