import { getAddress, ZeroAddress } from 'ethers'

export interface Web3Config {
  chainId: bigint
  chainIdHex: string
  chainName: string
  contractAddress: string
  walletRpcUrl?: string
}

export class Web3ConfigError extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'Web3ConfigError'
  }
}

export function loadWeb3Config(
  environment: Record<string, string | undefined> = import.meta.env,
): Web3Config {
  const chainIdValue = environment.VITE_CHAIN_ID?.trim() || '31337'
  let chainId: bigint
  try {
    chainId = BigInt(chainIdValue)
  } catch {
    throw new Web3ConfigError('VITE_CHAIN_ID must be a positive integer')
  }
  if (chainId <= 0n) {
    throw new Web3ConfigError('VITE_CHAIN_ID must be a positive integer')
  }

  const configuredAddress = environment.VITE_CONTRACT_ADDRESS?.trim()
  let contractAddress: string
  try {
    contractAddress = getAddress(configuredAddress ?? '')
  } catch {
    throw new Web3ConfigError('VITE_CONTRACT_ADDRESS must be a valid address')
  }
  if (contractAddress === ZeroAddress) {
    throw new Web3ConfigError('VITE_CONTRACT_ADDRESS must be the deployed contract')
  }

  const chainName = environment.VITE_CHAIN_NAME?.trim() || 'Configured EVM chain'
  const walletRpcUrl = environment.VITE_WALLET_RPC_URL?.trim() || undefined

  return {
    chainId,
    chainIdHex: `0x${chainId.toString(16)}`,
    chainName,
    contractAddress,
    walletRpcUrl,
  }
}
