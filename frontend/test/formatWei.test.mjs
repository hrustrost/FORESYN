import assert from 'node:assert/strict'
import test from 'node:test'

import { formatWei } from '../src/format.ts'

test('formats whole ETH without converting wei through Number', () => {
  assert.equal(formatWei('4000000000000000000'), '4 ETH')
})

test('preserves fractional wei and values beyond Number.MAX_SAFE_INTEGER', () => {
  assert.equal(formatWei('1'), '0.000000000000000001 ETH')
  assert.equal(
    formatWei('340282366920938463463374607431768211455'),
    '340282366920938463463.374607431768211455 ETH',
  )
})
