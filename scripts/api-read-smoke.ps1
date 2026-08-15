param(
    [int]$AnvilPort = 8548,
    [int]$ApiPort = 8081
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
$apiUrl = "http://127.0.0.1:$ApiPort"
$owner = '0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266'
$ownerKey = '0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80'
$aliceKey = '0x0000000000000000000000000000000000000000000000000000000000000001'
$metadataDigest = '0x7777777777777777777777777777777777777777777777777777777777777777'
$scratch = Join-Path ([System.IO.Path]::GetTempPath()) `
    ("foresyn-api-smoke-" + [guid]::NewGuid())
$null = New-Item -ItemType Directory -Path $scratch
$anvil = $null
$api = $null

try {
    $anvil = Start-Process anvil `
        -ArgumentList @('--silent', '--port', $AnvilPort, '--chain-id', $chainId) `
        -RedirectStandardOutput (Join-Path $scratch 'anvil.stdout.log') `
        -RedirectStandardError (Join-Path $scratch 'anvil.stderr.log') `
        -WindowStyle Hidden `
        -PassThru

    $anvilReady = $false
    foreach ($attempt in 1..50) {
        & cast chain-id --rpc-url $rpcUrl 2>$null | Out-Null
        if ($LASTEXITCODE -eq 0) {
            $anvilReady = $true
            break
        }
        Start-Sleep -Milliseconds 200
    }
    if (-not $anvilReady) { throw 'Anvil did not become ready within 10 seconds.' }

    $schemaExists = (& psql $env:TEST_DATABASE_URL -Atc `
        "SELECT to_regclass('public.indexed_blocks') IS NOT NULL").Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Failed to inspect the disposable API database.' }
    if ($schemaExists -eq 't') {
        & psql $env:TEST_DATABASE_URL -v ON_ERROR_STOP=1 -c `
            'TRUNCATE indexer_contract_coverage, market_positions, market_states, markets, indexer_checkpoints, blockchain_events, indexed_blocks CASCADE' | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'Failed to reset the disposable API database.' }
    }

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
    $alice = (& cast wallet address --private-key $aliceKey).Trim().ToLower()
    & cast send $alice --value 20ether --rpc-url $rpcUrl --private-key $ownerKey | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Could not fund Alice.' }

    $deadline = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds() + 3600
    & cast send $contractAddress `
        'createMarket(uint64,address,bytes32)' `
        $deadline `
        $owner `
        $metadataDigest `
        --rpc-url $rpcUrl `
        --private-key $ownerKey | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'createMarket transaction failed.' }
    & cast send $contractAddress 'takePosition(uint256,uint8)' 1 1 `
        --value 2ether --rpc-url $rpcUrl --private-key $ownerKey | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Owner YES position failed.' }
    & cast send $contractAddress 'takePosition(uint256,uint8)' 1 2 `
        --value 5ether --rpc-url $rpcUrl --private-key $aliceKey | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Alice NO position failed.' }

    $env:DATABASE_URL = $env:TEST_DATABASE_URL
    $env:EVM_RPC_URL = $rpcUrl
    $env:EVM_CHAIN_ID = $chainId.ToString()
    $env:FORESYN_CONTRACT_ADDRESS = $contractAddress
    $env:FORESYN_DEPLOYMENT_BLOCK = $deploymentBlock.ToString()
    $env:INDEXER_CONFIRMATIONS = '0'
    $env:INDEXER_BATCH_SIZE = '50'
    $env:FORESYN_BIND_ADDRESS = "127.0.0.1:$ApiPort"
    $env:FORESYN_CORS_ORIGIN = 'http://localhost:5173'

    Push-Location (Join-Path $PSScriptRoot '..')
    try {
        & cargo run --locked -p foresyn-backend --bin indexer
        if ($LASTEXITCODE -ne 0) { throw 'Indexer run failed.' }
        & cargo build --locked -p foresyn-backend --bin foresyn-backend
        if ($LASTEXITCODE -ne 0) { throw 'API build failed.' }

        $apiExecutable = Join-Path (Get-Location) 'target\debug\foresyn-backend.exe'
        $api = Start-Process $apiExecutable `
            -RedirectStandardOutput (Join-Path $scratch 'api.stdout.log') `
            -RedirectStandardError (Join-Path $scratch 'api.stderr.log') `
            -WindowStyle Hidden `
            -PassThru

        $apiReady = $false
        foreach ($attempt in 1..50) {
            try {
                $health = Invoke-RestMethod -Uri "$apiUrl/health" -Method Get
                if ($health.status -eq 'ok') {
                    $apiReady = $true
                    break
                }
            }
            catch {
                Start-Sleep -Milliseconds 200
            }
        }
        if (-not $apiReady) { throw 'API did not become ready within 10 seconds.' }

        $marketsResponse = Invoke-WebRequest -UseBasicParsing -Uri "$apiUrl/api/markets" -Method Get
        $markets = ConvertFrom-Json -InputObject $marketsResponse.Content
        $market = Invoke-RestMethod -Uri "$apiUrl/api/markets/1" -Method Get
        $positionsResponse = Invoke-WebRequest -UseBasicParsing `
            -Uri "$apiUrl/api/markets/1/positions" `
            -Method Get
        $positions = ConvertFrom-Json -InputObject $positionsResponse.Content

        if ($markets.Count -ne 1 -or $markets[0].market_id -ne '1') {
            throw 'GET /api/markets returned unexpected markets.'
        }
        if ($market.yes_pool -ne '2000000000000000000' -or
            $market.no_pool -ne '5000000000000000000' -or
            $market.total_pool -ne '7000000000000000000' -or
            $market.metadata_digest -ne $metadataDigest) {
            throw 'GET /api/markets/1 returned incorrect exact projection values.'
        }
        if ($positions.Count -ne 2) {
            throw "Expected two API positions, got $($positions.Count)."
        }
        $ownerPosition = $positions | Where-Object { $_.user_address -eq $owner }
        $alicePosition = $positions | Where-Object { $_.user_address -eq $alice }
        if (-not $ownerPosition -or $ownerPosition.yes_stake -ne '2000000000000000000' -or
            $ownerPosition.no_stake -ne '0') {
            throw 'Owner position JSON is incorrect.'
        }
        if (-not $alicePosition -or $alicePosition.yes_stake -ne '0' -or
            $alicePosition.no_stake -ne '5000000000000000000') {
            throw 'Alice position JSON is incorrect.'
        }
    }
    finally {
        Pop-Location
    }

    Write-Output 'REST API smoke passed: indexer projections served exact market and position JSON.'
}
finally {
    if ($api -and -not $api.HasExited) {
        Stop-Process -Id $api.Id -Force
        $api.WaitForExit()
    }
    if ($anvil -and -not $anvil.HasExited) {
        Stop-Process -Id $anvil.Id -Force
        $anvil.WaitForExit()
    }
    if (Test-Path -LiteralPath $scratch) {
        Remove-Item -LiteralPath $scratch -Recurse -Force
    }
}
