param(
    [int]$AnvilPort = 8546
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
$oldDigestOne = '0x1111111111111111111111111111111111111111111111111111111111111111'
$oldDigestTwo = '0x2222222222222222222222222222222222222222222222222222222222222222'
$replacementDigest = '0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
$scratch = Join-Path ([System.IO.Path]::GetTempPath()) ("foresyn-reorg-smoke-" + [guid]::NewGuid())
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

    $schemaExists = (& psql $env:TEST_DATABASE_URL -Atc `
        "SELECT to_regclass('public.indexed_blocks') IS NOT NULL").Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Failed to inspect the disposable reorg database.' }
    if ($schemaExists -eq 't') {
        & psql $env:TEST_DATABASE_URL -v ON_ERROR_STOP=1 -c `
            'TRUNCATE indexer_contract_coverage, market_positions, market_states, markets, indexer_checkpoints, blockchain_events, indexed_blocks CASCADE' | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'Failed to reset the disposable reorg database.' }
    }

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

    $deploymentBlock = [uint64](& cast block-number --rpc-url $rpcUrl).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Could not read the deployment block.' }
    $snapshotId = (& cast rpc evm_snapshot --rpc-url $rpcUrl).Trim()
    if ($LASTEXITCODE -ne 0 -or -not $snapshotId) { throw 'Could not snapshot the deployment block.' }

    $deadline = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds() + 3600
    foreach ($digest in @($oldDigestOne, $oldDigestTwo)) {
        & cast send $contractAddress `
            'createMarket(uint64,address,bytes32)' `
            $deadline `
            $owner `
            $digest `
            --rpc-url $rpcUrl `
            --private-key $privateKey | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'Original-branch createMarket transaction failed.' }
    }
    $originalHead = [uint64](& cast block-number --rpc-url $rpcUrl).Trim()

    $env:DATABASE_URL = $env:TEST_DATABASE_URL
    $env:EVM_RPC_URL = $rpcUrl
    $env:EVM_CHAIN_ID = $chainId.ToString()
    $env:FORESYN_CONTRACT_ADDRESS = $contractAddress
    $env:FORESYN_DEPLOYMENT_BLOCK = $deploymentBlock.ToString()
    $env:INDEXER_CONFIRMATIONS = '0'
    $env:INDEXER_BATCH_SIZE = '50'

    Push-Location (Join-Path $PSScriptRoot '..')
    try {
        & cargo run --locked -p foresyn-backend --bin indexer
        if ($LASTEXITCODE -ne 0) { throw 'Original-branch indexer run failed.' }

        $originalRawCount = (& psql $env:TEST_DATABASE_URL -Atc `
            'SELECT count(*) FROM blockchain_events').Trim()
        $originalMarketCount = (& psql $env:TEST_DATABASE_URL -Atc `
            'SELECT count(*) FROM markets').Trim()
        if ($originalRawCount -ne '2' -or $originalMarketCount -ne '2') {
            throw "Expected two original events and markets, got raw=$originalRawCount markets=$originalMarketCount."
        }

        $reverted = (& cast rpc evm_revert $snapshotId --rpc-url $rpcUrl).Trim()
        if ($LASTEXITCODE -ne 0 -or $reverted -ne 'true') {
            throw "Anvil failed to revert snapshot $snapshotId."
        }

        & cast send $contractAddress `
            'createMarket(uint64,address,bytes32)' `
            $deadline `
            $owner `
            $replacementDigest `
            --rpc-url $rpcUrl `
            --private-key $privateKey | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'Replacement-branch createMarket transaction failed.' }
        & cast rpc evm_mine --rpc-url $rpcUrl | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'Could not mine replacement-branch head.' }

        $replacementHead = [uint64](& cast block-number --rpc-url $rpcUrl).Trim()
        if ($replacementHead -ne $originalHead) {
            throw "Expected replacement head $originalHead, got $replacementHead."
        }

        & cargo run --locked -p foresyn-backend --bin indexer
        if ($LASTEXITCODE -ne 0) { throw 'Reorg recovery indexer run failed.' }

        $rawCount = (& psql $env:TEST_DATABASE_URL -Atc `
            'SELECT count(*) FROM blockchain_events').Trim()
        $marketCount = (& psql $env:TEST_DATABASE_URL -Atc `
            'SELECT count(*) FROM markets').Trim()
        $marketTwoCount = (& psql $env:TEST_DATABASE_URL -Atc `
            'SELECT count(*) FROM markets WHERE market_id = 2').Trim()
        $storedDigest = (& psql $env:TEST_DATABASE_URL -Atc `
            'SELECT encode(metadata_digest, ''hex'') FROM markets WHERE market_id = 1').Trim()
        $checkpointBlock = (& psql $env:TEST_DATABASE_URL -Atc `
            'SELECT last_block_number FROM indexer_checkpoints').Trim()
        if ($rawCount -ne '1' -or $marketCount -ne '1' -or $marketTwoCount -ne '0') {
            throw "Orphan cleanup failed: raw=$rawCount markets=$marketCount market2=$marketTwoCount."
        }
        if ($storedDigest -ne $replacementDigest.Substring(2)) {
            throw "Replacement projection digest mismatch: $storedDigest."
        }
        if ($checkpointBlock -ne $replacementHead.ToString()) {
            throw "Checkpoint did not replay to replacement head ${replacementHead}: $checkpointBlock."
        }

        & cargo run --locked -p foresyn-backend --bin indexer
        if ($LASTEXITCODE -ne 0) { throw 'Post-recovery restart failed.' }
        $rawCountAfterRestart = (& psql $env:TEST_DATABASE_URL -Atc `
            'SELECT count(*) FROM blockchain_events').Trim()
        $marketCountAfterRestart = (& psql $env:TEST_DATABASE_URL -Atc `
            'SELECT count(*) FROM markets').Trim()
        if ($rawCountAfterRestart -ne '1' -or $marketCountAfterRestart -ne '1') {
            throw 'Post-recovery restart created duplicate events or markets.'
        }
    }
    finally {
        Pop-Location
    }

    Write-Output 'Anvil reorg smoke passed: ancestor preserved, orphaned branch removed, replacement replayed, restart duplicates=0.'
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
