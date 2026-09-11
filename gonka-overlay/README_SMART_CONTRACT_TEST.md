# Local Verification of Smart Contract gRPC Allowlist

This guide describes a complete local run of the four contract-facing gRPC routes on a live Gonka network in Docker Desktop:

```text
smart-query JSON
  -> p0_probe.wasm
  -> QueryRequest::Grpc
  -> AcceptListGrpcQuerier
  -> native gRPC router/keeper
  -> protobuf response
  -> Rust decode
  -> contract JSON response
```

Target allowed routes to verify:

```text
/inference.inference.Query/GetCurrentEpoch
/inference.inference.Query/ListClaimRecipients
/inference.inference.Query/EpochPerformanceSummaryByParticipant
/inference.streamvesting.Query/TotalVestingAmount
```

Control denied route:

```text
/inference.inference.Query/EpochPerformanceSummaryAll
```

`p0-probe` is a diagnostic test-only fixture. Never deploy it to a public or production network.

## What Constitutes a Successful Verification

A run is considered successful when all of the following conditions are met:

- The Wasm fixture compiles reproducibly in the pinned Docker image;
- The compiled file checksum matches the manifest and the `store_code` event checksum;
- Focused Go tests, full `go test ./app`, `go vet`, and focused race tests pass;
- Chain and API images are built from the verified commit;
- The stateful Testermint test creates genesis, joins participants, and produces reward state;
- The node produces blocks and reports `catching_up=false`;
- An existing `EpochPerformanceSummary` record is located in native state;
- A single instance of the current `p0_probe.wasm` returns valid JSON for all four allowed queries;
- `EpochPerformanceSummaryAll` fails with `path is not allowed from the contract`;
- The commit, image IDs, artifact SHA-256, tx hashes, heights, code ID, contract address, payloads, and raw responses are recorded.

Empty `entries` and `total_amount` are valid transport/runtime responses. For the strongest evidence, obtain a non-empty claim-recipient state. For performance summaries, an actual existing record must be queried: querying a non-existent pair does not prove a positive runtime path.

## 1. Requirements

To execute the full run:

- Docker Desktop with Docker Engine, Compose v2, and buildx;
- JDK 21;
- Go version compatible with repository modules (Go 1.25.9 or newer recommended for full monorepo builds);
- `make`, `jq`, `git`, `shasum`;
- Internet access for initial downloads of base images, Gradle, and Go dependencies;
- On Apple Silicon — support for running `linux/arm64` and `linux/amd64` containers.

Recommended Docker Desktop resources:

```text
CPU:         8+
Memory:      16 GB
Swap:        4 GB
Free disk:   80 GB
```

The verified baseline run completed with approximately 8 GiB Docker RAM, but higher resources ensure stability for clean builds.

## 2. Navigate to Repository Root

All subsequent commands, unless stated otherwise, run from the root of the Gonka workspace:

```bash
cd "$(git rev-parse --show-toplevel)"
git status --short --branch
git rev-parse HEAD
```

Record the branch and full commit. Do not combine evidence compiled from one commit with source files from another.

## 3. Connect to Docker Desktop

Testermint utilizes both `docker compose` and the Docker API from Java. A Podman backend is not suitable for this run.

On macOS, stop the Podman machine if running:

```bash
/opt/podman/bin/podman machine stop
```

If the standard `/var/run/docker.sock` belongs to Docker Desktop:

```bash
unset DOCKER_HOST
unset CONTAINER_HOST
docker context use desktop-linux
```

If `/var/run/docker.sock` still points to Podman, explicitly configure the Docker Desktop socket:

```bash
export PATH="/Applications/Docker.app/Contents/Resources/bin:$PATH"
RUN_DOCKER_HOST="$(docker context inspect desktop-linux --format '{{(index .Endpoints "docker").Host}}')"
export DOCKER_HOST="$RUN_DOCKER_HOST"
unset CONTAINER_HOST
docker context use desktop-linux
```

