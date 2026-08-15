import { useState, type FormEvent } from 'react'

import { shortenAddress } from '../format'
import {
  NO_OUTCOME,
  type PositionOutcome,
  YES_OUTCOME,
} from '../web3/contract'
import type { Web3Config } from '../web3/config'
import type { TransactionState, TransactionStage } from '../web3/transaction'
import type { WalletController } from '../web3/useWallet'

interface TakePositionPanelProps {
  marketId: string
  wallet: WalletController
  config: Web3Config | null
  configError: string | null
  transaction: TransactionState
  onSubmit(outcome: PositionOutcome, amount: string): Promise<void>
}

const activeStages: TransactionStage[] = [
  'awaiting_wallet',
  'submitted',
  'confirming',
  'confirmed',
  'waiting_indexer',
]

export function TakePositionPanel({
  marketId,
  wallet,
  config,
  configError,
  transaction,
  onSubmit,
}: TakePositionPanelProps) {
  const [outcome, setOutcome] = useState<PositionOutcome>(YES_OUTCOME)
  const [amount, setAmount] = useState('')
  const transactionActive = activeStages.includes(transaction.stage)

  function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    void onSubmit(outcome, amount)
  }

  return (
    <section className="take-position" aria-labelledby="take-position-heading">
      <div className="take-position__heading">
        <div>
          <p className="eyebrow">User-signed write</p>
          <h3 id="take-position-heading">Take a position</h3>
        </div>
        <span className="market-target">Market #{marketId}</span>
      </div>

      {configError && (
        <WalletNotice
          tone="error"
          title="Web3 configuration required"
          message={configError}
        />
      )}

      {!configError && wallet.phase === 'unavailable' && (
        <WalletNotice
          tone="neutral"
          title="Injected wallet not found"
          message="Install MetaMask or another EIP-1193 wallet to submit a position."
        />
      )}

      {!configError &&
        (wallet.phase === 'disconnected' ||
          wallet.phase === 'checking' ||
          wallet.phase === 'connecting' ||
          wallet.phase === 'error') && (
          <div className="wallet-gate">
            <p>
              Connect an injected wallet. Foresyn never receives or stores your private
              key.
            </p>
            <button
              type="button"
              onClick={() => void wallet.connect()}
              disabled={wallet.phase === 'checking' || wallet.phase === 'connecting'}
            >
              {wallet.phase === 'checking'
                ? 'Checking wallet…'
                : wallet.phase === 'connecting'
                  ? 'Waiting for wallet confirmation…'
                  : 'Connect Wallet'}
            </button>
            {wallet.message && <small>{wallet.message}</small>}
          </div>
        )}

      {!configError &&
        config &&
        (wallet.phase === 'wrong_network' || wallet.phase === 'switching') && (
          <div className="wallet-gate wallet-gate--warning">
            <p>
              Wrong network. Transactions are enabled only on {config.chainName} (chain{' '}
              {config.chainId.toString()}).
            </p>
            <button
              type="button"
              onClick={() => void wallet.switchNetwork()}
              disabled={wallet.phase === 'switching'}
            >
              {wallet.phase === 'switching' ? 'Switching network…' : 'Switch Network'}
            </button>
            {wallet.message && <small>{wallet.message}</small>}
          </div>
        )}

      {!configError && config && wallet.phase === 'ready' && wallet.account && (
        <form className="position-form" onSubmit={handleSubmit}>
          <div className="connected-as">
            <span>Connected</span>
            <code title={wallet.account}>{shortenAddress(wallet.account)}</code>
            <strong>Ready</strong>
          </div>

          <fieldset disabled={transactionActive}>
            <legend>Outcome</legend>
            <div className="outcome-picker">
              <label className={outcome === YES_OUTCOME ? 'outcome-yes selected' : 'outcome-yes'}>
                <input
                  type="radio"
                  name="outcome"
                  value={YES_OUTCOME}
                  checked={outcome === YES_OUTCOME}
                  onChange={() => setOutcome(YES_OUTCOME)}
                />
                <span>YES</span>
              </label>
              <label className={outcome === NO_OUTCOME ? 'outcome-no selected' : 'outcome-no'}>
                <input
                  type="radio"
                  name="outcome"
                  value={NO_OUTCOME}
                  checked={outcome === NO_OUTCOME}
                  onChange={() => setOutcome(NO_OUTCOME)}
                />
                <span>NO</span>
              </label>
            </div>
          </fieldset>

          <label className="amount-field">
            <span>Position amount</span>
            <span className="amount-input">
              <input
                type="text"
                inputMode="decimal"
                autoComplete="off"
                placeholder="0.10"
                value={amount}
                onChange={(event) => setAmount(event.target.value)}
                disabled={transactionActive}
                aria-label="Position amount in ETH"
              />
              <strong>ETH</strong>
            </span>
          </label>

          <button className="submit-position" type="submit" disabled={transactionActive}>
            {transactionActive ? transactionLabel(transaction.stage) : `Take ${outcome === YES_OUTCOME ? 'YES' : 'NO'} position`}
          </button>
        </form>
      )}

      {transaction.stage !== 'idle' && <TransactionProgress transaction={transaction} />}
    </section>
  )
}

