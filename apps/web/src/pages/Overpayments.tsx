import { useCallback, useEffect, useState } from 'react'
import { useAuth } from '../contexts/AuthContext'

type Overpayment = {
  id: string
  claim_number: string
  kind: string
  service_line_number?: number | null
  amount: number
  payer_name?: string | null
  detail?: string | null
  identified_at: string
  due_date: string
  status: string
  overdue: boolean
  resolution_note?: string | null
}

const KIND_LABEL: Record<string, string> = {
  paid_above_allowed: 'Paid above allowed',
  duplicate_payment: 'Duplicate payment',
}
const money = (n: number) => n.toLocaleString(undefined, { style: 'currency', currency: 'USD' })

/** Overpayments found in remittances. Returning one is time-limited for many
 *  payers, so the due date is the column that matters. */
export default function Overpayments() {
  const { can } = useAuth()
  const [status, setStatus] = useState('identified')
  const [items, setItems] = useState<Overpayment[]>([])
  const [error, setError] = useState<string | null>(null)

  const load = useCallback(() => {
    const params = new URLSearchParams()
    if (status) params.set('status', status)
    fetch(`/api/v1/overpayments?${params}`)
      .then(r => (r.ok ? r.json() : Promise.reject(new Error(`HTTP ${r.status}`))))
      .then(data => { setItems(data); setError(null) })
      .catch(err => setError(err instanceof Error ? err.message : 'Could not load overpayments'))
  }, [status])

  useEffect(() => { load() }, [load])

  const record = async (item: Overpayment, next: string) => {
    const note = window.prompt(`Note for marking the ${money(item.amount)} overpayment on ${item.claim_number} as ${next}:`) ?? undefined
    if (note === undefined) return
    const resp = await fetch(`/api/v1/overpayments/${item.id}/status`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ status: next, note: note || null }),
    })
    if (!resp.ok) setError(`Could not update (HTTP ${resp.status})`)
    load()
  }

  const overdue = items.filter(i => i.overdue)
  return (
    <div className="page-body">
      <section className="workspace-heading">
        <div>
          <p>Payment integrity</p>
          <h1>Resolve overpayments with clarity.</h1>
          <span>Review potential duplicates and payment variances before they become avoidable exposure.</span>
        </div>
      </section>
      <div className="card">
        <div className="card-header"><h3>💸 Overpayments</h3></div>
        <div className="card-body">
          <p style={{ color: 'var(--text-muted)', marginTop: 0 }}>
            Payments above the allowed amount, or a claim paid twice without a reversal. Many payers require an
            identified overpayment to be refunded within a set time (60 days for Medicare); the due date comes
            from the refund window in Settings. A payer recoupment naming the claim marks it recouped automatically.
          </p>
          {overdue.length > 0 && (
            <div className="card" style={{ borderLeft: '4px solid var(--danger)', marginBottom: 12 }} role="alert">
              <div className="card-body">
                <strong>{overdue.length} overpayment(s) past their refund deadline</strong>, totalling{' '}
                {money(overdue.reduce((sum, i) => sum + i.amount, 0))}.
              </div>
            </div>
          )}
          <div className="filters-bar">
            <select className="form-select" value={status} onChange={e => setStatus(e.target.value)}>
              <option value="identified">Open (identified)</option>
              <option value="refunded">Refunded</option>
              <option value="recouped">Recouped by payer</option>
              <option value="disputed">Disputed</option>
              <option value="">All</option>
            </select>
          </div>
          {error && <p style={{ color: 'var(--danger)' }}>{error}</p>}
          <div className="table-container">
            <table>
              <thead>
                <tr><th>Claim</th><th>Payer</th><th>Kind</th><th>Amount</th><th>Due</th><th>Status</th><th>Detail</th><th></th></tr>
              </thead>
              <tbody>
                {items.length === 0 ? (
                  <tr><td colSpan={8} style={{ textAlign: 'center', padding: 24 }}>No overpayments</td></tr>
                ) : items.map(item => (
                  <tr key={item.id}>
                    <td>{item.claim_number}{item.service_line_number ? ` · line ${item.service_line_number}` : ''}</td>
                    <td>{item.payer_name || '—'}</td>
                    <td>{KIND_LABEL[item.kind] || item.kind}</td>
                    <td>{money(item.amount)}</td>
                    <td style={{ color: item.overdue ? 'var(--danger)' : undefined, fontWeight: item.overdue ? 600 : undefined }}>
                      {item.due_date}{item.overdue ? ' (overdue)' : ''}
                    </td>
                    <td>{item.status}</td>
                    <td style={{ fontSize: '0.8rem' }}>{item.resolution_note || item.detail || '—'}</td>
                    <td style={{ whiteSpace: 'nowrap' }}>
                      {can.approveWriteOffs() && item.status === 'identified' && (
                        <>
                          <button className="btn btn-sm btn-primary" onClick={() => record(item, 'refunded')}>Refunded</button>
                          <button className="btn btn-sm" style={{ marginLeft: 6 }} onClick={() => record(item, 'disputed')}>Dispute</button>
                        </>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      </div>
    </div>
  )
}