Avoid printing the full environment as it may contain tokens. Verify only the necessary parameters:

```bash
docker version --format 'client={{.Client.Version}} server={{.Server.Version}} os={{.Server.Os}} arch={{.Server.Arch}}'
docker info --format 'name={{.Name}} os={{.OperatingSystem}} arch={{.Architecture}} cpus={{.NCPU}} memory={{.MemTotal}}'
docker context show
docker compose version
docker buildx version
docker buildx inspect --bootstrap
readlink /var/run/docker.sock || true
```

Criteria:

- Server is Docker Desktop/Docker Engine, not Podman;
- OS is not `fedora`;
- Active context is `desktop-linux`;
- `docker compose` and `docker buildx` are operational;
- Java processes inherit the same `DOCKER_HOST`.

`make check-docker` alone is insufficient as it checks only `docker info` exit code and may yield false positives under Podman.

## 4. Configure JDK and Verify Toolchain

Example for Homebrew OpenJDK 21 on Apple Silicon:

```bash
export JAVA_HOME="/opt/homebrew/opt/openjdk@21/libexec/openjdk.jdk/Contents/Home"
export PATH="/opt/homebrew/opt/openjdk@21/bin:$PATH"

java -version
go version
make --version
jq --version
cd testermint && ./gradlew --version && cd ..
```

Verify both container architectures:

```bash
docker run --rm --platform linux/arm64 alpine uname -m
docker run --rm --platform linux/amd64 alpine uname -m
```

Expected outputs are `aarch64` and `x86_64` respectively. Chain images on Apple Silicon build natively as arm64; the pinned CosmWasm optimizer runs as amd64 for reproducible fixtures.

## 5. Check Ports, Disk, and Existing State

The full stack binds ports including:

```text
80-82, 1317, 8089-8092, 8101-8102, 8201-8202,
9000-9005, 9010-9015, 9020-9025, 9090-9091,
15432, 26656-26657
```

Sanity checks:

```bash
df -h /
docker system df
lsof -nP -iTCP -sTCP:LISTEN | grep -E ':(80|81|82|1317|8089|8090|8091|8092|9000|9010|9020|9090|9091|15432|26656|26657)[[:space:]]' || true
docker compose ls
docker ps -a --format '{{.Names}}\t{{.Status}}'
```

### State Warning

The stateful Testermint test reboots the test network and recreates local chain state. `local-test-net/launch*.sh` also clears `local-test-net/prod-local`. Preserve any needed local data and transaction evidence before proceeding. Never execute this against a valuable or persistent network.

## 6. Reproducibly Build and Verify Wasm Fixture

```bash
cd inference-chain/contracts/p0-probe

./build.sh verify
make build CONTAINER_ENGINE=docker
make check CONTAINER_ENGINE=docker
shasum -a 256 artifacts/p0_probe.wasm
stat -f 'size_bytes=%z' artifacts/p0_probe.wasm
git diff -- artifacts/p0_probe.wasm artifacts/checksums.txt

cd ../../..
```

For commit `042758f4aa911606d34fc90fc1f3f257c09d55b8`, expected:

```text
size:   272814 bytes
sha256: 7eacebc656412fe59d290ecac1a0608686372c39ae6d63a8cb7fcba23417a91d
```

If the fixture is modified in a newer commit, `artifacts/checksums.txt` remains the source of truth. Following a clean rebuild, the diff of artifact and manifest must be empty.

## 7. Run Go Regression

```bash
cd inference-chain

go test ./app -run '^(TestAcceptedGrpcQueries|TestAcceptedStargateQueriesUnchanged|TestWasmGrpcForwardMarketplaceQueryAllowlist)$' -count=1
go test ./app -count=1
go vet ./app
go test -race ./app -run '^TestWasmGrpcForwardMarketplaceQueryAllowlist$' -count=1

cd ..
git diff --check
git diff --cached --check
```

