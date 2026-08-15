import { getAddress, isAddress } from 'ethers'

import type { Web3Config } from './config'

type ProviderListener = (...arguments_: unknown[]) => void

export interface InjectedProvider {
  request(arguments_: {
    method: string
    params?: readonly unknown[] | Record<string, unknown>
  }): Promise<unknown>
  on(event: string, listener: ProviderListener): void
  removeListener(event: string, listener: ProviderListener): void
}

declare global {
  interface Window {
    ethereum?: InjectedProvider
  }
}

export interface WalletSnapshot {
  account: string | null
  chainId: bigint
}

export interface WalletEventHandlers {
  accountsChanged(accounts: string[]): void
  chainChanged(chainId: bigint): void
}

export function getInjectedProvider(): InjectedProvider | null {
  return window.ethereum ?? null
}

export function normalizeChainId(value: unknown): bigint {
  if (typeof value !== 'string' || !/^(?:0x[\da-f]+|\d+)$/i.test(value)) {
    throw new Error('Wallet returned an invalid chain ID')
  }
  return BigInt(value)
}

export function normalizeAccounts(value: unknown): string[] {
  if (!Array.isArray(value)) {
    throw new Error('Wallet returned an invalid account list')
  }
  return value
    .filter((account): account is string => typeof account === 'string' && isAddress(account))
    .map((account) => getAddress(account))
}

export async function readWallet(provider: InjectedProvider): Promise<WalletSnapshot> {
  const [accounts, chainId] = await Promise.all([
    provider.request({ method: 'eth_accounts' }),
    provider.request({ method: 'eth_chainId' }),
  ])
  return {
    account: normalizeAccounts(accounts)[0] ?? null,
    chainId: normalizeChainId(chainId),
  }
}

export async function requestWalletConnection(
  provider: InjectedProvider,
): Promise<WalletSnapshot> {
  const accounts = normalizeAccounts(
    await provider.request({ method: 'eth_requestAccounts' }),
  )
  const chainId = normalizeChainId(await provider.request({ method: 'eth_chainId' }))
  return { account: accounts[0] ?? null, chainId }
}

export async function switchWalletNetwork(
  provider: InjectedProvider,
  config: Web3Config,
): Promise<void> {
  try {
    await provider.request({
      method: 'wallet_switchEthereumChain',
      params: [{ chainId: config.chainIdHex }],
    })
  } catch (error) {
    if (walletErrorCode(error) !== 4902 || !config.walletRpcUrl) {
      throw error
    }

    await provider.request({
      method: 'wallet_addEthereumChain',
      params: [
        {
          chainId: config.chainIdHex,
          chainName: config.chainName,
          nativeCurrency: { name: 'Ether', symbol: 'ETH', decimals: 18 },
          rpcUrls: [config.walletRpcUrl],
        },
      ],
    })
  }
}

export function subscribeToWallet(
  provider: InjectedProvider,
  handlers: WalletEventHandlers,
): () => void {
  const accountsListener: ProviderListener = (accounts) => {
    try {
      handlers.accountsChanged(normalizeAccounts(accounts))
    } catch {
      handlers.accountsChanged([])
    }
  }
  const chainListener: ProviderListener = (chainId) => {
    try {
      handlers.chainChanged(normalizeChainId(chainId))
    } catch {
      // An invalid provider notification cannot authorize a transaction.
      handlers.chainChanged(-1n)
    }
  }

  provider.on('accountsChanged', accountsListener)
  provider.on('chainChanged', chainListener)

  return () => {
    provider.removeListener('accountsChanged', accountsListener)
    provider.removeListener('chainChanged', chainListener)
  }
}

export function isUserRejected(error: unknown): boolean {
  const code = walletErrorCode(error)
  return code === 4001 || code === 'ACTION_REJECTED'
}

function walletErrorCode(error: unknown): unknown {
  if (!error || typeof error !== 'object') {
    return undefined
  }
  if ('code' in error) {
    return error.code
  }
  if ('info' in error && error.info && typeof error.info === 'object') {
    const info = error.info
    if ('error' in info && info.error && typeof info.error === 'object' && 'code' in info.error) {
      return info.error.code
    }
  }
  return undefined
}
