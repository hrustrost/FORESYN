import type { Position } from '../api'
import { formatWei, shortenAddress } from '../format'

interface PositionsListProps {
  positions: Position[]
}

export function PositionsList({ positions }: PositionsListProps) {
  if (positions.length === 0) {
    return (
      <div className="positions-empty">
        <span className="positions-empty__mark" aria-hidden="true">
          ∅
        </span>
        <p>No positions have been indexed for this market.</p>
      </div>
    )
  }

  return (
    <div className="positions-table-wrap">
      <table className="positions-table">
        <thead>
          <tr>
            <th scope="col">Participant</th>
            <th scope="col" className="value-yes">
              YES stake
            </th>
            <th scope="col" className="value-no">
              NO stake
            </th>
            <th scope="col">Total</th>
          </tr>
        </thead>
        <tbody>
          {positions.map((position) => (
            <tr key={position.user_address}>
              <td>
                <span className="address" title={position.user_address}>
                  {shortenAddress(position.user_address)}
                </span>
                <small>Updated block {position.updated_block_number}</small>
              </td>
              <td className="value-yes">{formatWei(position.yes_stake)}</td>
              <td className="value-no">{formatWei(position.no_stake)}</td>
              <td className="value-total">{formatWei(position.total_stake)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}