On macOS, race builds may log a linker warning about malformed `LC_DYSYMTAB`. As long as the exit code is `0` and tests pass, this warning does not indicate failure.

## 8. Build Docker Images

### Canonical Build

For platforms where all targets build standardly:

```bash
make check-docker
make build-docker \
  GENESIS_OVERRIDES_FILE=inference-chain/test_genesis_overrides.json
```

The full target builds more images than needed for a focused smart-contract run, requiring significantly more time and disk space.

### Focused Apple Silicon Build

The stateful scenario requires chain, DAPI, mock-server, proxy, and edge-api. On Apple Silicon:

```bash
make node-build-docker \
  GENESIS_OVERRIDES_FILE=inference-chain/test_genesis_overrides.json \
  DOCKER_PLATFORM=linux/arm64 \
  DOCKER_GOOS=linux \
  DOCKER_GOARCH=arm64 \
  BLST_PORTABLE=1

make mock-server-build-docker DOCKER_PLATFORM=linux/arm64
make proxy-build-docker DOCKER_PLATFORM=linux/arm64
make edge-api-build-docker \
  DOCKER_PLATFORM=linux/arm64 \
  DOCKER_GOOS=linux \
  DOCKER_GOARCH=arm64 \
  BLST_PORTABLE=1
```

At the time of verification, `decentralized-api/Makefile` inside `build-docker` unconditionally overrode the platform to `linux/amd64`. Under amd64 emulation, the Go compiler crashed with `SIGSEGV` in Envoy assembler. Pending a Makefile fix, DAPI can be built directly with the same Dockerfile as arm64:

```bash
RUN_VERSION="$(git describe --always)"
RUN_COMMIT="$(git rev-parse HEAD)"
RUN_LDFLAGS="-X github.com/cosmos/cosmos-sdk/version.Name=decentralized-api -X github.com/cosmos/cosmos-sdk/version.AppName=decentralized-api -X github.com/cosmos/cosmos-sdk/version.Version=${RUN_VERSION} -X github.com/cosmos/cosmos-sdk/version.Commit=${RUN_COMMIT}"

docker build \
  --platform linux/arm64 \
  --build-arg LDFLAGS="$RUN_LDFLAGS" \
  --build-arg GOOS=linux \
  --build-arg GOARCH=arm64 \
  --build-arg BLST_PORTABLE=1 \
  --build-arg DEVSHARD_VERSION="$RUN_VERSION" \
  -f decentralized-api/Dockerfile . \
  -t "ghcr.io/product-science/api:${RUN_VERSION}"

docker tag \
  "ghcr.io/product-science/api:${RUN_VERSION}" \
  ghcr.io/product-science/api:latest
```

Verify architecture and save image IDs:

```bash
docker image inspect \
  ghcr.io/product-science/inferenced:latest \
  ghcr.io/product-science/api:latest \
  ghcr.io/product-science/edge-api:latest \
  ghcr.io/product-science/proxy:latest \
  inference-mock-server:latest \
  --format '{{index .RepoTags 0}} id={{.Id}} os={{.Os}} arch={{.Architecture}} size={{.Size}}'
```

All five images on Apple Silicon must have `arch=arm64`.

If the build terminates with a transient `unexpected EOF` error while downloading Go modules, retry the same command: successfully pulled layers are retained in BuildKit cache. If disk is full, first inspect `docker system df`; do not delete images or volumes containing needed state blindly.

## 9. Run Stateful Testermint Scenario

From the root of the repository:

```bash
make run-tests \
  TESTS='ClaimRecipientTests.claim rewards can be routed to configured recipient'
```

Equivalent direct invocation:

```bash
cd testermint
./gradlew :test \
  --tests 'ClaimRecipientTests.claim rewards can be routed to configured recipient' \
  -DexcludeTags=unstable,exclude
cd ..
```

