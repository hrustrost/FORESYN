import { useCallback, useEffect, useState } from 'react'

import {
  getMarket,
  getMarkets,
  getPositions,
  type Market,
  type Position,
} from './api'
import { MarketDetail } from './components/MarketDetail'
import { MarketList } from './components/MarketList'
import { TakePositionPanel } from './components/TakePositionPanel'
import { WalletButton } from './components/WalletButton'
import {
  parsePositiveEthAmount,
  PositionInputError,
  submitPosition,
  type PositionOutcome,
  WrongWalletNetworkError,
} from './web3/contract'
import { loadWeb3Config, type Web3Config } from './web3/config'
import {
  projectionBaseline,
  projectionIncludesPosition,
  type TransactionState,
} from './web3/transaction'
import { useWallet } from './web3/useWallet'
import { isUserRejected } from './web3/wallet'

type LoadState = 'loading' | 'ready' | 'error'
const IDLE_TRANSACTION: TransactionState = { stage: 'idle' }
const INDEXER_POLL_INTERVAL_MS = 2_000
const INDEXER_POLL_ATTEMPTS = 30

function App() {
  const [web3Configuration] = useState<{
    config: Web3Config | null
    error: string | null
  }>(() => {
    try {
      return { config: loadWeb3Config(), error: null }
    } catch (error) {
      return {
        config: null,
        error:
          error instanceof Error
            ? error.message
            : 'Frontend Web3 configuration is invalid.',
      }
    }
  })
  const wallet = useWallet(web3Configuration.config)
  const [markets, setMarkets] = useState<Market[]>([])
  const [marketListState, setMarketListState] = useState<LoadState>('loading')
  const [selectedMarketId, setSelectedMarketId] = useState<string | null>(null)
  const [selectedMarket, setSelectedMarket] = useState<Market | null>(null)
  const [positions, setPositions] = useState<Position[]>([])
  const [detailState, setDetailState] = useState<LoadState>('ready')
  const [reloadKey, setReloadKey] = useState(0)
  const [detailReloadKey, setDetailReloadKey] = useState(0)
  const [transaction, setTransaction] =
    useState<TransactionState>(IDLE_TRANSACTION)
  const transactionInProgress = [
    'awaiting_wallet',
    'submitted',
    'confirming',
    'confirmed',
    'waiting_indexer',
  ].includes(transaction.stage)

  const retryMarkets = useCallback(() => {
    setMarketListState('loading')
    setReloadKey((key) => key + 1)
  }, [])

  const takePosition = useCallback(
    async (outcome: PositionOutcome, amount: string) => {
      const market = selectedMarket
      const account = wallet.account
      const provider = wallet.provider
      const config = web3Configuration.config
      if (!market || !markets.some((candidate) => candidate.market_id === market.market_id)) {
        setTransaction({
          stage: 'failed',
          message: 'Select a valid indexed market before submitting a position.',
        })
        return
      }
      if (!account || !provider || wallet.phase !== 'ready') {
        setTransaction({
          stage: 'failed',
          marketId: market.market_id,
          message: 'Connect a wallet on the configured network first.',
        })
        return
      }
      if (!config) {
        setTransaction({
          stage: 'failed',
          marketId: market.market_id,
          message: web3Configuration.error ?? 'Frontend Web3 configuration is invalid.',
        })
        return
      }

      let amountWei: bigint
      try {
        amountWei = parsePositiveEthAmount(amount)
      } catch (error) {
        setTransaction({
          stage: 'failed',
          marketId: market.market_id,
          message:
            error instanceof PositionInputError
              ? error.message
              : 'Enter a valid positive ETH amount.',
        })
        return
      }

      const baseline = projectionBaseline(market, positions, account, outcome)
      setTransaction({ stage: 'awaiting_wallet', marketId: market.market_id })

      try {
        const response = await submitPosition(
          provider,
          config,
          market.market_id,
          outcome,
          amountWei,
        )
        setTransaction({
          stage: 'submitted',
          marketId: market.market_id,
          hash: response.hash,
        })

        await delay(0)
        setTransaction({
          stage: 'confirming',
          marketId: market.market_id,
          hash: response.hash,
        })
        const receipt = await response.wait()
        if (!receipt || receipt.status !== 1) {
          throw new Error('Transaction was not successful')
        }

        setTransaction({
          stage: 'confirmed',
          marketId: market.market_id,
          hash: response.hash,
        })
        await delay(0)
        setTransaction({
          stage: 'waiting_indexer',
          marketId: market.market_id,
          hash: response.hash,
          message: 'Confirmed by the chain. Waiting for the REST projection to catch up.',
        })

        const indexed = await waitForIndexedProjection(
          market.market_id,
          account,
          outcome,
          amountWei,
          baseline,
        )
        if (!indexed) {
          setTransaction({
            stage: 'indexing_delayed',
            marketId: market.market_id,
            hash: response.hash,
            message:
              'The transaction is confirmed, but the indexed projection was not observed yet.',
          })
          return
        }

        setSelectedMarket(indexed.market)
        setPositions(indexed.positions)
        setMarkets((current) =>
          current.map((candidate) =>
            candidate.market_id === indexed.market.market_id ? indexed.market : candidate,
          ),
        )
        setTransaction({
          stage: 'indexed',
          marketId: market.market_id,
          hash: response.hash,
          message: 'The Axum API now exposes the updated pools and position.',
        })
      } catch (error) {
        const message = isUserRejected(error)
          ? 'The transaction was rejected in the wallet.'
          : error instanceof WrongWalletNetworkError
            ? 'Switch to the configured network before submitting.'
            : error instanceof PositionInputError
              ? error.message
              : 'The transaction failed or was reverted.'
        setTransaction((current) => ({
          stage: 'failed',
          marketId: market.market_id,
          hash: current.hash,
          message,
        }))
      }
    },
    [
      markets,
      positions,
      selectedMarket,
      wallet.account,
      wallet.phase,
      wallet.provider,
      web3Configuration.config,
      web3Configuration.error,
    ],
  )

  useEffect(() => {
    const controller = new AbortController()

    getMarkets(controller.signal)
      .then((nextMarkets) => {
        setMarkets(nextMarkets)
        setMarketListState('ready')
        setSelectedMarketId((current) => {
          if (current && nextMarkets.some((market) => market.market_id === current)) {
            return current
          }
          return nextMarkets[0]?.market_id ?? null
        })
      })
      .catch((error: unknown) => {
        if (error instanceof DOMException && error.name === 'AbortError') {
          return
        }
        setMarketListState('error')
      })

    return () => controller.abort()
  }, [reloadKey])

  useEffect(() => {
    if (!selectedMarketId) {
      setSelectedMarket(null)
      setPositions([])
      setDetailState('ready')
      return
    }

    const controller = new AbortController()
    setDetailState('loading')
    setSelectedMarket(null)
    setPositions([])

    Promise.all([
      getMarket(selectedMarketId, controller.signal),
      getPositions(selectedMarketId, controller.signal),
    ])
      .then(([market, nextPositions]) => {
        setSelectedMarket(market)
        setPositions(nextPositions)
        setDetailState('ready')
      })
      .catch((error: unknown) => {
        if (error instanceof DOMException && error.name === 'AbortError') {
          return
        }
        setDetailState('error')
      })

    return () => controller.abort()
  }, [selectedMarketId, detailReloadKey])

  return (
    <div className="app-shell">
      <header className="site-header">
        <a className="brand" href="/" aria-label="Foresyn home">
          <span className="brand__mark" aria-hidden="true">
            F
          </span>
          <span>
            <strong>Foresyn</strong>
            <small>Prediction market explorer</small>
          </span>
        </a>
        <div className="header-actions">
          <div className="header-context">
            <span className="projection-dot" aria-hidden="true" />
            PostgreSQL projections
          </div>
          <WalletButton wallet={wallet} config={web3Configuration.config} />
        </div>
      </header>

      <main className="dashboard">
        <section className="intro" aria-labelledby="page-title">
          <div>
            <p className="eyebrow">Verifiable read path</p>
            <h1 id="page-title">Market intelligence, reconstructed from events.</h1>
          </div>
          <p>
            Market reads come from confirmed EVM events decoded by the Rust indexer
            and served through Axum/PostgreSQL. Position writes are explicitly
            user-signed in the injected wallet and sent to the EVM.
          </p>
        </section>

        <div className="pipeline" aria-label="Foresyn read architecture">
          <span>EVM events</span>
          <i aria-hidden="true">→</i>
          <span>Alloy indexer</span>
          <i aria-hidden="true">→</i>
          <span>PostgreSQL</span>
          <i aria-hidden="true">→</i>
          <span>Axum API</span>
          <i aria-hidden="true">→</i>
          <strong>React UI</strong>
        </div>

        {marketListState === 'loading' && <DashboardLoading />}

        {marketListState === 'error' && (
          <ErrorState
            title="The market feed is unavailable"
            message="Check that the Foresyn backend is running, then try again."
            onRetry={retryMarkets}
          />
        )}

        {marketListState === 'ready' && markets.length === 0 && <EmptyMarkets />}

        {marketListState === 'ready' && markets.length > 0 && (
          <div className="workspace">
            <MarketList
              markets={markets}
              selectedMarketId={selectedMarketId}
              disabled={transactionInProgress}
              onSelect={(marketId) => {
                setSelectedMarketId(marketId)
                setTransaction(IDLE_TRANSACTION)
              }}
            />

            <section className="detail-panel" aria-live="polite">
              {detailState === 'loading' && <DetailLoading />}
              {detailState === 'error' && (
                <ErrorState
                  title="This market could not be loaded"
                  message="Its detail projection is temporarily unavailable. Select another market or retry the feed."
                  onRetry={() => setDetailReloadKey((key) => key + 1)}
                  compact
                />
              )}
              {detailState === 'ready' && selectedMarket && (
                <MarketDetail
                  market={selectedMarket}
                  positions={positions}
                  actions={
                    <TakePositionPanel
                      marketId={selectedMarket.market_id}
                      wallet={wallet}
                      config={web3Configuration.config}
                      configError={web3Configuration.error}
                      transaction={
                        transaction.marketId === selectedMarket.market_id
                          ? transaction
                          : IDLE_TRANSACTION
                      }
                      onSubmit={takePosition}
                    />
                  }
                />
              )}
            </section>
          </div>
        )}
      </main>

      <footer className="site-footer">
        <span>Indexed reads · wallet-signed writes</span>
        <span>Blockchain remains the financial source of truth</span>
      </footer>
    </div>
  )
}

