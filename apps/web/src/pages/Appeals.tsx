import React, { useState, useEffect } from 'react'
import { useAuth } from '../contexts/AuthContext'
import AssigneeCell, { useAssignableUsers } from '../components/AssigneeCell'

const API_BASE = '/api/v1'

type Appeal = {
  id: string; denial_id: string; claim_number?: string; patient_name?: string; payer_name?: string; cpt_code?: string
  resolution_type?: string; assigned_user_id?: string | null; assigned_username?: string | null; needs_appeal?: boolean
  charge_amount?: number; outcome_status?: string | null; ai_analysis_id?: string | null
}
type Letter = { draft_appeal_letter?: string; needs_appeal?: boolean; explanation?: string; claim_number?: string; patient_name?: string; date_of_birth?: string; payer_name?: string; payer_id_number?: string; service_from?: string; cpt_code?: string; icd_10_codes?: string[] }
type Notice = { error: boolean; text: string }
type Feedback = { rating: number; was_paid_on_resubmit: boolean | null; feedback_text: string }

function formatCurrency(value?: number) {
  return new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD' }).format(value || 0)
}

function formatDate(dateStr?: string) {
  if (!dateStr) return '—'
  return new Date(dateStr).toLocaleDateString('en-US')
}

export default function Appeals() {
  const [appeals, setAppeals] = useState<Appeal[]>([])
  const [selectedAppeal, setSelectedAppeal] = useState<Appeal | null>(null)
  const [showDetail, setShowDetail] = useState(false)
  const [letterData, setLetterData] = useState<Letter | null>(null)
  const [outcomeFilter, setOutcomeFilter] = useState('')
  const [loading, setLoading] = useState(true)
  const [updating, setUpdating] = useState(false)
  const [feedback, setFeedback] = useState<Feedback>({ rating: 0, was_paid_on_resubmit: null, feedback_text: '' })
  const [notice, setNotice] = useState<Notice | null>(null)
  const [generating, setGenerating] = useState(false)

  const { can } = useAuth()
  // Only managers can assign, so only they need the user list.
  const assignableUsers = useAssignableUsers(can.assignWork())

  const loadAppeals = (filters: { outcome_status?: string } = {}) => {
    // category=appeal keeps this tab to work that actually challenges the
    // payer. Corrected claims, records requests, payer calls and write-offs
    // are denial work, not appeals, and live on the Worklist tab.
    const params = new URLSearchParams({ limit: '50', category: 'appeal' })
    if (filters.outcome_status) params.set('outcome_status', filters.outcome_status)

    fetch(`${API_BASE}/appeals?${params}`)
      .then(r => r.json())
      .then((data: unknown) => { setAppeals(Array.isArray(data) ? data as Appeal[] : []); setLoading(false) })
      .catch(err => { console.error(err); setLoading(false) })
  }

  // Reload on selection. The Filter button was the only way to apply this.
  useEffect(() => {
    setLoading(true)
    loadAppeals({ outcome_status: outcomeFilter })
  }, [outcomeFilter])

  const handleUpdateOutcome = async (appealId: string, newStatus: string) => {
    setUpdating(true)
    try {
      await fetch(`${API_BASE}/appeals/${appealId}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ outcome_status: newStatus }),
      })
      loadAppeals({ outcome_status: outcomeFilter })
    } catch (err) {
      console.error('Update failed:', err)
    }
    setUpdating(false)
  }

  const submitFeedback = async (appeal: Appeal, outcome: string) => {
    // The feedback loop only means anything if it records whether the AI's
    // recommendation actually got the claim paid.
    if (!appeal.ai_analysis_id) return
    try {
      await fetch(`${API_BASE}/feedback`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          ai_analysis_id: appeal.ai_analysis_id,
          rating: feedback.rating || null,
          accepted: outcome === 'approved' || outcome === 'resolved',
          action_taken: appeal.resolution_type,
          was_paid_on_resubmit: outcome === 'approved' ? true : (outcome === 'denied_again' ? false : null),
          resubmit_result: outcome,
          feedback_text: feedback.feedback_text || null,
        }),
      })
      setNotice({ error: false, text: 'Outcome and feedback recorded.' })
    } catch (err) {
      setNotice({ error: true, text: `Feedback failed: ${err instanceof Error ? err.message : 'request failed'}` })
    }
  }

  const handleResolve = async (appeal: Appeal, outcome: string) => {
    setUpdating(true)
    await handleUpdateOutcome(appeal.id, outcome)
    await submitFeedback(appeal, outcome)
    setFeedback({ rating: 0, was_paid_on_resubmit: null, feedback_text: '' })
    setShowDetail(false)
    setUpdating(false)
  }

  const handleViewLetter = async (appealId: string) => {
    const resp = await fetch(`${API_BASE}/appeals/${appealId}/letter`)
    const data = await resp.json()
    setLetterData(data as Letter)
  }

  const handleGenerateAnalysis = async (appeal: Appeal) => {
    setGenerating(true)
    setNotice(null)
    try {
      const resp = await fetch(`${API_BASE}/analyses/generate`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ denial_id: appeal.denial_id, temperature: 0.3 }),
      })
      const data = await resp.json() as Record<string, unknown>
      if (!resp.ok) {
        throw new Error(typeof data.detail === 'string' ? data.detail : 'Analysis failed')
      }
      if (data.stored) {
        setNotice({ error: false, text: 'Analysis generated. Refreshing letter...' })
        await handleViewLetter(appeal.id)
      } else {
        const parsed = data.parsed_json as Record<string, unknown> | undefined
        setNotice({ error: true, text: `Analysis failed: ${typeof parsed?.error === 'string' ? parsed.error : 'Unknown error'}` })
      }
    } catch (err) {
      setNotice({ error: true, text: `Analysis failed: ${err instanceof Error ? err.message : 'request failed'}` })
    }
    setGenerating(false)
  }

  return (
    <div className="page-body">
      <div className="filters-bar">
        <select className="form-select" value={outcomeFilter} onChange={e => setOutcomeFilter(e.target.value)}>
          <option value="">All Statuses</option>
          <option value="queued">Queued</option>
          <option value="in_progress">In Progress</option>
          <option value="submitted">Submitted</option>
          <option value="resolved">Resolved</option>
          <option value="denied_again">Denied Again</option>
          <option value="cancelled">Cancelled</option>
        </select>
        <button className="btn" onClick={() => setOutcomeFilter('')}>Clear</button>
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
                <th>Resolution</th>
                <th>Owner</th>
                <th>AI Says</th>
                <th>Amount</th>
                <th>Status</th>
                <th>Actions</th>
              </tr>
            </thead>
            <tbody>
              {appeals.length === 0 ? (
                <tr><td colSpan={10} style={{ textAlign: 'center', padding: 20, color: 'var(--gray-500)' }}>
                  No appeals in the queue. Denials queued as a corrected claim, clinical
                  documentation, a payer call or a write-off are on the Worklist tab.
                </td></tr>
              ) : (
                appeals.map(a => (
                  <tr key={a.id}>
                    <td>{a.claim_number}</td>
                    <td>{a.patient_name || '—'}</td>
                    <td>{a.payer_name}</td>
                    <td>{a.cpt_code || '—'}</td>
                    <td>{a.resolution_type?.replace(/_/g, ' ')}</td>
                      <AssigneeCell item={a} users={assignableUsers}
                                    onAssigned={() => loadAppeals({ outcome_status: outcomeFilter })} />
                    <td><span style={{ color: a.needs_appeal ? 'var(--success)' : 'var(--danger)', fontSize: '0.85rem' }}>{a.needs_appeal ? '✅ Yes' : '❌ No'}</span></td>
                    <td>{formatCurrency(a.charge_amount)}</td>
                    <td><span className={`badge badge-${(a.outcome_status || 'queued').replace(/_/g, '-')}`}>{a.outcome_status || 'queued'}</span></td>
                    <td>
                      <button className="btn btn-sm" onClick={() => { setSelectedAppeal(a); setShowDetail(true); handleViewLetter(a.id) }}>
                        👁️ View
                      </button>
                    </td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>
      </div>

      {/* Appeal Detail Modal */}
      {showDetail && selectedAppeal && (
        <div className="modal-overlay" onClick={() => setShowDetail(false)}>
          <div className="modal" onClick={e => e.stopPropagation()} style={{ maxWidth: '800px' }}>
            <div className="modal-header">
              <h3>Appeal Details — {selectedAppeal.claim_number}</h3>
              <button className="btn" onClick={() => setShowDetail(false)}>✕</button>
            </div>
            <div className="modal-body">
              {/* Status Actions */}
              <div style={{ marginBottom: 20, display: 'flex', gap: 8, flexWrap: 'wrap' }}>
                {(!selectedAppeal.outcome_status || selectedAppeal.outcome_status === 'queued') && (
                  <button className="btn btn-primary btn-sm" disabled={updating}
                          onClick={() => handleUpdateOutcome(selectedAppeal.id, 'in_progress')}>
                    ▶️ Start Processing
                  </button>
                )}
                {['queued', 'in_progress'].includes(selectedAppeal.outcome_status || 'queued') && (
                  <button className="btn btn-sm" disabled={updating}
                          onClick={() => handleUpdateOutcome(selectedAppeal.id, 'submitted')}>
                    📤 Submit to Payer
                  </button>
                )}
                {selectedAppeal.outcome_status === 'submitted' && (
                  <>
                    <button className="btn btn-success btn-sm" disabled={updating}
                            onClick={() => handleResolve(selectedAppeal, 'approved')}>
                      ✅ Payer Approved — Paid
                    </button>
                    <button className="btn btn-sm" disabled={updating}
                            onClick={() => handleResolve(selectedAppeal, 'denied_again')}>
                      ❌ Denied Again
                    </button>
                  </>
                )}
                {selectedAppeal.outcome_status === 'cancelled' && (
                  <div style={{ color: 'var(--gray-500)', fontSize: '0.9rem', display: 'flex', alignItems: 'center' }}>
                    ⚠️ This appeal was cancelled. The denial is now back in your denials queue.
                  </div>
                )}
                {selectedAppeal.outcome_status && !['approved', 'overruled', 'resolved', 'denied_again', 'cancelled'].includes(selectedAppeal.outcome_status) && (
                  <button className="btn btn-sm" disabled={updating}
                          onClick={() => handleResolve(selectedAppeal, 'cancelled')}>
                    🚫 Cancel
                  </button>
                )}
              </div>

              {/* Feedback — closes the loop the schema was built for */}
              {selectedAppeal.ai_analysis_id && selectedAppeal.outcome_status === 'submitted' && (
                <div className="card" style={{ marginBottom: 20 }}>
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
                      Recorded when you mark the outcome above.
                    </p>
                  </div>
                </div>
              )}

              {/* Appeal Letter Preview */}
              {letterData && (
                <div>
                  <h4 style={{ marginBottom: 8 }}>📝 Appeal Letter Preview</h4>
                  {!letterData.draft_appeal_letter && !letterData.needs_appeal && letterData.explanation ? (
                    <div className="callout callout-info">
                      <p style={{ fontWeight: 600, marginBottom: 8 }}>⚠️ This denial does not require an appeal.</p>
                      {/* Inherits the callout's foreground rather than naming a
                          colour that only works on a pale background. */}
                      <p style={{ lineHeight: 1.6 }}>{letterData.explanation}</p>
                      <p style={{ color: 'var(--gray-500)', fontSize: '0.85rem', marginTop: 12 }}>
                        This is a corrected-claim situation. Fix the billing/coding error and resubmit.
                      </p>
                    </div>
                  ) : !letterData.draft_appeal_letter ? (
                    <div style={{ padding: 16, background: 'var(--gray-50)', borderRadius: 8, textAlign: 'center' }}>
                      <p style={{ color: 'var(--gray-500)', marginBottom: 12 }}>No appeal letter has been generated yet. Run AI analysis to create one.</p>
                      <button className="btn btn-primary btn-sm" disabled={generating}
                              onClick={() => handleGenerateAnalysis(selectedAppeal)}>
                        {generating ? '⏳ Generating...' : '🤖 Generate AI Analysis'}
                      </button>
                    </div>
                  ) : (
                    <div className="appeal-letter" style={{ whiteSpace: 'pre-wrap', fontFamily: 'monospace', fontSize: '0.85rem' }}>
{`RE: Appeal for Claim ${letterData.claim_number}

Patient: ${letterData.patient_name}
DOB: ${formatDate(letterData.date_of_birth)}
Payer: ${letterData.payer_name}
Payer ID: ${letterData.payer_id_number}
Service Date: ${formatDate(letterData.service_from)}
CPT: ${letterData.cpt_code}
Diagnosis: ${letterData.icd_10_codes?.join(', ')}

---

${letterData.draft_appeal_letter}

---

${letterData.explanation ? `AI Analysis: ${letterData.explanation}` : ''}`}
                    </div>
                  )}
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