Do not interrupt the process during PoC or epoch transitions. The scenario may take 10-20 minutes. In the verified baseline run, it completed in `8m 29s`.

In an adjacent terminal, observe node status:

```bash
docker ps --format '{{.Names}}\t{{.Image}}\t{{.Status}}' | sort
docker network inspect chain-public
docker exec genesis-node inferenced status 2>/dev/null | \
  jq '{height:.sync_info.latest_block_height,time:.sync_info.latest_block_time,catching_up:.sync_info.catching_up}'
```

Criteria:

- Gradle reports `BUILD SUCCESSFUL`;
- genesis, join1, join2, and `test-dns` containers are present;
- Three PostgreSQL containers are healthy;
- Block height is advancing;
- `catching_up=false`.

The scenario deliberately stops one participant's API prior to manual claim. Therefore, `join1-api` in state `Exited (137)` after a successful test is expected; smart contract queries execute against `genesis-node`.

## 10. Locate Real Performance Summary

Do not guess the current epoch. Query the native list first:

```bash
docker exec genesis-node inferenced query inference \
  list-epoch-performance-summary --output json | \
  jq '.epochPerformanceSummary'
```

Select an existing entry, preferably with `claimed=true`, and save the pair:

```bash
NATIVE_SUMMARIES="$(docker exec genesis-node inferenced query inference list-epoch-performance-summary --output json)"
EPOCH_INDEX="$(printf '%s' "$NATIVE_SUMMARIES" | jq -r '.epochPerformanceSummary[] | select(.claimed == true) | .epoch_index' | head -n 1)"
PARTICIPANT_ID="$(printf '%s' "$NATIVE_SUMMARIES" | jq -r '.epochPerformanceSummary[] | select(.claimed == true) | .participant_id' | head -n 1)"

test -n "$EPOCH_INDEX"
test -n "$PARTICIPANT_ID"

printf 'epoch=%s participant=%s\n' "$EPOCH_INDEX" "$PARTICIPANT_ID"
```

Verify targeted native query:

```bash
docker exec genesis-node inferenced query inference \
  show-epoch-performance-summary-by-participant \
  "$EPOCH_INDEX" "$PARTICIPANT_ID" --output json | jq .
```

Additionally verify claim-recipient and vesting state:

```bash
docker exec genesis-node inferenced query inference \
  list-claim-recipients "$PARTICIPANT_ID" --output json | jq .

docker exec genesis-node inferenced query streamvesting \
  total-vesting "$PARTICIPANT_ID" --output json | jq .
```

Claim-recipient overrides can be pruned after the target epoch has passed. Record positive evidence immediately following Testermint test completion. If the response is already empty, the route can still be proven via an empty successful response or by creating a new future override via a standard transaction.

## 11. Upload Current Wasm

Copy the artifact into the genesis container and verify hash:

```bash
docker cp \
  inference-chain/contracts/p0-probe/artifacts/p0_probe.wasm \
  genesis-node:/tmp/p0_probe.wasm

LOCAL_WASM_SHA="$(shasum -a 256 inference-chain/contracts/p0-probe/artifacts/p0_probe.wasm | awk '{print $1}')"
CONTAINER_WASM_SHA="$(docker exec genesis-node sha256sum /tmp/p0_probe.wasm | awk '{print $1}')"

printf 'local=%s\ncontainer=%s\n' "$LOCAL_WASM_SHA" "$CONTAINER_WASM_SHA"
test "$LOCAL_WASM_SHA" = "$CONTAINER_WASM_SHA"
```

Store transaction:

```bash
STORE_RESULT="$(docker exec genesis-node inferenced tx wasm store /tmp/p0_probe.wasm \
  --from genesis \
  --keyring-backend test \
  --gas auto \
  --gas-adjustment 1.3 \
  --broadcast-mode sync \
  --output json \
  --yes)"

STORE_TX_HASH="$(printf '%s' "$STORE_RESULT" | jq -r '.txhash')"
printf 'store_tx=%s\n' "$STORE_TX_HASH"
test -n "$STORE_TX_HASH"
```