function WalletNotice({
  tone,
  title,
  message,
}: {
  tone: 'neutral' | 'error'
  title: string
  message: string
}) {
  return (
    <div className={`wallet-notice wallet-notice--${tone}`}>
      <strong>{title}</strong>
      <p>{message}</p>
    </div>
  )
}

function transactionLabel(stage: TransactionStage): string {
  switch (stage) {
    case 'awaiting_wallet':
      return 'Waiting for wallet confirmation…'
    case 'submitted':
    case 'confirming':
      return 'Waiting for on-chain confirmation…'
    case 'confirmed':
    case 'waiting_indexer':
      return 'Waiting for indexer…'
    default:
      return 'Processing…'
  }
}

function TransactionProgress({ transaction }: { transaction: TransactionState }) {
  const completed = transaction.stage === 'indexed'
  const failed = transaction.stage === 'failed' || transaction.stage === 'indexing_delayed'

  return (
    <div
      className={`transaction-progress${completed ? ' transaction-progress--complete' : ''}${failed ? ' transaction-progress--failed' : ''}`}
      aria-live="polite"
    >
      <div className="transaction-progress__title">
        <strong>{transactionTitle(transaction.stage)}</strong>
        {transaction.hash && (
          <code title={transaction.hash}>{shortenAddress(transaction.hash)}</code>
        )}
      </div>
      <ol>
        <ProgressStep
          label="Wallet signature"
          state={stepState(transaction.stage, 1, Boolean(transaction.hash))}
        />
        <ProgressStep
          label="Transaction submitted"
          state={stepState(transaction.stage, 2, Boolean(transaction.hash))}
        />
        <ProgressStep
          label="Confirmed on-chain"
          state={stepState(transaction.stage, 3, Boolean(transaction.hash))}
        />
        <ProgressStep
          label="Indexed projection"
          state={stepState(transaction.stage, 4, Boolean(transaction.hash))}
        />
      </ol>
      {transaction.message && <p>{transaction.message}</p>}
    </div>
  )
}

function ProgressStep({
  label,
  state,
}: {
  label: string
  state: 'pending' | 'current' | 'complete' | 'failed'
}) {
  return (
    <li className={`progress-step progress-step--${state}`}>
      <span aria-hidden="true" />
      {label}
    </li>
  )
}

function stepState(
  stage: TransactionStage,
  step: number,
  hasTransactionHash: boolean,
): 'pending' | 'current' | 'complete' | 'failed' {
  if (stage === 'failed') {
    if (!hasTransactionHash) {
      return step === 1 ? 'failed' : 'pending'
    }
    if (step < 3) return 'complete'
    return step === 3 ? 'failed' : 'pending'
  }
  if (stage === 'indexing_delayed') {
    return step < 4 ? 'complete' : 'failed'
  }

  const currentStep: Record<TransactionStage, number> = {
    idle: 0,
    awaiting_wallet: 1,
    submitted: 2,
    confirming: 2,
    confirmed: 3,
    waiting_indexer: 4,
    indexed: 5,
    indexing_delayed: 4,
    failed: 1,
  }
  const current = currentStep[stage]
  if (step < current) return 'complete'
  if (step === current) return 'current'
  return 'pending'
}

function transactionTitle(stage: TransactionStage): string {
  const titles: Record<TransactionStage, string> = {
    idle: 'Ready',
    awaiting_wallet: 'Waiting for wallet confirmation',
    submitted: 'Transaction submitted',
    confirming: 'Waiting for confirmation',
    confirmed: 'Confirmed on-chain',
    waiting_indexer: 'Waiting for indexer',
    indexed: 'Indexed and updated',
    indexing_delayed: 'On-chain, indexing delayed',
    failed: 'Transaction failed',
  }
  return titles[stage]
}
