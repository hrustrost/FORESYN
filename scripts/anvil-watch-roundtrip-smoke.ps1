param(
    [int]$AnvilPort = 8549,
    [int]$ApiPort = 8082,
    [int]$PollIntervalMilliseconds = 200
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
$metadataDigest = '0x8888888888888888888888888888888888888888888888888888888888888888'
$scratch = Join-Path ([System.IO.Path]::GetTempPath()) `
    ("foresyn-watch-smoke-" + [guid]::NewGuid())
$null = New-Item -ItemType Directory -Path $scratch
$anvil = $null
$indexer = $null
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
    if ($LASTEXITCODE -ne 0) { throw 'Failed to inspect the disposable watch database.' }
    if ($schemaExists -eq 't') {
        & psql $env:TEST_DATABASE_URL -v ON_ERROR_STOP=1 -c `
            'TRUNCATE indexer_contract_coverage, market_positions, market_states, markets, indexer_checkpoints, blockchain_events, indexed_blocks CASCADE' | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'Failed to reset the disposable watch database.' }
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
        & cargo build --locked -p foresyn-backend --bin indexer --bin foresyn-backend
        if ($LASTEXITCODE -ne 0) { throw 'Backend binaries failed to build.' }

        $indexerExecutable = Join-Path (Get-Location) 'target\debug\indexer.exe'
        $apiExecutable = Join-Path (Get-Location) 'target\debug\foresyn-backend.exe'
        $indexer = Start-Process $indexerExecutable `
            -ArgumentList @('--watch', '--poll-interval-ms', $PollIntervalMilliseconds) `
            -RedirectStandardOutput (Join-Path $scratch 'indexer.stdout.log') `
            -RedirectStandardError (Join-Path $scratch 'indexer.stderr.log') `
            -WindowStyle Hidden `
            -PassThru

        $watchReady = $false
        foreach ($attempt in 1..100) {
            if ($indexer.HasExited) {
                throw 'Watch indexer exited during startup.'
            }
            $coverage = (& psql $env:TEST_DATABASE_URL -Atc `
                "SELECT COUNT(*) FROM indexer_contract_coverage WHERE chain_id = $chainId" 2>$null)
            if ($LASTEXITCODE -eq 0 -and $coverage.Trim() -eq '1') {
                $watchReady = $true
                break
            }
            Start-Sleep -Milliseconds 200
        }
        if (-not $watchReady) { throw 'Watch indexer did not finish startup within 20 seconds.' }

        $watchProcessId = $indexer.Id
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
            }
            Start-Sleep -Milliseconds 200
        }
        if (-not $apiReady) { throw 'API did not become ready within 10 seconds.' }

        $deadline = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds() + 3600
        & cast send $contractAddress `
            'createMarket(uint64,address,bytes32)' `
            $deadline `
            $owner `
            $metadataDigest `
            --rpc-url $rpcUrl `
            --private-key $ownerKey | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'createMarket transaction failed.' }

        $marketObserved = $false
        foreach ($attempt in 1..100) {
            try {
                $market = Invoke-RestMethod -Uri "$apiUrl/api/markets/1" -Method Get
                if ($market.market_id -eq '1' -and $market.yes_pool -eq '0') {
                    $marketObserved = $true
                    break
                }
            }
            catch {
            }
            Start-Sleep -Milliseconds 200
        }
        if (-not $marketObserved) {
            throw 'Watch indexer did not expose the post-startup MarketCreated event.'
        }

        & cast send $contractAddress 'takePosition(uint256,uint8)' 1 1 `
            --value 2ether --rpc-url $rpcUrl --private-key $ownerKey | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'PositionTaken transaction failed.' }

        $positionObserved = $false
        foreach ($attempt in 1..100) {
            try {
                $market = Invoke-RestMethod -Uri "$apiUrl/api/markets/1" -Method Get
                $positionsResponse = Invoke-WebRequest -UseBasicParsing `
                    -Uri "$apiUrl/api/markets/1/positions" `
                    -Method Get
                $positions = @(ConvertFrom-Json -InputObject $positionsResponse.Content)
                $ownerPosition = $positions | Where-Object { $_.user_address -eq $owner }
                if ($market.yes_pool -eq '2000000000000000000' -and
                    $market.no_pool -eq '0' -and
                    $ownerPosition.yes_stake -eq '2000000000000000000' -and
                    $ownerPosition.no_stake -eq '0') {
                    $positionObserved = $true
                    break
                }
            }
            catch {
            }
            Start-Sleep -Milliseconds 200
        }
        if (-not $positionObserved) {
            throw 'Watch indexer did not expose the post-startup PositionTaken event.'
        }
        if ($indexer.HasExited -or $indexer.Id -ne $watchProcessId) {
            throw 'The continuous update required an unexpected indexer restart.'
        }
    }
    finally {
        Pop-Location
    }

    Write-Output 'Continuous round-trip smoke passed: one watch process indexed post-startup market and position transactions into the REST API.'
}
finally {
    if ($api -and -not $api.HasExited) {
        Stop-Process -Id $api.Id -Force
        $api.WaitForExit()
    }
    if ($indexer -and -not $indexer.HasExited) {
        Stop-Process -Id $indexer.Id -Force
        $indexer.WaitForExit()
    }
    if ($anvil -and -not $anvil.HasExited) {
        Stop-Process -Id $anvil.Id -Force
        $anvil.WaitForExit()
    }
    if (Test-Path -LiteralPath $scratch) {
        Remove-Item -LiteralPath $scratch -Recurse -Force
    }
}