Wait 2-5 seconds and query the tx. Do not save raw store tx JSON into public reports without filtering: it contains the full Wasm bytecode and adds excessive noise:

```bash
sleep 3
docker exec genesis-node inferenced query tx "$STORE_TX_HASH" --output json | \
  jq '{height,code,store_code_events:[.events[]? | select(.type == "store_code")]}'
```

Extract `code_id` and checksum:

```bash
STORE_TX="$(docker exec genesis-node inferenced query tx "$STORE_TX_HASH" --output json)"
STORE_CODE="$(printf '%s' "$STORE_TX" | jq -r '.code')"
CODE_ID="$(printf '%s' "$STORE_TX" | jq -r '.events[] | select(.type == "store_code") | .attributes[] | select(.key == "code_id") | .value')"
EVENT_WASM_SHA="$(printf '%s' "$STORE_TX" | jq -r '.events[] | select(.type == "store_code") | .attributes[] | select(.key == "code_checksum") | .value')"

test "$STORE_CODE" = "0"
test "$EVENT_WASM_SHA" = "$LOCAL_WASM_SHA"
printf 'code_id=%s checksum=%s\n' "$CODE_ID" "$EVENT_WASM_SHA"
```

If the transaction is not yet found, wait for the next block and retry query.

## 12. Instantiate Without Admin

```bash
INSTANTIATE_RESULT="$(docker exec genesis-node inferenced tx wasm instantiate "$CODE_ID" '{}' \
  --label p0-grpc-docker-live \
  --no-admin \
  --from genesis \
  --keyring-backend test \
  --gas auto \
  --gas-adjustment 1.3 \
  --broadcast-mode sync \
  --output json \
  --yes)"

INSTANTIATE_TX_HASH="$(printf '%s' "$INSTANTIATE_RESULT" | jq -r '.txhash')"
printf 'instantiate_tx=%s\n' "$INSTANTIATE_TX_HASH"
test -n "$INSTANTIATE_TX_HASH"
```

After transaction is included in a block:

```bash
sleep 3
INSTANTIATE_TX="$(docker exec genesis-node inferenced query tx "$INSTANTIATE_TX_HASH" --output json)"
INSTANTIATE_CODE="$(printf '%s' "$INSTANTIATE_TX" | jq -r '.code')"
CONTRACT_ADDRESS="$(printf '%s' "$INSTANTIATE_TX" | jq -r '.events[] | select(.type == "instantiate") | .attributes[] | select(.key == "_contract_address") | .value')"

test "$INSTANTIATE_CODE" = "0"
test -n "$CONTRACT_ADDRESS"
printf 'contract=%s\n' "$CONTRACT_ADDRESS"
```

Verify code ID, label, and absence of admin:

```bash
docker exec genesis-node inferenced query wasm contract \
  "$CONTRACT_ADDRESS" --output json | \
  jq '{address,contract_info:{code_id:.contract_info.code_id,creator:.contract_info.creator,admin:.contract_info.admin,label:.contract_info.label}}'
```

## 13. Execute Four Allowed Smart Queries

All four queries must execute against the single `CONTRACT_ADDRESS`.

### 13.1 GetCurrentEpoch

```bash
QUERY_CURRENT_EPOCH='{"get_current_epoch":{}}'

docker exec genesis-node inferenced query wasm contract-state smart \
  "$CONTRACT_ADDRESS" "$QUERY_CURRENT_EPOCH" --output json | jq .
```

Expected shape:

```json
{"data":{"epoch":8}}
```

The epoch number depends on the block height at time of query.

### 13.2 ListClaimRecipients

