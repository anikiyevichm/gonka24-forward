$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repoRoot = Split-Path -Parent $PSScriptRoot
$protoRoot = Join-Path $repoRoot "packages/gonka-proto/proto/vendor"
$checksumFile = Join-Path $repoRoot "packages/gonka-proto/proto/checksums.sha256"
$generatorManifest = Join-Path $repoRoot "tools/proto-gen/Cargo.toml"

if (-not (Test-Path -LiteralPath $checksumFile -PathType Leaf)) {
    throw "Missing protobuf checksum manifest: $checksumFile"
}

$expectedEntries = Get-Content -LiteralPath $checksumFile | Where-Object {
    $_ -and -not $_.StartsWith("#")
}

foreach ($entry in $expectedEntries) {
    if ($entry -notmatch '^([0-9a-f]{64})  (.+)$') {
        throw "Invalid checksum line: $entry"
    }

    $expectedHash = $Matches[1]
    $relativePath = $Matches[2].Replace('/', [IO.Path]::DirectorySeparatorChar)
    $sourcePath = Join-Path $protoRoot $relativePath
    if (-not (Test-Path -LiteralPath $sourcePath -PathType Leaf)) {
        throw "Missing vendored protobuf source: $relativePath"
    }

    $actualHash = (Get-FileHash -LiteralPath $sourcePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualHash -ne $expectedHash) {
        throw "Checksum mismatch for $relativePath. Expected $expectedHash, got $actualHash"
    }
}

$actualFiles = @(Get-ChildItem -LiteralPath $protoRoot -Recurse -File -Filter "*.proto")
if ($actualFiles.Count -ne $expectedEntries.Count) {
    throw "Vendored protobuf file count differs from checksum manifest: expected $($expectedEntries.Count), got $($actualFiles.Count)"
}

& cargo +1.81.0 run --locked --quiet --manifest-path $generatorManifest
if ($LASTEXITCODE -ne 0) {
    throw "Gonka protobuf generation failed with exit code $LASTEXITCODE"
}

& cargo +1.81.0 fmt --manifest-path $generatorManifest -- --check
if ($LASTEXITCODE -ne 0) {
    throw "Host generator formatting check failed with exit code $LASTEXITCODE"
}

Write-Host "Gonka protobuf bindings are regenerated from the verified snapshot."
