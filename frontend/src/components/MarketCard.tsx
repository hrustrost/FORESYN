import type { Market } from '../api'
import { formatDeadline, formatWei, shortenAddress } from '../format'

interface MarketCardProps {
  market: Market
  selected: boolean
  disabled?: boolean
  onSelect: (marketId: string) => void
}

export function MarketCard({ market, selected, disabled = false, onSelect }: MarketCardProps) {
  return (
    <button
      className={`market-card${selected ? ' market-card--selected' : ''}`}
      type="button"
      onClick={() => onSelect(market.market_id)}
      aria-pressed={selected}
      disabled={disabled}
    >
      <span className="market-card__topline">
        <span className="market-number">Market #{market.market_id}</span>
        <span className="block-number">Block {market.creation_block_number}</span>
      </span>

      <span className="market-card__resolver">
        Resolver
        <span title={market.resolver}>{shortenAddress(market.resolver)}</span>
      </span>

      <span className="market-card__deadline">
        Deadline
        <time dateTime={market.deadline} title={`Unix timestamp ${market.deadline}`}>
          {formatDeadline(market.deadline)}
        </time>
      </span>

      <span className="pool-row" aria-label="Market pools">
        <span className="pool-cell pool-cell--yes">
          <span>YES</span>
          <strong>{formatWei(market.yes_pool)}</strong>
        </span>
        <span className="pool-cell pool-cell--no">
          <span>NO</span>
          <strong>{formatWei(market.no_pool)}</strong>
        </span>
      </span>

      <span className="market-card__total">
        Total liquidity
        <strong>{formatWei(market.total_pool)}</strong>
      </span>
    </button>
  )
}