```bash
QUERY_CLAIM_RECIPIENTS="$(jq -nc \
  --arg participant "$PARTICIPANT_ID" \
  '{list_claim_recipients:{participant:$participant}}')"

docker exec genesis-node inferenced query wasm contract-state smart \
  "$CONTRACT_ADDRESS" "$QUERY_CLAIM_RECIPIENTS" --output json | jq .
```

A successful non-empty response has the form:

```json
{
  "data": {
    "entries": [
      {"epoch": 4, "recipient": "gonka1..."}
    ]
  }
}
```

`{"data":{"entries":[]}}` also verifies the route/codec path, but note in evidence that the schedule override had expired.

### 13.3 EpochPerformanceSummaryByParticipant

```bash
QUERY_PERFORMANCE="$(jq -nc \
  --argjson epoch_index "$EPOCH_INDEX" \
  --arg participant_id "$PARTICIPANT_ID" \
  '{epoch_performance_summary:{epoch_index:$epoch_index,participant_id:$participant_id}}')"

docker exec genesis-node inferenced query wasm contract-state smart \
  "$CONTRACT_ADDRESS" "$QUERY_PERFORMANCE" --output json | jq .
```

Expected shape for an existing record:

```json
{
  "data": {
    "epoch_index": 4,
    "participant_id": "gonka1...",
    "earned_coins": 0,
    "rewarded_coins": 94864721408887,
    "claimed": true
  }
}
```

Values and `claimed` status depend on actual chain state. Ensure the returned JSON matches the selected native summary.

### 13.4 TotalVestingAmount

```bash
QUERY_TOTAL_VESTING="$(jq -nc \
  --arg participant_address "$PARTICIPANT_ID" \
  '{total_vesting:{participant_address:$participant_address}}')"

docker exec genesis-node inferenced query wasm contract-state smart \
  "$CONTRACT_ADDRESS" "$QUERY_TOTAL_VESTING" --output json | jq .
```

For an account without a vesting schedule, expected:

```json
{"data":{"total_amount":[]}}
```

A non-empty schedule has the shape:

```json
{"data":{"total_amount":[{"denom":"ngonka","amount":"123"}]}}
```

Both verify the route and protobuf decoding successfully.

## 14. Verify Denied Boundary

The probe includes `raw_grpc` solely for adversarial testing of the allowlist:

```bash
QUERY_DENIED='{"raw_grpc":{"path":"/inference.inference.Query/EpochPerformanceSummaryAll","data":""}}'

set +e
DENIED_OUTPUT="$(docker exec genesis-node inferenced query wasm contract-state smart \
  "$CONTRACT_ADDRESS" "$QUERY_DENIED" --output json 2>&1)"
denied_exit_code=$?
set -e

printf '%s\n' "$DENIED_OUTPUT"
printf 'exit_code=%s\n' "$denied_exit_code"

test "$denied_exit_code" -ne 0
printf '%s\n' "$DENIED_OUTPUT" | \
  grep -F "'/inference.inference.Query/EpochPerformanceSummaryAll' path is not allowed from the contract"
```

Do not use variable name `status` for exit code in zsh: it is a reserved shell parameter.

Expected error message:

```text
Unsupported query type: '/inference.inference.Query/EpochPerformanceSummaryAll'
path is not allowed from the contract
```

If the broad route unexpectedly returns successful JSON, the check has failed: the allowlist is wider than intended.

## 15. Save Evidence

Minimum evidence to capture:

```bash
date '+local_time=%Y-%m-%dT%H:%M:%S%z'
git status --short --branch
git rev-parse HEAD
docker version --format 'client={{.Client.Version}} server={{.Server.Version}} os={{.Server.Os}} arch={{.Server.Arch}}'
docker context show
docker compose version
docker buildx version
docker image inspect ghcr.io/product-science/inferenced:latest --format 'id={{.Id}} os={{.Os}} arch={{.Architecture}} size={{.Size}}'
shasum -a 256 inference-chain/contracts/p0-probe/artifacts/p0_probe.wasm
docker ps --format '{{.Names}}\t{{.Image}}\t{{.Status}}' | sort
docker exec genesis-node inferenced status 2>/dev/null | jq '.sync_info'
```

