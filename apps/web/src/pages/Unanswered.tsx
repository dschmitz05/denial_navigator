import { useCallback, useEffect, useState } from 'react'

type Unanswered = {
  id: string
  claim_number: string
  payer_name?: string | null
  total_charge?: number | null
  service_from?: string | null
  submitted_at: string
  days_outstanding: number
  response_days: number
  timely_filing_due?: string | null
  days_to_timely_filing?: number | null
  last_action?: string | null
  last_note?: string | null
  last_action_at?: string | null
}

const ACTIONS: Record<string, string> = {
  status_inquiry: 'Status inquiry (276)',
  resubmitted: 'Resubmitted',
  payer_contact: 'Payer contacted',
}
const money = (n?: number | null) => (n ?? 0).toLocaleString(undefined, { style: 'currency', currency: 'USD' })

/** Claims the payer has not answered. With no remittance they never become
 *  denials, so this is where they surface before timely filing runs out. */
export default function UnansweredClaims() {
  const [items, setItems] = useState<Unanswered[]>([])
  const [error, setError] = useState<string | null>(null)

  const load = useCallback(() => {
    fetch('/api/v1/claims/unanswered')
      .then(r => (r.ok ? r.json() : Promise.reject(new Error(`HTTP ${r.status}`))))
      .then(data => { setItems(data); setError(null) })
      .catch(err => setError(err instanceof Error ? err.message : 'Could not load'))
  }, [])
  useEffect(() => { load() }, [load])

  const record = async (item: Unanswered, action: string) => {
    const note = window.prompt(`${ACTIONS[action]} for claim ${item.claim_number} - note (optional):`)
    if (note === null) return
    const resp = await fetch(`/api/v1/claims/${item.id}/followups`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ action, note: note || null }),
    })
    if (!resp.ok) setError(`Could not record (HTTP ${resp.status})`)
    load()
  }

  return (
    <div className="page-body">
      <div className="card">
        <div className="card-header"><h3>📭 No response from payer</h3></div>
        <div className="card-body">
          <p style={{ color: 'var(--text-muted)', marginTop: 0 }}>
            Claims submitted on an 837 with no remittance after the payer's usual response time (a "Payer response"
            rule in Settings, 30 days if none). A claim leaves this list when its first 835 arrives. Set a timely-filing
            rule to see how long is left.
          </p>
          {error && <p style={{ color: 'var(--danger)' }}>{error}</p>}
          <div className="table-container">
            <table>
              <thead>
                <tr><th>Claim</th><th>Payer</th><th>Charge</th><th>Waiting</th><th>Timely filing</th><th>Last follow-up</th><th></th></tr>
              </thead>
              <tbody>
                {items.length === 0 ? (
                  <tr><td colSpan={7} style={{ textAlign: 'center', padding: 24 }}>No unanswered claims</td></tr>
                ) : items.map(item => {
                  const left = item.days_to_timely_filing
                  return (
                    <tr key={item.id}>
                      <td>{item.claim_number}</td>
                      <td>{item.payer_name || '—'}</td>
                      <td>{money(item.total_charge)}</td>
                      <td>{item.days_outstanding} days (expected {item.response_days})</td>
                      <td style={{ color: left != null && left < 0 ? 'var(--danger)' : left != null && left <= 30 ? 'var(--warning-text)' : undefined }}>
                        {item.timely_filing_due ? `${item.timely_filing_due} (${left != null && left < 0 ? `${-left} days past` : `${left} days left`})` : 'No rule'}
                      </td>
                      <td style={{ fontSize: '0.8rem' }}>
                        {item.last_action ? `${ACTIONS[item.last_action] || item.last_action}, ${new Date(item.last_action_at || '').toLocaleDateString()}${item.last_note ? ` — ${item.last_note}` : ''}` : '—'}
                      </td>
                      <td style={{ whiteSpace: 'nowrap' }}>
                        {Object.entries(ACTIONS).map(([action, label]) => (
                          <button key={action} className="btn btn-sm" style={{ marginLeft: 4 }} onClick={() => record(item, action)}>{label}</button>
                        ))}
                      </td>
                    </tr>
                  )
                })}
              </tbody>
            </table>
          </div>
        </div>
      </div>
    </div>
  )
}
