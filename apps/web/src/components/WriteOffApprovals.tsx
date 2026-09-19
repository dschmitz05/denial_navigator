import { useCallback, useEffect, useState } from 'react'

const API_BASE = '/api/v1'

type WriteOffRequest = {
  id: string
  amount: number
  reason?: string | null
  requested_by?: string | null
  requested_at: string
  claim_number: string
  payer_name?: string | null
  cagc: string
  carc_code?: string | null
  cpt_code?: string | null
}

const money = (n: number) => n.toLocaleString(undefined, { style: 'currency', currency: 'USD' })

/** Write-offs waiting for a manager. The API refuses a decision on your own
 *  request, so the buttons explain that instead of failing silently. */
export default function WriteOffApprovals({ onDecided }: { onDecided?: () => void }) {
  const [items, setItems] = useState<WriteOffRequest[]>([])
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState<string | null>(null)

  const load = useCallback(async () => {
    try {
      const resp = await fetch(`${API_BASE}/write-offs?status=pending`)
      if (!resp.ok) throw new Error(`Could not load write-off approvals (HTTP ${resp.status})`)
      setItems(await resp.json())
      setError(null)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Could not load write-off approvals')
    }
  }, [])

  useEffect(() => { load() }, [load])

  const decide = async (item: WriteOffRequest, decision: 'approve' | 'reject') => {
    let note: string | null = null
    if (decision === 'reject') {
      note = window.prompt(`Why reject the ${money(item.amount)} write-off on claim ${item.claim_number}?`)
      if (!note || !note.trim()) return
    }
    setBusy(item.id)
    try {
      const resp = await fetch(`${API_BASE}/write-offs/${item.id}/${decision}`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(note ? { note } : {}),
      })
      if (resp.status === 403) throw new Error('You cannot decide on a write-off you requested; another manager must.')
      if (!resp.ok) {
        const data = await resp.json().catch(() => ({}))
        throw new Error(typeof data.detail === 'string' ? data.detail : `Could not ${decision} (HTTP ${resp.status})`)
      }
      await load()
      onDecided?.()
    } catch (err) {
      setError(err instanceof Error ? err.message : `Could not ${decision}`)
    } finally {
      setBusy(null)
    }
  }

  if (!items.length && !error) return null

  return (
    <div className="card" style={{ marginBottom: 12, borderLeft: '4px solid var(--warning)' }}>
      <div className="card-header"><h3>Write-offs awaiting approval ({items.length})</h3></div>
      <div className="card-body">
        {error && <p style={{ color: 'var(--danger)' }}>{error}</p>}
        {items.length > 0 && (
          <div className="table-container">
            <table>
              <thead>
                <tr><th>Claim</th><th>Payer</th><th>Code</th><th>Amount</th><th>Requested by</th><th>Reason</th><th></th></tr>
              </thead>
              <tbody>
                {items.map(item => (
                  <tr key={item.id}>
                    <td>{item.claim_number}</td>
                    <td>{item.payer_name || '—'}</td>
                    <td>{item.cagc}-{item.carc_code || '?'}{item.cpt_code ? ` · ${item.cpt_code}` : ''}</td>
                    <td>{money(item.amount)}</td>
                    <td>{item.requested_by || '—'}<div style={{ fontSize: '0.8rem', color: 'var(--text-muted)' }}>{new Date(item.requested_at).toLocaleString()}</div></td>
                    <td>{item.reason || '—'}</td>
                    <td style={{ whiteSpace: 'nowrap' }}>
                      <button className="btn btn-sm btn-primary" disabled={busy === item.id} onClick={() => decide(item, 'approve')}>Approve</button>
                      <button className="btn btn-sm" style={{ marginLeft: 6 }} disabled={busy === item.id} onClick={() => decide(item, 'reject')}>Reject</button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </div>
  )
}
