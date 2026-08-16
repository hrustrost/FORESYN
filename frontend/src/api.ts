/** Human-readable market description held off-chain and committed on-chain by digest. */
export interface MarketMetadata {
  question: string
  description: string
  resolution_criteria: string
  category: string
  /** Omitted by the API when the market has no source link. */
  source_url?: string | null
}

export interface Market {
  market_id: string
  resolver: string
  creator: string
  deadline: string
  metadata_digest: string
  creation_block_number: string
  yes_pool: string
  no_pool: string
  total_pool: string
  /** Absent when no off-chain metadata is recorded for this market. */
  metadata?: MarketMetadata
  /** Whether `metadata` re-hashes to `metadata_digest`. Absent with `metadata`. */
  metadata_verified?: boolean
}

export interface Position {
  user_address: string
  yes_stake: string
  no_stake: string
  total_stake: string
  updated_block_number: string
}

const configuredApiUrl = import.meta.env.VITE_API_URL?.trim()
const apiUrl = (configuredApiUrl || 'http://localhost:8080').replace(/\/$/, '')

export class ApiUnavailableError extends Error {
  constructor() {
    super('The Foresyn read API is unavailable')
    this.name = 'ApiUnavailableError'
  }
}

async function getJson<T>(path: string, signal?: AbortSignal): Promise<T> {
  try {
    const response = await fetch(`${apiUrl}${path}`, {
      headers: { Accept: 'application/json' },
      signal,
    })

    if (!response.ok) {
      throw new ApiUnavailableError()
    }

    return (await response.json()) as T
  } catch (error) {
    if (error instanceof DOMException && error.name === 'AbortError') {
      throw error
    }
    throw new ApiUnavailableError()
  }
}

export function getMarkets(signal?: AbortSignal): Promise<Market[]> {
  return getJson<Market[]>('/api/markets', signal)
}

export function getMarket(
  marketId: string,
  signal?: AbortSignal,
): Promise<Market> {
  return getJson<Market>(`/api/markets/${encodeURIComponent(marketId)}`, signal)
}

export function getPositions(
  marketId: string,
  signal?: AbortSignal,
): Promise<Position[]> {
  return getJson<Position[]>(
    `/api/markets/${encodeURIComponent(marketId)}/positions`,
    signal,
  )
}
