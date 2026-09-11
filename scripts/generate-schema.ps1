$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $repoRoot
try {
    cargo run --locked --example factory-schema -p marketplace-factory
    if ($LASTEXITCODE -ne 0) {
        throw "Factory schema generation failed with exit code $LASTEXITCODE"
    }

    cargo run --locked --example deal-schema -p marketplace-deal
    if ($LASTEXITCODE -ne 0) {
        throw "Deal schema generation failed with exit code $LASTEXITCODE"
    }
}
finally {
    Pop-Location
}

Write-Host "Marketplace contract schemas regenerated."
