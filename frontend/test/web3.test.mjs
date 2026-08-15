import assert from 'node:assert/strict'
import test from 'node:test'

import { parsePositiveEthAmount, YES_OUTCOME } from '../src/web3/contract.ts'
import { loadWeb3Config } from '../src/web3/config.ts'
import {
  projectionBaseline,
  projectionIncludesPosition,
} from '../src/web3/transaction.ts'
import { normalizeChainId } from '../src/web3/wallet.ts'

test('normalizes wallet chain IDs without Number conversion', () => {
  assert.equal(normalizeChainId('0x7a69'), 31337n)
  assert.equal(normalizeChainId('31337'), 31337n)
  assert.throws(() => normalizeChainId('31337.0'))
})

test('parses only positive exact ETH amounts', () => {
  assert.equal(parsePositiveEthAmount('4'), 4_000_000_000_000_000_000n)
  assert.equal(parsePositiveEthAmount('0.000000000000000001'), 1n)
  assert.throws(() => parsePositiveEthAmount('0'))
  assert.throws(() => parsePositiveEthAmount('1e3'))
  assert.throws(() => parsePositiveEthAmount('0.0000000000000000001'))
})

test('builds exact configured chain values and rejects the zero contract', () => {
  const config = loadWeb3Config({
    VITE_CHAIN_ID: '31337',
    VITE_CONTRACT_ADDRESS: '0x1111111111111111111111111111111111111111',
    VITE_CHAIN_NAME: 'Anvil',
    VITE_WALLET_RPC_URL: 'http://127.0.0.1:8545',
  })
  assert.equal(config.chainId, 31337n)
  assert.equal(config.chainIdHex, '0x7a69')
  assert.throws(() =>
    loadWeb3Config({
      VITE_CHAIN_ID: '31337',
      VITE_CONTRACT_ADDRESS: '0x0000000000000000000000000000000000000000',
    }),
  )
})

test('waits for both the emitted pool and user stake projection', () => {
  const market = {
    market_id: '1',
    resolver: '0x1111111111111111111111111111111111111111',
    creator: '0x2222222222222222222222222222222222222222',
    deadline: '1',
    metadata_digest: `0x${'33'.repeat(32)}`,
    creation_block_number: '100',
    yes_pool: '2000000000000000000',
    no_pool: '0',
    total_pool: '2000000000000000000',
  }
  const account = '0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
  const positions = [
    {
      user_address: account,
      yes_stake: '2000000000000000000',
      no_stake: '0',
      total_stake: '2000000000000000000',
      updated_block_number: '101',
    },
  ]
  const baseline = projectionBaseline(market, positions, account, YES_OUTCOME)
  const amount = 1_000_000_000_000_000_000n

  assert.equal(
    projectionIncludesPosition(market, positions, account, YES_OUTCOME, amount, baseline),
    false,
  )
  assert.equal(
    projectionIncludesPosition(
      { ...market, yes_pool: '3000000000000000000' },
      [{ ...positions[0], yes_stake: '3000000000000000000' }],
      account,
      YES_OUTCOME,
      amount,
      baseline,
    ),
    true,
  )
})