The report should also include:

- stdout and exit codes of deterministic Rust/Go checks;
- Testermint test name, duration, and `BUILD SUCCESSFUL`;
- native performance-summary record;
- store tx hash, height, code ID, and checksum;
- instantiate tx hash, height, contract address, and proof of null admin;
- four exact payloads and four raw successful responses;
- denied payload, non-zero exit code, and raw error output;
- documentation of any infrastructure workaround.

Never include seed phrases, private keys, keyring contents, passwords, registry tokens, full environment dumps, or raw store transactions containing Wasm bytecode in evidence.

## 16. Post-Verification Steps

If the network is needed for further inspection, keep it running and record contract address and tx hashes. Before cleanup, review active compose projects and volumes:

```bash
docker compose ls
docker ps -a --format '{{.Names}}\t{{.Status}}'
docker volume ls
docker system df
```

Removing Testermint projects, containers, and volumes destroys local chain state. Perform cleanup only after preserving evidence, targeting only the specific genesis/join test projects.

After pushing commits, verify the remote GitHub Actions workflow:

```text
Reproduce P0 Wasm fixture
```

Local PASS without this CI job validates runtime behavior on the tested workstation, but does not replace canonical remote reproducibility evidence.

## Troubleshooting

### Docker CLI Still Shows Podman

Symptoms: server OS is `fedora`, socket path contains `podman`, `docker compose` or `docker buildx` are missing.

Fix: stop Podman, start Docker Desktop, switch context, and explicitly point to the Docker Desktop socket as shown in step 3.

### Testermint Cannot See Docker Although CLI Works

Ensure `DOCKER_HOST` is exported in the environment, not just prefixed before a single CLI command. The Gradle/Java process must inherit it.

### DAPI Crashes on amd64 Build on Apple Silicon

Use the focused native arm64 build from step 8. This is a known limitation of the DAPI Makefile under emulation, not a smart-contract allowlist issue.

### `query tx` Reports Transaction Not Found

Broadcast mode `sync` returns before inclusion in a block. Wait a few seconds and retry query. The final `code` must be `0`.

### Performance Query Returns `unknown request`

Confirm the exact `epoch_index + participant_id` pair via native list and participant-scoped query first. Do not use an unconfirmed or future epoch.

### Claim-Recipient Response Is Empty

Scheduled overrides can be pruned after the target epoch. Record evidence immediately after the Testermint test completes. A typed empty response still proves the transport path; for non-empty business state, submit a new future override.

### `TotalVestingAmount` Returns Empty Array

This is expected for accounts without a vesting schedule. The verification is successful as long as the request passes the allowlist and the contract decodes the native response.

### Running Out of Disk Space

Run `docker system df`. BuildKit cache can be cleared independently of images and volumes. Do not delete active state volumes without verification.

## Verified Reference Baseline

Local execution on 2026-09-08 on commit `042758f4aa911606d34fc90fc1f3f257c09d55b8`:

```text
Wasm SHA-256:
7eacebc656412fe59d290ecac1a0608686372c39ae6d63a8cb7fcba23417a91d

store tx:
44D6EE83C4DA648A7DB9C0211C3CBF63211B823DCF7F1B50CBD3E4AE9800CBF5

instantiate tx:
57FC11EF5DE0A774A6AAE1A1CBD23D8F6E3624EEBDB87BB8C67C50F6DB35419B

code_id: 1
contract:
gonka14hj2tavq8fpesdwxxcu44rty3hh90vhujrvcmstl4zr3txmfvw9sjhejf8

Testermint:
BUILD SUCCESSFUL in 8m 29s
```

All four allowed queries succeeded through this contract, and `EpochPerformanceSummaryAll` received the expected allowlist denial.
