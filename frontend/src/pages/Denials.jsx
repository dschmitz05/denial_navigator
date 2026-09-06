import React, { useState, useEffect } from 'react'

const API_BASE = '/api/v1'

function formatCurrency(value) {
  return new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD' }).format(value)
}

function formatDate(dateStr) {
  if (!dateStr) return '—'
  return new Date(dateStr).toLocaleDateString('en-US')
}

export default function Denials() {
  const [denials, setDenials] = useState([])
  const [selectedDenial, setSelectedDenial] = useState(null)
  const [showDetail, setShowDetail] = useState(false)
  const [statusFilter, setStatusFilter] = useState('')
  const [carcFilter, setCarcFilter] = useState('')
  const [carcOptions, setCarcOptions] = useState([])
  const [generatingId, setGeneratingId] = useState(null)
  const [error, setError] = useState(null)
  const [notice, setNotice] = useState(null)
  const [queueing, setQueueing] = useState(false)
  const [loading, setLoading] = useState(true)

  const loadDenials = (filters = {}) => {
    const params = new URLSearchParams({ limit: 50 })
    if (filters.status) params.set('status', filters.status)
    if (filters.carc_code) params.set('carc_code', filters.carc_code)

    fetch(`${API_BASE}/denials?${params}`)
      .then(r => r.json())
      .then(data => { setDenials(data); setLoading(false) })
      .catch(err => { console.error(err); setLoading(false) })
  }

  // Re-query whenever a filter changes. Previously the selects only set
  // state and nothing reloaded until the Filter button was pressed, so
  // choosing a filter appeared to do nothing.
  useEffect(() => {
    setLoading(true)
    loadDenials({ status: statusFilter, carc_code: carcFilter })
  }, [statusFilter, carcFilter])

  // The CARC list is built from the codes actually present in the data.
  // It used to be hard-coded to five codes that mostly never occur.
  //
  // It also has to follow the status filter: the counts are a promise about
  // how many rows picking that code will show, so they have to be counted
  // over the same denials the table is listing. Counting denials already sent
  // to Appeals or the Worklist made every count read high.
  useEffect(() => {
    const params = new URLSearchParams()
    if (statusFilter) params.set('status', statusFilter)
    fetch(`${API_BASE}/denials/carc-options?${params}`)
      .then(r => r.json())
      .then(data => setCarcOptions(Array.isArray(data) ? data : []))
      .catch(err => console.error('CARC options failed:', err))
  }, [statusFilter])

  const handleGenerateAnalysis = async (denialId) => {
    // Track the specific row: a single `generating` flag disabled every AI
    // button on the page while one request was in flight.
    setGeneratingId(denialId)
    setError(null)
    try {
      const resp = await fetch(`${API_BASE}/analyses/generate`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ denial_id: denialId }),
      })
      const result = await resp.json()
      if (resp.ok) {
        loadDenials({ status: statusFilter, carc_code: carcFilter })
        if (selectedDenial?.id === denialId) {
          setSelectedDenial(prev => ({ ...prev, analysis: result }))
        }
      } else {
        // A failure used to be swallowed entirely, so the button looked inert.
        setError(result?.detail || `Analysis failed (HTTP ${resp.status})`)
      }
    } catch (err) {
      console.error('Analysis generation failed:', err)
      setError(err.message || 'Analysis request failed')
    }
    setGeneratingId(null)
  }

  // Map the AI's required_action onto the resolution types the queue accepts,
  // so the recommendation becomes a unit of work rather than advice. This is a
  // fallback: the server sends `recommended_resolution`, which also applies the
  // PR rule (a patient-responsibility balance is billed, never written off).
  const RESOLUTION_FOR = {
    appeal: 'appeal_letter',
    coding_correction: 'corrected_claim',
    clinical_documentation: 'clinical_docs',
    bill_patient: 'bill_patient',
    no_action_required: 'write_off',
  }

  // Which tab a queued item lands on. Mirrors APPEAL_RESOLUTION_TYPES in
  // api-gateway/routes/appeals.py — only a letter to the payer is an appeal.
  const APPEAL_TYPES = ['appeal_letter']
  const WORKLIST_TYPES = ['corrected_claim', 'clinical_docs', 'payer_contact', 'bill_patient', 'write_off']
  const destinationFor = (t) => (APPEAL_TYPES.includes(t) ? 'Appeals' : 'Worklist')
  const LABELS = {
    appeal_letter: '⚖️ Appeal letter',
    corrected_claim: '✏️ Corrected claim',
    clinical_docs: '📄 Clinical documentation',
    payer_contact: '📞 Payer contact',
    bill_patient: '🧾 Bill patient',
    write_off: '🗑️ Write-off',
  }

  const handleQueueAppeal = async (denial, resolutionType) => {
    setQueueing(true)
    setNotice(null)
    try {
      const resp = await fetch(`${API_BASE}/appeals`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          denial_id: denial.id,
          ai_analysis_id: denial.ai_analysis_id || null,
          resolution_type: resolutionType,
        }),
      })
      const data = await resp.json()
      setNotice(resp.ok
        ? { error: false, text: `Queued as ${resolutionType.replace(/_/g, ' ')}. Track it on the ${destinationFor(resolutionType)} page.` }
        : { error: true, text: data?.detail || `Could not queue (HTTP ${resp.status})` })
      if (resp.ok) {
        loadDenials({ status: statusFilter, carc_code: carcFilter })
        setSelectedDenial(prev => ({ ...prev, appeal_id: data.id, appeal_status: data.outcome_status }))
      }
    } catch (err) {
      setNotice({ error: true, text: err.message })
    }
    setQueueing(false)
  }

  const handleSelectDenial = async (denial) => {
    const resp = await fetch(`${API_BASE}/denials/${denial.id}`)
    const data = await resp.json()
    setSelectedDenial(data)
    setShowDetail(true)
  }

  const carcChoices = carcFilter && !carcOptions.some(o => o.carc_code === carcFilter)
    ? [...carcOptions, { carc_code: carcFilter, description: 'No matches under this status filter', denial_count: 0 }]
    : carcOptions

  const recommended = selectedDenial?.recommended_resolution
    || (selectedDenial?.required_action ? RESOLUTION_FOR[selectedDenial.required_action] : null)

  return (
    <div className="page-body">
      <div className="filters-bar">
        <select className="form-select" value={statusFilter} onChange={e => setStatusFilter(e.target.value)}>
          <option value="">Active Only</option>
          <option value="open">Open</option>
          <option value="analyzed">Analyzed</option>
          <option value="in_progress">In Progress (worklist)</option>
          <option value="in_appeal">In Appeal</option>
          <option value="appealed">Appealed/Resolved</option>
        </select>
        <select className="form-select" value={carcFilter} onChange={e => setCarcFilter(e.target.value)}>
          <option value="">All CARC Codes ({carcChoices.length})</option>
          {carcChoices.map(o => (
            <option key={o.carc_code} value={o.carc_code}>
              {o.carc_code} — {o.description} ({o.denial_count})
            </option>
          ))}
        </select>
        <button className="btn" onClick={() => { setStatusFilter(''); setCarcFilter('') }}>Clear</button>
      </div>

      {notice && (
        <div className="card" style={{ marginBottom: 12, borderLeft: `4px solid ${notice.error ? 'var(--danger)' : 'var(--success)'}` }}>
          <div className="card-body" style={{ color: notice.error ? 'var(--danger)' : 'var(--success-text)' }}>{notice.text}</div>
        </div>
      )}

      {error && (
        <div className="card" style={{ marginBottom: 12, borderLeft: '4px solid var(--danger)' }}>
          <div className="card-body" style={{ color: 'var(--danger)' }}>
            <strong>AI analysis failed:</strong> {error}
          </div>
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
                <th>Amount</th>
                <th>Status</th>
                <th>Deadline</th>
                <th>Actions</th>
              </tr>
            </thead>
            <tbody>
              {denials.length === 0 ? (
                <tr><td colSpan="9" style={{ textAlign: 'center', padding: 20 }}>No denials found</td></tr>
              ) : (
                denials.map(d => (
                  <tr key={d.id} style={{ cursor: 'pointer' }} onClick={() => handleSelectDenial(d)}>
                    <td>{d.claim_number}</td>
                    <td>{d.patient_name || '—'}</td>
                    <td>{d.payer_name}</td>
                    <td>{d.cpt_code || '—'}</td>
                    <td>{d.carc_code || '—'}</td>
                    <td>{formatCurrency(d.charge_amount)}</td>
                    <td><span className={`badge badge-${(d.status || 'open').replace(/ /g, '-')}`}>{d.status || 'open'}</span></td>
                    <td>{formatDate(d.appeal_deadline)}</td>
                    <td>
                      <button
                        className="btn btn-sm btn-primary"
                        onClick={e => { e.stopPropagation(); handleGenerateAnalysis(d.id) }}
                        disabled={generatingId === d.id}
                      >
                        {generatingId === d.id ? '⏳' : '🤖 AI'}
                      </button>
                    </td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>
      </div>

      {/* Detail Modal */}
      {showDetail && selectedDenial && (
        <div className="modal-overlay" onClick={() => setShowDetail(false)}>
          <div className="modal" onClick={e => e.stopPropagation()}>
            <div className="modal-header">
              <h3>Denial Details — {selectedDenial.claim_number}</h3>
              <button className="btn" onClick={() => setShowDetail(false)}>✕</button>
            </div>
            <div className="modal-body">
              {/* Patient & Claim Info */}
              <div className="detail-grid">
                <div className="detail-item">
                  <div className="detail-label">Patient</div>
                  <div className="detail-value">{selectedDenial.patient_name || '—'}</div>
                </div>
                <div className="detail-item">
                  <div className="detail-label">Payer</div>
                  <div className="detail-value">{selectedDenial.payer_name}</div>
                </div>
                <div className="detail-item">
                  <div className="detail-label">CPT Code</div>
                  <div className="detail-value">{selectedDenial.cpt_code || '—'}</div>
                </div>
                <div className="detail-item">
                  <div className="detail-label">Charge</div>
                  <div className="detail-value">{formatCurrency(selectedDenial.charge_amount)}</div>
                </div>
                <div className="detail-item">
                  <div className="detail-label">CARC</div>
                  <div className="detail-value">{selectedDenial.carc_code} — {selectedDenial.carc_description || '—'}</div>
                </div>
                <div className="detail-item">
                  <div className="detail-label">Diagnosis</div>
                  <div className="detail-value">{selectedDenial.icd_10_codes?.join(', ') || '—'}</div>
                </div>
              </div>

              {/* AI Analysis */}
              {selectedDenial.explanation && (
                <div style={{ marginBottom: 20 }}>
                  <h4 style={{ marginBottom: 8 }}>🤖 AI Analysis</h4>
                  <div className="card">
                    <div className="card-body">
                      <p style={{ marginBottom: 12 }}><strong>Explanation:</strong> {selectedDenial.explanation}</p>
                      <p style={{ marginBottom: 12 }}><strong>Category:</strong> {selectedDenial.denial_category}</p>
                      {selectedDenial.required_action && (
                        <p style={{ marginBottom: 12 }}>
                          <strong>Required action:</strong>{' '}
                          <span className="badge">{selectedDenial.required_action.replace(/_/g, ' ')}</span>
                        </p>
                      )}
                      {selectedDenial.confidence_score && (
                        <p style={{ marginBottom: 12 }}><strong>Confidence:</strong> {Math.round(selectedDenial.confidence_score * 100)}%</p>
                      )}
                      {Array.isArray(selectedDenial.steps) && selectedDenial.steps.length > 0 && (
                        <div>
                          <strong>Resolution steps:</strong>
                          <ol style={{ marginTop: 6, paddingLeft: 20 }}>
                            {selectedDenial.steps.map((s, i) => (
                              <li key={i} style={{ marginBottom: 4 }}>
                                {typeof s === 'string' ? s : s.action}
                              </li>
                            ))}
                          </ol>
                        </div>
                      )}
                    </div>
                  </div>
                </div>
              )}

              {/* Queue this denial as work. Appeals and everything else are
                  offered separately, because they go to different tabs. */}
              {selectedDenial.ai_analysis_id && (
                <div style={{ marginBottom: 20 }}>
                  <h4 style={{ marginBottom: 8 }}>📋 Queue this denial as work</h4>
                  {selectedDenial.appeal_id ? (
                    <p style={{ color: 'var(--gray-500)' }}>
                      Already queued as{' '}
                      <strong>{(selectedDenial.appeal_resolution_type || 'work').replace(/_/g, ' ')}</strong>
                      {' '}— status{' '}
                      <span className="badge">{selectedDenial.appeal_status || 'queued'}</span>.
                      Track it on the {destinationFor(selectedDenial.appeal_resolution_type)} page.
                    </p>
                  ) : (
                    <div>
                      {recommended && (
                        <p style={{ color: 'var(--gray-500)', marginBottom: 10 }}>
                          Recommended: <strong>{LABELS[recommended] || recommended}</strong>
                          {' '}— it will go to the {destinationFor(recommended)} tab.
                        </p>
                      )}
                      {selectedDenial.cagc === 'PR' && (
                        <p style={{ color: 'var(--warning-text)', marginBottom: 10, fontSize: '0.9rem' }}>
                          🧾 This is a <strong>PR (Patient Responsibility)</strong> adjustment —
                          {' '}{selectedDenial.carc_description || 'the payer assigned this balance to the patient'}.
                          {' '}The balance is collectible and belongs on the patient's statement, not written off.
                        </p>
                      )}

                      <div style={{ marginBottom: 12 }}>
                        <div style={{ fontSize: '0.8rem', textTransform: 'uppercase', letterSpacing: 1, color: 'var(--gray-500)', marginBottom: 6 }}>
                          Appeal the payer's decision → Appeals tab
                        </div>
                        <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
                          {APPEAL_TYPES.map(t => (
                            <button key={t} className={`btn btn-sm ${t === recommended ? 'btn-primary' : ''}`}
                                    disabled={queueing}
                                    onClick={() => handleQueueAppeal(selectedDenial, t)}>
                              {queueing ? 'Queueing…' : LABELS[t]}{t === recommended ? ' (recommended)' : ''}
                            </button>
                          ))}
                        </div>
                      </div>

                      <div>
                        <div style={{ fontSize: '0.8rem', textTransform: 'uppercase', letterSpacing: 1, color: 'var(--gray-500)', marginBottom: 6 }}>
                          Work the denial without appealing → Worklist tab
                        </div>
                        <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
                          {WORKLIST_TYPES.map(t => (
                            <button key={t} className={`btn btn-sm ${t === recommended ? 'btn-primary' : ''}`}
                                    disabled={queueing}
                                    onClick={() => handleQueueAppeal(selectedDenial, t)}>
                              {queueing ? 'Queueing…' : LABELS[t]}{t === recommended ? ' (recommended)' : ''}
                            </button>
                          ))}
                        </div>
                      </div>
                    </div>
                  )}
                </div>
              )}

              {/* Appeal Letter */}
              {selectedDenial.draft_appeal_letter && (
                <div>
                  <h4 style={{ marginBottom: 8 }}>📝 Draft Appeal Letter</h4>
                  <div className="appeal-letter">
                    {selectedDenial.draft_appeal_letter}
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
