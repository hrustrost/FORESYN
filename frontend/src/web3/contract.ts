import {
  BrowserProvider,
  Contract,
  parseEther,
  type ContractTransactionResponse,
  type Eip1193Provider,
} from 'ethers'

import type { Web3Config } from './config'
import { normalizeChainId, type InjectedProvider } from './wallet'

export const YES_OUTCOME = 1 as const
export const NO_OUTCOME = 2 as const
export type PositionOutcome = typeof YES_OUTCOME | typeof NO_OUTCOME

const TAKE_POSITION_ABI = [
  'function takePosition(uint256 marketId, uint8 outcome) payable',
] as const

export class PositionInputError extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'PositionInputError'
  }
}

export class WrongWalletNetworkError extends Error {
  constructor() {
    super('Wallet is connected to the wrong network')
    this.name = 'WrongWalletNetworkError'
  }
}

export function parsePositiveEthAmount(value: string): bigint {
  const normalized = value.trim()
  if (!/^(?:\d+|\d*\.\d{1,18})$/.test(normalized)) {
    throw new PositionInputError('Enter a valid ETH amount with up to 18 decimals')
  }

  let wei: bigint
  try {
    wei = parseEther(normalized)
  } catch {
    throw new PositionInputError('Enter a valid ETH amount with up to 18 decimals')
  }
  if (wei <= 0n) {
    throw new PositionInputError('Position amount must be greater than zero')
  }
  return wei
}

export async function submitPosition(
  provider: InjectedProvider,
  config: Web3Config,
  marketId: string,
  outcome: PositionOutcome,
  amountWei: bigint,
): Promise<ContractTransactionResponse> {
  if (!/^\d+$/.test(marketId) || BigInt(marketId) <= 0n) {
    throw new PositionInputError('The selected market is invalid')
  }
  if (amountWei <= 0n) {
    throw new PositionInputError('Position amount must be greater than zero')
  }

  const actualChainId = normalizeChainId(
    await provider.request({ method: 'eth_chainId' }),
  )
  if (actualChainId !== config.chainId) {
    throw new WrongWalletNetworkError()
  }

  const browserProvider = new BrowserProvider(provider as Eip1193Provider)
  const signer = await browserProvider.getSigner()
  const contract = new Contract(config.contractAddress, TAKE_POSITION_ABI, signer)
  return (await contract.takePosition(marketId, outcome, {
    value: amountWei,
  })) as ContractTransactionResponse
}
