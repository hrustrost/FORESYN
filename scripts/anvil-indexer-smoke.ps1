param(
    [int]$AnvilPort = 8545
)

$ErrorActionPreference = 'Stop'

if (-not $env:TEST_DATABASE_URL) {
    throw 'TEST_DATABASE_URL must point to a disposable PostgreSQL database.'
}

foreach ($command in @('anvil', 'cast', 'forge', 'cargo', 'psql')) {
    if (-not (Get-Command $command -ErrorAction SilentlyContinue)) {
        throw "Required command '$command' was not found on PATH."
    }
}

$chainId = 31337
$rpcUrl = "http://127.0.0.1:$AnvilPort"
$owner = '0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266'
$privateKey = '0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80'
$metadataDigest = '0x1111111111111111111111111111111111111111111111111111111111111111'
$scratch = Join-Path ([System.IO.Path]::GetTempPath()) ("foresyn-smoke-" + [guid]::NewGuid())
$null = New-Item -ItemType Directory -Path $scratch
$anvil = $null

try {
    $anvil = Start-Process anvil `
        -ArgumentList @('--silent', '--port', $AnvilPort, '--chain-id', $chainId) `
        -RedirectStandardOutput (Join-Path $scratch 'anvil.stdout.log') `
        -RedirectStandardError (Join-Path $scratch 'anvil.stderr.log') `
        -WindowStyle Hidden `
        -PassThru

    $ready = $false
    foreach ($attempt in 1..50) {
        & cast chain-id --rpc-url $rpcUrl 2>$null | Out-Null
        if ($LASTEXITCODE -eq 0) {
            $ready = $true
            break
        }
        Start-Sleep -Milliseconds 200
    }
    if (-not $ready) {
        throw 'Anvil did not become ready within 10 seconds.'
    }

    & psql $env:TEST_DATABASE_URL -v ON_ERROR_STOP=1 -c `
        'TRUNCATE indexer_contract_coverage, market_positions, market_states, markets, indexer_checkpoints, blockchain_events, indexed_blocks CASCADE' | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Failed to reset the disposable smoke-test database.' }

    Push-Location (Join-Path $PSScriptRoot '..\contracts')
    try {
        $deployOutput = & forge create `
            'src/ForesynPredictionMarket.sol:ForesynPredictionMarket' `
            --broadcast `
            --json `
            --rpc-url $rpcUrl `
            --private-key $privateKey `
            --constructor-args $owner
        if ($LASTEXITCODE -ne 0) { throw 'Contract deployment failed.' }
        $contractAddress = ($deployOutput | Out-String | ConvertFrom-Json).deployedTo
    }
    finally {
        Pop-Location
    }

    $deploymentBlock = (& cast block-number --rpc-url $rpcUrl).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Could not read the deployment block.' }
    $deadline = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds() + 3600

    & cast send $contractAddress `
        'createMarket(uint64,address,bytes32)' `
        $deadline `
        $owner `
        $metadataDigest `
        --rpc-url $rpcUrl `
        --private-key $privateKey | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'createMarket transaction failed.' }

    $env:DATABASE_URL = $env:TEST_DATABASE_URL
    $env:EVM_RPC_URL = $rpcUrl
    $env:EVM_CHAIN_ID = $chainId.ToString()
    $env:FORESYN_CONTRACT_ADDRESS = $contractAddress
    $env:FORESYN_DEPLOYMENT_BLOCK = $deploymentBlock
    $env:INDEXER_CONFIRMATIONS = '0'
    $env:INDEXER_BATCH_SIZE = '50'

    Push-Location (Join-Path $PSScriptRoot '..')
    try {
        & cargo run --locked -p foresyn-backend --bin indexer
        if ($LASTEXITCODE -ne 0) { throw 'First indexer run failed.' }

        $rawCount = (& psql $env:TEST_DATABASE_URL -Atc 'SELECT count(*) FROM blockchain_events').Trim()
        $marketCount = (& psql $env:TEST_DATABASE_URL -Atc 'SELECT count(*) FROM markets').Trim()
        if ($rawCount -ne '1' -or $marketCount -ne '1') {
            throw "Expected one raw event and one market, got raw=$rawCount market=$marketCount."
        }

        & cargo run --locked -p foresyn-backend --bin indexer
        if ($LASTEXITCODE -ne 0) { throw 'Restarted indexer run failed.' }

        $rawCountAfterRestart = (& psql $env:TEST_DATABASE_URL -Atc `
            'SELECT count(*) FROM blockchain_events').Trim()
        $marketCountAfterRestart = (& psql $env:TEST_DATABASE_URL -Atc `
            'SELECT count(*) FROM markets').Trim()
        if ($rawCountAfterRestart -ne '1' -or $marketCountAfterRestart -ne '1') {
            throw 'Restart created duplicate raw events or market projections.'
        }
    }
    finally {
        Pop-Location
    }

    Write-Output 'Anvil indexer smoke test passed: raw=1, markets=1, restart duplicates=0.'
}
finally {
    if ($anvil -and -not $anvil.HasExited) {
        Stop-Process -Id $anvil.Id -Force
        $anvil.WaitForExit()
    }
    if (Test-Path -LiteralPath $scratch) {
        Remove-Item -LiteralPath $scratch -Recurse -Force
    }
}
