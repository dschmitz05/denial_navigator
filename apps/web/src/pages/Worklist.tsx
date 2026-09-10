import React, { useState, useEffect } from 'react'
import { useAuth } from '../contexts/AuthContext'
import AssigneeCell, { useAssignableUsers } from '../components/AssigneeCell'

const API_BASE = '/api/v1'

type WorkItem = { id: string; denial_id?: string; claim_number?: string; patient_name?: string; payer_name?: string; cpt_code?: string; carc_code?: string; resolution_type?: string; assigned_user_id?: string | null; assigned_username?: string | null; charge_amount?: number; outcome_status?: string | null; ai_analysis_id?: string | null }
type WorkDetail = WorkItem & { carc_description?: string; service_from?: string; explanation?: string; root_cause_summary?: string; required_action?: string; steps?: Array<string | { action?: string }>; needs_appeal?: boolean }
type Notice = { error: boolean; text: string }
type Feedback = { rating: number; feedback_text: string }
type WorkType = { label: string; icon: string; blurb: string; submitLabel: string | null; skipSubmit: boolean }

function formatCurrency(value?: number) {
  return new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD' }).format(value || 0)
}

function formatDate(dateStr?: string) {
  if (!dateStr) return '—'
  return new Date(dateStr).toLocaleDateString('en-US')
}

// Each kind of non-appeal work has its own middle step. "Submit to payer" is
// meaningless for a write-off and misleading for a records request, so the
// label follows the work rather than the other way round.
const WORK_TYPES: Record<string, WorkType> = {
  corrected_claim: {
    label: 'Corrected claim',
    icon: '✏️',
    blurb: 'Fix the coding or billing error, then resubmit the claim.',
    submitLabel: '📤 Mark resubmitted',
    skipSubmit: false,
  },
  clinical_docs: {
    label: 'Clinical documentation',
    icon: '📄',
    blurb: 'Gather the records or medical justification the payer asked for and send them.',
    submitLabel: '📤 Mark records sent',
    skipSubmit: false,
  },
  payer_contact: {
    label: 'Payer contact',
    icon: '📞',
    blurb: 'Call the payer to clarify the adjudication before deciding on next steps.',
    submitLabel: '📤 Mark payer contacted',
    skipSubmit: false,
  },
  bill_patient: {
    label: 'Bill patient',
    icon: '🧾',
    blurb: 'The payer assigned this balance to the patient — deductible, coinsurance or copay. '
         + 'Move it to patient billing and send a statement. This money is collectible.',
    submitLabel: '📤 Mark statement sent',
    skipSubmit: false,
  },
  write_off: {
    label: 'Write-off',
    icon: '🗑️',
    blurb: 'Correctly adjudicated — close it out and write the balance off.',
    submitLabel: null,
    skipSubmit: true,
  },
}

function workType(resolutionType?: string): WorkType {
  return (resolutionType && WORK_TYPES[resolutionType]) || {
    label: (resolutionType || 'work').replace(/_/g, ' '),
    icon: '🛠️',
    blurb: '',
    submitLabel: '📤 Mark submitted',
    skipSubmit: false,
  }
}

