const WEI_PER_ETH = 1_000_000_000_000_000_000n
const MAX_DATE_MILLISECONDS = 8_640_000_000_000_000n

export function formatWei(value: string): string {
  if (!/^\d+$/.test(value)) {
    throw new Error('Wei value must be an unsigned decimal string')
  }

  const wei = BigInt(value)
  const whole = wei / WEI_PER_ETH
  const remainder = wei % WEI_PER_ETH

  if (remainder === 0n) {
    return `${whole} ETH`
  }

  const fraction = remainder.toString().padStart(18, '0').replace(/0+$/, '')
  return `${whole}.${fraction} ETH`
}

export function shortenAddress(address: string): string {
  if (address.length <= 14) {
    return address
  }
  return `${address.slice(0, 6)}…${address.slice(-4)}`
}

export function formatDeadline(timestamp: string): string {
  if (!/^\d+$/.test(timestamp)) {
    return timestamp
  }

  const milliseconds = BigInt(timestamp) * 1_000n
  if (milliseconds > MAX_DATE_MILLISECONDS) {
    return `Unix ${timestamp}`
  }

  const date = new Date(Number(milliseconds))
  if (Number.isNaN(date.getTime())) {
    return `Unix ${timestamp}`
  }

  return new Intl.DateTimeFormat('en', {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
    timeZone: 'UTC',
    timeZoneName: 'short',
  }).format(date)
}
