import React, { useState, useEffect } from 'react'

const API_BASE = '/api/v1'

function formatCurrency(value) {
  return new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD' }).format(value)
}

function formatDate(dateStr) {
  if (!dateStr) return '—'
  return new Date(dateStr).toLocaleDateString('en-US')
}

export default function Appeals() {
  const [appeals, setAppeals] = useState([])
  const [selectedAppeal, setSelectedAppeal] = useState(null)
  const [showDetail, setShowDetail] = useState(false)
  const [letterData, setLetterData] = useState(null)
  const [outcomeFilter, setOutcomeFilter] = useState('')
  const [loading, setLoading] = useState(true)
  const [updating, setUpdating] = useState(false)
  const [feedback, setFeedback] = useState({ rating: 0, was_paid_on_resubmit: null, feedback_text: '' })
  const [notice, setNotice] = useState(null)

  const loadAppeals = (filters = {}) => {
    const params = new URLSearchParams({ limit: 50 })
    if (filters.outcome_status) params.set('outcome_status', filters.outcome_status)

    fetch(`${API_BASE}/appeals?${params}`)
      .then(r => r.json())
      .then(data => { setAppeals(data); setLoading(false) })
      .catch(err => { console.error(err); setLoading(false) })
  }

  // Reload on selection. The Filter button was the only way to apply this.
  useEffect(() => {
    setLoading(true)
    loadAppeals({ outcome_status: outcomeFilter })
  }, [outcomeFilter])

  const handleUpdateOutcome = async (appealId, newStatus) => {
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

  const submitFeedback = async (appeal, outcome) => {
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
      setNotice({ error: true, text: `Feedback failed: ${err.message}` })
    }
  }

  const handleResolve = async (appeal, outcome) => {
    setUpdating(true)
    await handleUpdateOutcome(appeal.id, outcome)
    await submitFeedback(appeal, outcome)
    setFeedback({ rating: 0, was_paid_on_resubmit: null, feedback_text: '' })
    setShowDetail(false)
    setUpdating(false)
  }

  const handleViewLetter = async (appealId) => {
    const resp = await fetch(`${API_BASE}/appeals/${appealId}/letter`)
    const data = await resp.json()
    setLetterData(data)
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
        </select>
        <button className="btn" onClick={() => setOutcomeFilter('')}>Clear</button>
      </div>

      {notice && (
        <div className="card" style={{ marginBottom: 12, borderLeft: `4px solid ${notice.error ? '#dc2626' : '#16a34a'}` }}>
          <div className="card-body" style={{ color: notice.error ? '#dc2626' : '#166534' }}>{notice.text}</div>
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
                <th>Amount</th>
                <th>Status</th>
                <th>Actions</th>
              </tr>
            </thead>
            <tbody>
              {appeals.length === 0 ? (
                <tr><td colSpan="8" style={{ textAlign: 'center', padding: 20 }}>No appeals found</td></tr>
              ) : (
                appeals.map(a => (
                  <tr key={a.id}>
                    <td>{a.claim_number}</td>
                    <td>{a.patient_name || '—'}</td>
                    <td>{a.payer_name}</td>
                    <td>{a.cpt_code || '—'}</td>
                    <td>{a.resolution_type?.replace(/_/g, ' ')}</td>
                    <td>{formatCurrency(a.charge_amount)}</td>
                    <td><span className={`badge badge-${(a.outcome_status || 'queued').replace(/ /g, '-')}`}>{a.outcome_status || 'queued'}</span></td>
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
                <button className="btn btn-sm" disabled={updating}
                        onClick={() => handleResolve(selectedAppeal, 'cancelled')}>
                  🚫 Cancel
                </button>
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
                      <span style={{ alignSelf: 'center', color: '#6b7280', fontSize: '0.85rem' }}>
                        1 = unusable, 5 = resolved it
                      </span>
                    </div>
                    <textarea className="form-input" rows={2}
                              placeholder="What did you change, and what actually worked?"
                              value={feedback.feedback_text}
                              onChange={e => setFeedback({ ...feedback, feedback_text: e.target.value })} />
                    <p style={{ fontSize: '0.8rem', color: '#6b7280', marginTop: 6 }}>
                      Recorded when you mark the outcome above.
                    </p>
                  </div>
                </div>
              )}

              {/* Appeal Letter Preview */}
              {letterData && (
                <div>
                  <h4 style={{ marginBottom: 8 }}>📝 Appeal Letter Preview</h4>
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

${letterData.draft_appeal_letter || 'No appeal letter generated. Generate AI analysis first.'}

---

${letterData.explanation ? `AI Analysis: ${letterData.explanation}` : ''}`}
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
