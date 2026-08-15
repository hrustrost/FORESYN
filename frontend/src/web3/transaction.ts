import type { Market, Position } from '../api'
import { NO_OUTCOME, type PositionOutcome, YES_OUTCOME } from './contract'

export type TransactionStage =
  | 'idle'
  | 'awaiting_wallet'
  | 'submitted'
  | 'confirming'
  | 'confirmed'
  | 'waiting_indexer'
  | 'indexed'
  | 'indexing_delayed'
  | 'failed'

export interface TransactionState {
  stage: TransactionStage
  marketId?: string
  hash?: string
  message?: string
}

export interface ProjectionBaseline {
  pool: bigint
  userStake: bigint
}

export function projectionBaseline(
  market: Market,
  positions: Position[],
  account: string,
  outcome: PositionOutcome,
): ProjectionBaseline {
  const position = positions.find(
    (candidate) => candidate.user_address.toLowerCase() === account.toLowerCase(),
  )
  return {
    pool: BigInt(outcome === YES_OUTCOME ? market.yes_pool : market.no_pool),
    userStake: BigInt(
      outcome === YES_OUTCOME ? (position?.yes_stake ?? '0') : (position?.no_stake ?? '0'),
    ),
  }
}

export function projectionIncludesPosition(
  market: Market,
  positions: Position[],
  account: string,
  outcome: PositionOutcome,
  amountWei: bigint,
  baseline: ProjectionBaseline,
): boolean {
  const position = positions.find(
    (candidate) => candidate.user_address.toLowerCase() === account.toLowerCase(),
  )
  if (!position) {
    return false
  }

  const pool = BigInt(outcome === YES_OUTCOME ? market.yes_pool : market.no_pool)
  const userStake = BigInt(
    outcome === NO_OUTCOME ? position.no_stake : position.yes_stake,
  )

  return pool >= baseline.pool + amountWei && userStake >= baseline.userStake + amountWei
}