export default function Worklist() {
  const [items, setItems] = useState<WorkItem[]>([])
  const [selected, setSelected] = useState<WorkItem | null>(null)
  const [detail, setDetail] = useState<WorkDetail | null>(null)
  const [showDetail, setShowDetail] = useState(false)
  const [outcomeFilter, setOutcomeFilter] = useState('')
  const [typeFilter, setTypeFilter] = useState('')
  const [loading, setLoading] = useState(true)
  const [updating, setUpdating] = useState(false)
  const [notice, setNotice] = useState<Notice | null>(null)
  const [feedback, setFeedback] = useState<Feedback>({ rating: 0, feedback_text: '' })

  const { can } = useAuth()
  // Only managers can assign, so only they need the user list.
  const assignableUsers = useAssignableUsers(can.assignWork())

  const loadItems = (filters: { outcome_status?: string; resolution_type?: string } = {}) => {
    // category=worklist is the server-side rule: everything that is not an
    // appeal. The Appeals tab asks the same endpoint for category=appeal, so
    // an item can never show up on both or fall between them.
    const params = new URLSearchParams({ limit: '50', category: 'worklist' })
    if (filters.outcome_status) params.set('outcome_status', filters.outcome_status)
    if (filters.resolution_type) params.set('resolution_type', filters.resolution_type)

    fetch(`${API_BASE}/appeals?${params}`)
      .then(r => r.json())
      .then((data: unknown) => { setItems(Array.isArray(data) ? data as WorkItem[] : []); setLoading(false) })
      .catch(err => { console.error(err); setLoading(false) })
  }

  useEffect(() => {
    setLoading(true)
    loadItems({ outcome_status: outcomeFilter, resolution_type: typeFilter })
  }, [outcomeFilter, typeFilter])

  const openItem = async (item: WorkItem) => {
    setSelected(item)
    setShowDetail(true)
    setDetail(null)
    try {
      const resp = await fetch(`${API_BASE}/appeals/${item.id}`)
      const data = await resp.json()
      setDetail(resp.ok ? data as WorkDetail : null)
      if (!resp.ok) setNotice({ error: true, text: typeof data?.detail === 'string' ? data.detail : 'Could not load this item' })
    } catch (err) {
      setNotice({ error: true, text: err instanceof Error ? err.message : 'Could not load this item' })
    }
  }

  const updateOutcome = async (itemId: string, newStatus: string) => {
    setUpdating(true)
    try {
      const resp = await fetch(`${API_BASE}/appeals/${itemId}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ outcome_status: newStatus }),
      })
      if (!resp.ok) {
        const data = await resp.json().catch(() => ({}))
        throw new Error(typeof data.detail === 'string' ? data.detail : `Update failed (HTTP ${resp.status})`)
      }
      loadItems({ outcome_status: outcomeFilter, resolution_type: typeFilter })
      const refreshed = await resp.json()
      setSelected(prev => (prev ? { ...prev, outcome_status: typeof refreshed.outcome_status === 'string' ? refreshed.outcome_status : prev.outcome_status } : prev))
    } catch (err) {
      setNotice({ error: true, text: err instanceof Error ? err.message : 'Update failed' })
    }
    setUpdating(false)
  }

  const submitFeedback = async (item: WorkItem, outcome: string) => {
    const analysisId = detail?.ai_analysis_id || item.ai_analysis_id
    if (!analysisId) return
    try {
      await fetch(`${API_BASE}/feedback`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          ai_analysis_id: analysisId,
          rating: feedback.rating || null,
          accepted: outcome === 'resolved',
          action_taken: item.resolution_type,
          // Only a resubmitted claim can be "paid on resubmit"; for a records
          // request or a write-off the question does not apply.
          was_paid_on_resubmit: item.resolution_type === 'corrected_claim'
            ? outcome === 'resolved'
            : null,
          resubmit_result: outcome,
          feedback_text: feedback.feedback_text || null,
        }),
      })
    } catch (err) {
      setNotice({ error: true, text: `Feedback failed: ${err instanceof Error ? err.message : 'request failed'}` })
    }
  }

  const closeOut = async (item: WorkItem, outcome: string) => {
    setUpdating(true)
    await updateOutcome(item.id, outcome)
    await submitFeedback(item, outcome)
    setFeedback({ rating: 0, feedback_text: '' })
    setShowDetail(false)
    setNotice({ error: false, text: `Marked ${outcome.replace(/_/g, ' ')}.` })
    setUpdating(false)
  }

  const status = selected?.outcome_status || 'queued'
  const isOpen = !['approved', 'overruled', 'resolved', 'denied_again', 'cancelled'].includes(status)
  const meta = workType(selected?.resolution_type)

  return (
    <div className="page-body">
      <div className="filters-bar">
        <select className="form-select" value={typeFilter} onChange={e => setTypeFilter(e.target.value)}>
          <option value="">All Work Types</option>
          {Object.entries(WORK_TYPES).map(([value, t]) => (
            <option key={value} value={value}>{t.icon} {t.label}</option>
          ))}
        </select>
        <select className="form-select" value={outcomeFilter} onChange={e => setOutcomeFilter(e.target.value)}>
          <option value="">Open Items</option>
          <option value="queued">Queued</option>
          <option value="in_progress">In Progress</option>
          <option value="submitted">Submitted</option>
          <option value="resolved">Resolved</option>
          <option value="cancelled">Cancelled</option>
        </select>
        <button className="btn" onClick={() => { setOutcomeFilter(''); setTypeFilter('') }}>Clear</button>
      </div>

      {notice && (
        <div className="card" style={{ marginBottom: 12, borderLeft: `4px solid ${notice.error ? 'var(--danger)' : 'var(--success)'}` }}>
          <div className="card-body" style={{ color: notice.error ? 'var(--danger)' : 'var(--success-text)' }}>{notice.text}</div>
        </div>
      )}

      <div className="card">
        <div className="table-container">
          <table>
            <thead>
              <tr>
                <th>Claim #</th>
                <th>Patient</th>
                <th>Payer</th>
                <th>CPT</th>
                <th>CARC</th>
                <th>Work Type</th>
                <th>Owner</th>
                <th>Amount</th>
                <th>Status</th>
                <th>Actions</th>
              </tr>
            </thead>
            <tbody>
              {loading ? (
                <tr><td colSpan={10} style={{ textAlign: 'center', padding: 20 }}>Loading…</td></tr>
              ) : items.length === 0 ? (
                <tr><td colSpan={10} style={{ textAlign: 'center', padding: 20, color: 'var(--gray-500)' }}>
                  Nothing in the worklist. Denials queued as a corrected claim, clinical
                  documentation, a payer call or a write-off land here — appeals go to the Appeals tab.
                </td></tr>
              ) : (
                items.map(item => {
                  const t = workType(item.resolution_type)
                  return (
                    <tr key={item.id}>
                      <td>{item.claim_number}</td>
                      <td>{item.patient_name || '—'}</td>
                      <td>{item.payer_name}</td>
                      <td>{item.cpt_code || '—'}</td>
                      <td>{item.carc_code || '—'}</td>
                      <td>{t.icon} {t.label}</td>
                      <AssigneeCell item={item} users={assignableUsers}
                                    onAssigned={() => loadItems({ outcome_status: outcomeFilter, resolution_type: typeFilter })} />
                      <td>{formatCurrency(item.charge_amount)}</td>
                      <td><span className={`badge badge-${(item.outcome_status || 'queued').replace(/_/g, '-')}`}>
                        {(item.outcome_status || 'queued').replace(/_/g, ' ')}
                      </span></td>
                      <td>
                        <button className="btn btn-sm" onClick={() => openItem(item)}>👁️ View</button>
                      </td>
                    </tr>
                  )
                })
              )}
            </tbody>
          </table>
        </div>
      </div>

      {showDetail && selected && (
        <div className="modal-overlay" onClick={() => setShowDetail(false)}>
          <div className="modal" onClick={e => e.stopPropagation()} style={{ maxWidth: '800px' }}>
            <div className="modal-header">
              <h3>{meta.icon} {meta.label} — {selected.claim_number}</h3>
              <button className="btn" onClick={() => setShowDetail(false)}>✕</button>
            </div>
            <div className="modal-body">
              <p style={{ color: 'var(--gray-500)', marginBottom: 16 }}>{meta.blurb}</p>

              {/* Status actions, worded for the work being done */}
              <div style={{ marginBottom: 20, display: 'flex', gap: 8, flexWrap: 'wrap' }}>
                {isOpen && status === 'queued' && (
                  <button className="btn btn-primary btn-sm" disabled={updating}
                          onClick={() => updateOutcome(selected.id, 'in_progress')}>
                    ▶️ Start work
                  </button>
                )}
                {isOpen && !meta.skipSubmit && ['queued', 'in_progress'].includes(status) && (
                  <button className="btn btn-sm" disabled={updating}
                          onClick={() => updateOutcome(selected.id, 'submitted')}>
                    {meta.submitLabel}
                  </button>
                )}
                {isOpen && (
                  <button className="btn btn-success btn-sm" disabled={updating}
                          onClick={() => closeOut(selected, 'resolved')}>
                    ✅ {meta.skipSubmit ? 'Write off & close' : 'Mark resolved'}
                  </button>
                )}
                {isOpen && selected.resolution_type === 'corrected_claim' && status === 'submitted' && (
                  <button className="btn btn-sm" disabled={updating}
                          onClick={() => closeOut(selected, 'denied_again')}>
                    ❌ Denied again
                  </button>
                )}
                {isOpen && (
                  <button className="btn btn-sm" disabled={updating}
                          onClick={() => closeOut(selected, 'cancelled')}>
                    🚫 Cancel
                  </button>
                )}
                {!isOpen && (
                  <div style={{ color: 'var(--gray-500)', fontSize: '0.9rem', display: 'flex', alignItems: 'center' }}>
                    This item is closed ({status.replace(/_/g, ' ')}).
                    {['denied_again', 'cancelled'].includes(status) && ' The denial is back in the denials queue.'}
                  </div>
                )}
              </div>

              <div className="detail-grid">
                <div className="detail-item">
                  <div className="detail-label">Patient</div>
                  <div className="detail-value">{selected.patient_name || '—'}</div>
                </div>
                <div className="detail-item">
                  <div className="detail-label">Payer</div>
                  <div className="detail-value">{selected.payer_name}</div>
                </div>
                <div className="detail-item">
                  <div className="detail-label">CPT</div>
                  <div className="detail-value">{selected.cpt_code || '—'}</div>
                </div>
                <div className="detail-item">
                  <div className="detail-label">Amount</div>
                  <div className="detail-value">{formatCurrency(selected.charge_amount)}</div>
                </div>
                <div className="detail-item">
                  <div className="detail-label">CARC</div>
                  <div className="detail-value">
                    {detail?.carc_code || selected.carc_code || '—'}
                    {detail?.carc_description ? ` — ${detail.carc_description}` : ''}
                  </div>
                </div>
                <div className="detail-item">
                  <div className="detail-label">Service date</div>
                  <div className="detail-value">{formatDate(detail?.service_from)}</div>
                </div>
              </div>

              {/* What the biller actually has to do */}
              {detail && (
                <div style={{ marginTop: 20 }}>
                  <h4 style={{ marginBottom: 8 }}>🤖 AI Analysis</h4>
                  {detail.explanation ? (
                    <div className="card">
                      <div className="card-body">
                        <p style={{ marginBottom: 12 }}><strong>Why it was denied:</strong> {detail.explanation}</p>
                        {detail.root_cause_summary && (
                          <p style={{ marginBottom: 12 }}><strong>Root cause:</strong> {detail.root_cause_summary}</p>
                        )}
                        {detail.required_action && (
                          <p style={{ marginBottom: 12 }}>
                            <strong>Required action:</strong>{' '}
                            <span className="badge">{detail.required_action.replace(/_/g, ' ')}</span>
                          </p>
                        )}
                        {Array.isArray(detail.steps) && detail.steps.length > 0 && (
                          <div>
                            <strong>Resolution steps:</strong>
                            <ol style={{ marginTop: 6, paddingLeft: 20 }}>
                              {detail.steps.map((s, i) => (
                                <li key={i} style={{ marginBottom: 4 }}>
                                  {typeof s === 'string' ? s : s.action}
                                </li>
                              ))}
                            </ol>
                          </div>
                        )}
                        {detail.needs_appeal && (
                          <p style={{ marginTop: 12, color: 'var(--warning-text)' }}>
                            ⚠️ The AI recommended an appeal for this denial. If that is the right
                            call, cancel this item and re-queue it as an appeal letter.
                          </p>
                        )}
                      </div>
                    </div>
                  ) : (
                    <p style={{ color: 'var(--gray-500)' }}>
                      No AI analysis is linked to this item. Run one from the Denials page for
                      step-by-step guidance.
                    </p>
                  )}
                </div>
              )}

              {/* Feedback — same loop the Appeals tab feeds */}
              {(detail?.ai_analysis_id || selected.ai_analysis_id) && isOpen && (
                <div className="card" style={{ marginTop: 20 }}>
                  <div className="card-body">
                    <h4 style={{ marginBottom: 8 }}>Rate the AI recommendation</h4>
                    <div style={{ display: 'flex', gap: 6, marginBottom: 8 }}>
                      {[1, 2, 3, 4, 5].map(n => (
                        <button key={n}
                                className={`btn btn-sm ${feedback.rating === n ? 'btn-primary' : ''}`}
                                onClick={() => setFeedback({ ...feedback, rating: n })}>
                          {n}
                        </button>
                      ))}
                      <span style={{ alignSelf: 'center', color: 'var(--gray-500)', fontSize: '0.85rem' }}>
                        1 = unusable, 5 = resolved it
                      </span>
                    </div>
                    <textarea className="form-input" rows={2}
                              placeholder="What did you change, and what actually worked?"
                              value={feedback.feedback_text}
                              onChange={e => setFeedback({ ...feedback, feedback_text: e.target.value })} />
                    <p style={{ fontSize: '0.8rem', color: 'var(--gray-500)', marginTop: 6 }}>
                      Recorded when you close this item out above.
                    </p>
                  </div>
                </div>
              )}
            </div>
            <div className="modal-footer">
              <button className="btn" onClick={() => setShowDetail(false)}>Close</button>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