async function waitForIndexedProjection(
  marketId: string,
  account: string,
  outcome: PositionOutcome,
  amountWei: bigint,
  baseline: ReturnType<typeof projectionBaseline>,
): Promise<{ market: Market; positions: Position[] } | null> {
  for (let attempt = 0; attempt < INDEXER_POLL_ATTEMPTS; attempt += 1) {
    try {
      const [market, positions] = await Promise.all([
        getMarket(marketId),
        getPositions(marketId),
      ])
      if (
        projectionIncludesPosition(
          market,
          positions,
          account,
          outcome,
          amountWei,
          baseline,
        )
      ) {
        return { market, positions }
      }
    } catch {
      // A transient read failure remains distinct from the confirmed transaction.
    }
    await delay(INDEXER_POLL_INTERVAL_MS)
  }
  return null
}

function delay(milliseconds: number): Promise<void> {
  return new Promise((resolve) => window.setTimeout(resolve, milliseconds))
}

function DashboardLoading() {
  return (
    <div className="workspace" aria-label="Loading markets" aria-busy="true">
      <div className="loading-list">
        <div className="skeleton skeleton--heading" />
        {[0, 1, 2].map((item) => (
          <div className="skeleton skeleton--card" key={item} />
        ))}
      </div>
      <div className="skeleton skeleton--detail" />
    </div>
  )
}

function DetailLoading() {
  return (
    <div className="detail-loading" aria-label="Loading selected market" aria-busy="true">
      <div className="skeleton skeleton--heading" />
      <div className="skeleton skeleton--metrics" />
      <div className="skeleton skeleton--table" />
    </div>
  )
}

function EmptyMarkets() {
  return (
    <section className="state-card">
      <span className="state-card__icon" aria-hidden="true">
        0
      </span>
      <h2>No indexed markets yet</h2>
      <p>
        Once a MarketCreated event is confirmed and indexed, its projection will
        appear here.
      </p>
    </section>
  )
}

interface ErrorStateProps {
  title: string
  message: string
  onRetry: () => void
  compact?: boolean
}

function ErrorState({ title, message, onRetry, compact = false }: ErrorStateProps) {
  return (
    <section className={`state-card state-card--error${compact ? ' state-card--compact' : ''}`}>
      <span className="state-card__icon" aria-hidden="true">
        !
      </span>
      <h2>{title}</h2>
      <p>{message}</p>
      <button type="button" onClick={onRetry}>
        Try again
      </button>
    </section>
  )
}

export default App
