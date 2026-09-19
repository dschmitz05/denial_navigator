import { useEffect, useState } from 'react'

/** What common PLB reason codes mean to a biller. */
export const PLB_REASON: Record<string, string> = {
  WO: 'Overpayment recovery (offset)',
  FB: 'Forward balance',
  L6: 'Interest owed',
  '72': 'Authorized return',
  CS: 'Adjustment',
  J1: 'Nonreimbursable',
  WU: 'Unspecified recovery',
  IR: 'IRS withholding',
  L3: 'Penalty',
  B2: 'Rebate',
  C5: 'Temporary allowance',
  CT: 'Capitation interest',
}

export type ProviderAdjustment = {
  id: string
  payer_name?: string | null
  trace_number?: string | null
  payment_date?: string | null
  reason_code: string
  reference_number?: string | null
  amount: number
  claim_number?: string | null
  file_name?: string | null
}

type SummaryRow = { payer_name: string; month: string; reason_code: string; lines: number; amount: number }

const money = (n: number) => n.toLocaleString(undefined, { style: 'currency', currency: 'USD' })

export const reasonLabel = (code: string) => `${code}${PLB_REASON[code] ? ` · ${PLB_REASON[code]}` : ''}`

/** Totals by month, payer and reason. A positive amount was taken out of a
 *  payment (a recoupment, say); a negative one was added (interest). */
export default function ProviderAdjustmentsSummary() {
  const [rows, setRows] = useState<SummaryRow[] | null>(null)

  useEffect(() => {
    fetch('/api/v1/ingestion/provider-adjustments/summary')
      .then(r => (r.ok ? r.json() : []))
      .then(setRows)
      .catch(() => setRows([]))
  }, [])

  if (!rows || rows.length === 0) return null
  return (
    <div className="card" style={{ marginTop: 24 }}>
      <div className="card-header"><h3>🏦 Provider-level adjustments (PLB)</h3></div>
      <div className="card-body">
        <p style={{ color: 'var(--text-muted)', fontSize: '0.85rem', marginTop: 0 }}>
          Payment changes that belong to no single patient claim. A positive amount was taken out of a payment
          (for example an overpayment recovered from other claims); a negative amount was added, such as interest.
        </p>
        <div className="table-container">
          <table>
            <thead><tr><th>Month</th><th>Payer</th><th>Reason</th><th>Lines</th><th>Amount</th></tr></thead>
            <tbody>
              {rows.map(r => (
                <tr key={`${r.month}-${r.payer_name}-${r.reason_code}`}>
                  <td>{r.month}</td>
                  <td>{r.payer_name}</td>
                  <td>{reasonLabel(r.reason_code)}</td>
                  <td>{r.lines}</td>
                  <td>{money(r.amount)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>
    </div>
  )
}
