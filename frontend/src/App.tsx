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

type LoadState = 'loading' | 'ready' | 'error'

function App() {
  const [markets, setMarkets] = useState<Market[]>([])
  const [marketListState, setMarketListState] = useState<LoadState>('loading')
  const [selectedMarketId, setSelectedMarketId] = useState<string | null>(null)
  const [selectedMarket, setSelectedMarket] = useState<Market | null>(null)
  const [positions, setPositions] = useState<Position[]>([])
  const [detailState, setDetailState] = useState<LoadState>('ready')
  const [reloadKey, setReloadKey] = useState(0)
  const [detailReloadKey, setDetailReloadKey] = useState(0)

  const retryMarkets = useCallback(() => {
    setMarketListState('loading')
    setReloadKey((key) => key + 1)
  }, [])

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
        <div className="header-context">
          <span className="projection-dot" aria-hidden="true" />
          PostgreSQL projections
        </div>
      </header>

      <main className="dashboard">
        <section className="intro" aria-labelledby="page-title">
          <div>
            <p className="eyebrow">Verifiable read path</p>
            <h1 id="page-title">Market intelligence, reconstructed from events.</h1>
          </div>
          <p>
            Confirmed EVM events are decoded by the Rust indexer and served from
            disposable PostgreSQL projections. This interface never calls RPC or
            submits transactions.
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
              onSelect={setSelectedMarketId}
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
                <MarketDetail market={selectedMarket} positions={positions} />
              )}
            </section>
          </div>
        )}
      </main>

      <footer className="site-footer">
        <span>Read-only interview demo</span>
        <span>Blockchain remains the financial source of truth</span>
      </footer>
    </div>
  )
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
