import { useCallback, useEffect, useState } from 'react'

import type { Web3Config } from './config'
import {
  getInjectedProvider,
  isUserRejected,
  readWallet,
  requestWalletConnection,
  subscribeToWallet,
  switchWalletNetwork,
  type InjectedProvider,
} from './wallet'

export type WalletPhase =
  | 'checking'
  | 'unavailable'
  | 'disconnected'
  | 'connecting'
  | 'wrong_network'
  | 'switching'
  | 'ready'
  | 'error'

export interface WalletController {
  phase: WalletPhase
  account: string | null
  chainId: bigint | null
  provider: InjectedProvider | null
  message: string | null
  connect(): Promise<void>
  switchNetwork(): Promise<void>
}

interface WalletState {
  phase: WalletPhase
  account: string | null
  chainId: bigint | null
  message: string | null
}

export function useWallet(config: Web3Config | null): WalletController {
  const [provider] = useState(() => getInjectedProvider())
  const [state, setState] = useState<WalletState>({
    phase: provider ? 'checking' : 'unavailable',
    account: null,
    chainId: null,
    message: null,
  })

  const stateFor = useCallback(
    (account: string | null, chainId: bigint): WalletState => ({
      phase: account
        ? config && chainId === config.chainId
          ? 'ready'
          : 'wrong_network'
        : 'disconnected',
      account,
      chainId,
      message: null,
    }),
    [config],
  )

  useEffect(() => {
    if (!provider) {
      return
    }

    let active = true
    readWallet(provider)
      .then((snapshot) => {
        if (active) {
          setState(stateFor(snapshot.account, snapshot.chainId))
        }
      })
      .catch(() => {
        if (active) {
          setState({
            phase: 'error',
            account: null,
            chainId: null,
            message: 'The injected wallet could not be read.',
          })
        }
      })

    const unsubscribe = subscribeToWallet(provider, {
      accountsChanged(accounts) {
        setState((current) =>
          current.chainId === null
            ? { ...current, account: accounts[0] ?? null }
            : stateFor(accounts[0] ?? null, current.chainId),
        )
      },
      chainChanged(chainId) {
        setState((current) => stateFor(current.account, chainId))
      },
    })

    return () => {
      active = false
      unsubscribe()
    }
  }, [provider, stateFor])

  const connect = useCallback(async () => {
    if (!provider) {
      setState((current) => ({
        ...current,
        phase: 'unavailable',
        message: 'Install an injected wallet such as MetaMask to continue.',
      }))
      return
    }

    setState((current) => ({ ...current, phase: 'connecting', message: null }))
    try {
      const snapshot = await requestWalletConnection(provider)
      setState(stateFor(snapshot.account, snapshot.chainId))
    } catch (error) {
      setState((current) => ({
        ...current,
        phase: current.account ? current.phase : 'disconnected',
        message: isUserRejected(error)
          ? 'Wallet connection was cancelled.'
          : 'The wallet could not be connected.',
      }))
    }
  }, [provider, stateFor])

  const switchNetwork = useCallback(async () => {
    if (!provider || !config) {
      return
    }
    setState((current) => ({ ...current, phase: 'switching', message: null }))
    try {
      await switchWalletNetwork(provider, config)
      const snapshot = await readWallet(provider)
      setState(stateFor(snapshot.account, snapshot.chainId))
    } catch (error) {
      setState((current) => ({
        ...current,
        phase: 'wrong_network',
        message: isUserRejected(error)
          ? 'Network switch was cancelled.'
          : `Switch to ${config.chainName} in your wallet.`,
      }))
    }
  }, [config, provider, stateFor])

  return { ...state, provider, connect, switchNetwork }
}
