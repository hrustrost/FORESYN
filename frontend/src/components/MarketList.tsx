import type { Market } from '../api'
import { MarketCard } from './MarketCard'

interface MarketListProps {
  markets: Market[]
  selectedMarketId: string | null
  onSelect: (marketId: string) => void
}

export function MarketList({
  markets,
  selectedMarketId,
  onSelect,
}: MarketListProps) {
  return (
    <section className="market-list" aria-labelledby="markets-heading">
      <div className="section-heading">
        <div>
          <p className="eyebrow">Canonical projections</p>
          <h2 id="markets-heading">Markets</h2>
        </div>
        <span className="count-badge">{markets.length}</span>
      </div>

      <div className="market-list__items">
        {markets.map((market) => (
          <MarketCard
            key={market.market_id}
            market={market}
            selected={market.market_id === selectedMarketId}
            onSelect={onSelect}
          />
        ))}
      </div>
    </section>
  )
}
