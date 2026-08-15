import { shortenAddress } from '../format'
import type { Web3Config } from '../web3/config'
import type { WalletController } from '../web3/useWallet'

interface WalletButtonProps {
  wallet: WalletController
  config: Web3Config | null
}

export function WalletButton({ wallet, config }: WalletButtonProps) {
  if (!config) {
    return <span className="wallet-button wallet-button--disabled">Web3 not configured</span>
  }

  if (wallet.phase === 'ready' && wallet.account) {
    return (
      <span className="wallet-button wallet-button--connected" title={wallet.account}>
        <span className="wallet-button__dot" aria-hidden="true" />
        {shortenAddress(wallet.account)}
      </span>
    )
  }

  if (wallet.phase === 'wrong_network' || wallet.phase === 'switching') {
    return (
      <button
        className="wallet-button wallet-button--warning"
        type="button"
        onClick={() => void wallet.switchNetwork()}
        disabled={wallet.phase === 'switching'}
      >
        {wallet.phase === 'switching' ? 'Switching…' : `Switch to ${config.chainName}`}
      </button>
    )
  }

  if (wallet.phase === 'unavailable') {
    return <span className="wallet-button wallet-button--disabled">Wallet unavailable</span>
  }

  return (
    <button
      className="wallet-button"
      type="button"
      onClick={() => void wallet.connect()}
      disabled={wallet.phase === 'checking' || wallet.phase === 'connecting'}
    >
      {wallet.phase === 'checking'
        ? 'Checking wallet…'
        : wallet.phase === 'connecting'
          ? 'Waiting for wallet…'
          : 'Connect Wallet'}
    </button>
  )
}
