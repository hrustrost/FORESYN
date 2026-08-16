import type { ReactNode } from 'react'

import type { Market, Position } from '../api'
import { formatDeadline, formatWei, shortenAddress } from '../format'
import { PositionsList } from './PositionsList'

interface MarketDetailProps {
  market: Market
  positions: Position[]
  actions?: ReactNode
}

export function MarketDetail({ market, positions, actions }: MarketDetailProps) {
  const hasVerifiedMetadata = market.metadata && market.metadata_verified

  return (
    <article className="market-detail" aria-labelledby="detail-heading">
      <header className="market-detail__header">
        <div>
          {hasVerifiedMetadata && market.metadata ? (
            <>
              <h2 id="detail-heading" className="market-detail__question">
                {market.metadata.question}
              </h2>
              <p className="eyebrow">Market #{market.market_id}</p>
            </>
          ) : (
            <>
              <p className="eyebrow">On-chain market</p>
              <h2 id="detail-heading">Market #{market.market_id}</h2>
              {!market.metadata && (
                <p className="market-detail__unavailable">Metadata unavailable</p>
              )}
            </>
          )}
        </div>
        <div className="status-badges">
          {hasVerifiedMetadata && (
            <div className="verified-badge">VERIFIED METADATA</div>
          )}
          <div className="readonly-pill">Indexed view</div>
        </div>
      </header>

      {hasVerifiedMetadata && market.metadata && (
        <section className="market-detail__metadata">
          <p className="metadata-description">
            {market.metadata.description}
          </p>
          <div className="resolution-criteria">
            <dt>Resolution criteria</dt>
            <dd>{market.metadata.resolution_criteria}</dd>
          </div>
          <div className="metadata-category">
            <span className="category-badge">{market.metadata.category}</span>
          </div>
        </section>
      )}

      <dl className="market-metadata">
        <div>
          <dt>Resolver</dt>
          <dd title={market.resolver}>{shortenAddress(market.resolver)}</dd>
        </div>
        <div>
          <dt>Creator</dt>
          <dd title={market.creator}>{shortenAddress(market.creator)}</dd>
        </div>
        <div>
          <dt>Deadline</dt>
          <dd title={`Unix timestamp ${market.deadline}`}>
            {formatDeadline(market.deadline)}
          </dd>
        </div>
        <div>
          <dt>Creation block</dt>
          <dd>{market.creation_block_number}</dd>
        </div>
      </dl>

      <section className="detail-pools" aria-label="Pool balances">
        <div className="detail-pool detail-pool--yes">
          <span>YES pool</span>
          <strong>{formatWei(market.yes_pool)}</strong>
        </div>
        <div className="detail-pool detail-pool--no">
          <span>NO pool</span>
          <strong>{formatWei(market.no_pool)}</strong>
        </div>
        <div className="detail-pool detail-pool--total">
          <span>Total pool</span>
          <strong>{formatWei(market.total_pool)}</strong>
        </div>
      </section>

      <div className="digest-row">
        <span>Metadata digest</span>
        <code title={market.metadata_digest}>{market.metadata_digest}</code>
      </div>

      {actions}

      <section className="positions" aria-labelledby="positions-heading">
        <div className="positions__heading">
          <div>
            <p className="eyebrow">Indexed accounts</p>
            <h3 id="positions-heading">Positions</h3>
          </div>
          <span className="count-badge">{positions.length}</span>
        </div>
        <PositionsList positions={positions} />
      </section>
    </article>
  )
}
