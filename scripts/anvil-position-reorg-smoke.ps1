param(
    [int]$AnvilPort = 8547
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
$ownerKey = '0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80'
$aliceKey = '0x0000000000000000000000000000000000000000000000000000000000000001'
$bobKey = '0x0000000000000000000000000000000000000000000000000000000000000002'
$carolKey = '0x0000000000000000000000000000000000000000000000000000000000000003'
$metadataDigest = '0x5555555555555555555555555555555555555555555555555555555555555555'
$scratch = Join-Path ([System.IO.Path]::GetTempPath()) `
    ("foresyn-position-reorg-smoke-" + [guid]::NewGuid())
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
    if ($LASTEXITCODE -ne 0) { throw 'Failed to reset the disposable smoke database.' }

    Push-Location (Join-Path $PSScriptRoot '..\contracts')
    try {
        $deployOutput = & forge create `
            'src/ForesynPredictionMarket.sol:ForesynPredictionMarket' `
            --broadcast `
            --json `
            --rpc-url $rpcUrl `
            --private-key $ownerKey `
            --constructor-args $owner
        if ($LASTEXITCODE -ne 0) { throw 'Contract deployment failed.' }
        $contractAddress = ($deployOutput | Out-String | ConvertFrom-Json).deployedTo
    }
    finally {
        Pop-Location
    }

    $deploymentBlock = [uint64](& cast block-number --rpc-url $rpcUrl).Trim()
    $alice = (& cast wallet address --private-key $aliceKey).Trim()
    $bob = (& cast wallet address --private-key $bobKey).Trim()
    $carol = (& cast wallet address --private-key $carolKey).Trim()
    foreach ($participant in @($alice, $bob, $carol)) {
        & cast send $participant `
            --value 20ether `
            --rpc-url $rpcUrl `
            --private-key $ownerKey | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "Could not fund participant $participant." }
    }

    $deadline = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds() + 3600
    & cast send $contractAddress `
        'createMarket(uint64,address,bytes32)' `
        $deadline `
        $owner `
        $metadataDigest `
        --rpc-url $rpcUrl `
        --private-key $ownerKey | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'createMarket transaction failed.' }

    $snapshotId = (& cast rpc evm_snapshot --rpc-url $rpcUrl).Trim()
    if ($LASTEXITCODE -ne 0 -or -not $snapshotId) { throw 'Could not snapshot the market block.' }

    & cast send $contractAddress 'takePosition(uint256,uint8)' 1 1 `
        --value 2ether --rpc-url $rpcUrl --private-key $aliceKey | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Original Alice YES position failed.' }
    & cast send $contractAddress 'takePosition(uint256,uint8)' 1 2 `
        --value 3ether --rpc-url $rpcUrl --private-key $bobKey | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Original Bob NO position failed.' }
    & cast send $contractAddress 'takePosition(uint256,uint8)' 1 1 `
        --value 1ether --rpc-url $rpcUrl --private-key $aliceKey | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Original Alice repeated YES position failed.' }
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

        $oldState = (& psql $env:TEST_DATABASE_URL -Atc `
            "SELECT yes_pool::text || ',' || no_pool::text FROM market_states WHERE market_id = 1").Trim()
        $oldAlice = (& psql $env:TEST_DATABASE_URL -Atc `
            "SELECT yes_stake::text || ',' || no_stake::text FROM market_positions WHERE market_id = 1 AND encode(user_address, 'hex') = '$($alice.Substring(2).ToLower())'").Trim()
        $oldBob = (& psql $env:TEST_DATABASE_URL -Atc `
            "SELECT yes_stake::text || ',' || no_stake::text FROM market_positions WHERE market_id = 1 AND encode(user_address, 'hex') = '$($bob.Substring(2).ToLower())'").Trim()
        if ($oldState -ne '3000000000000000000,3000000000000000000' -or
            $oldAlice -ne '3000000000000000000,0' -or
            $oldBob -ne '0,3000000000000000000') {
            throw "Original mutable projection mismatch: state=$oldState alice=$oldAlice bob=$oldBob."
        }

        $reverted = (& cast rpc evm_revert $snapshotId --rpc-url $rpcUrl).Trim()
        if ($LASTEXITCODE -ne 0 -or $reverted -ne 'true') {
            throw "Anvil failed to revert snapshot $snapshotId."
        }

        & cast send $contractAddress 'takePosition(uint256,uint8)' 1 1 `
            --value 4ether --rpc-url $rpcUrl --private-key $aliceKey | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'Replacement Alice YES position failed.' }
        & cast send $contractAddress 'takePosition(uint256,uint8)' 1 2 `
            --value 5ether --rpc-url $rpcUrl --private-key $carolKey | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'Replacement Carol NO position failed.' }
        & cast rpc evm_mine --rpc-url $rpcUrl | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'Could not mine replacement-branch head.' }
        $replacementHead = [uint64](& cast block-number --rpc-url $rpcUrl).Trim()
        if ($replacementHead -ne $originalHead) {
            throw "Expected replacement head $originalHead, got $replacementHead."
        }

        & cargo run --locked -p foresyn-backend --bin indexer
        if ($LASTEXITCODE -ne 0) { throw 'Position reorg recovery run failed.' }

        $newState = (& psql $env:TEST_DATABASE_URL -Atc `
            "SELECT yes_pool::text || ',' || no_pool::text FROM market_states WHERE market_id = 1").Trim()
        $newAlice = (& psql $env:TEST_DATABASE_URL -Atc `
            "SELECT yes_stake::text || ',' || no_stake::text FROM market_positions WHERE market_id = 1 AND encode(user_address, 'hex') = '$($alice.Substring(2).ToLower())'").Trim()
        $newCarol = (& psql $env:TEST_DATABASE_URL -Atc `
            "SELECT yes_stake::text || ',' || no_stake::text FROM market_positions WHERE market_id = 1 AND encode(user_address, 'hex') = '$($carol.Substring(2).ToLower())'").Trim()
        $bobCount = (& psql $env:TEST_DATABASE_URL -Atc `
            "SELECT count(*) FROM market_positions WHERE market_id = 1 AND encode(user_address, 'hex') = '$($bob.Substring(2).ToLower())'").Trim()
        $rawCount = (& psql $env:TEST_DATABASE_URL -Atc `
            'SELECT count(*) FROM blockchain_events').Trim()
        if ($newState -ne '4000000000000000000,5000000000000000000' -or
            $newAlice -ne '4000000000000000000,0' -or
            $newCarol -ne '0,5000000000000000000' -or
            $bobCount -ne '0' -or $rawCount -ne '3') {
            throw "Replacement projection mismatch: state=$newState alice=$newAlice carol=$newCarol bob=$bobCount raw=$rawCount."
        }

        & cargo run --locked -p foresyn-backend --bin indexer
        if ($LASTEXITCODE -ne 0) { throw 'Post-recovery restart failed.' }
        $restartCounts = (& psql $env:TEST_DATABASE_URL -Atc `
            "SELECT (SELECT count(*) FROM blockchain_events)::text || ',' || (SELECT count(*) FROM market_positions)::text").Trim()
        if ($restartCounts -ne '3,2') {
            throw "Restart created duplicates: $restartCounts."
        }
    }
    finally {
        Pop-Location
    }

    Write-Output 'Anvil PositionTaken reorg smoke passed: old positions removed, replacement pools/positions rebuilt, restart duplicates=0.'
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
