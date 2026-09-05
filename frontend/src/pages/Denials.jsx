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
  useEffect(() => {
    fetch(`${API_BASE}/denials/carc-options`)
      .then(r => r.json())
      .then(setCarcOptions)
      .catch(err => console.error('CARC options failed:', err))
  }, [])

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

  // Map the AI's required_action onto the resolution types the appeals queue
  // accepts, so the recommendation becomes a unit of work rather than advice.
  const RESOLUTION_FOR = {
    appeal: 'appeal_letter',
    coding_correction: 'corrected_claim',
    clinical_documentation: 'clinical_docs',
    no_action_required: 'write_off',
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
        ? { error: false, text: `Queued as ${resolutionType.replace(/_/g, ' ')}. Track it on the Appeals page.` }
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

  return (
    <div className="page-body">
      <div className="filters-bar">
        <select className="form-select" value={statusFilter} onChange={e => setStatusFilter(e.target.value)}>
          <option value="">All Statuses</option>
          <option value="open">Open</option>
          <option value="analyzed">Analyzed</option>
        </select>
        <select className="form-select" value={carcFilter} onChange={e => setCarcFilter(e.target.value)}>
          <option value="">All CARC Codes ({carcOptions.length})</option>
          {carcOptions.map(o => (
            <option key={o.carc_code} value={o.carc_code}>
              {o.carc_code} — {o.description} ({o.denial_count})
            </option>
          ))}
        </select>
        <button className="btn" onClick={() => { setStatusFilter(''); setCarcFilter('') }}>Clear</button>
      </div>

      {notice && (
        <div className="card" style={{ marginBottom: 12, borderLeft: `4px solid ${notice.error ? '#dc2626' : '#16a34a'}` }}>
          <div className="card-body" style={{ color: notice.error ? '#dc2626' : '#166534' }}>{notice.text}</div>
        </div>
      )}

      {error && (
        <div className="card" style={{ marginBottom: 12, borderLeft: '4px solid #dc2626' }}>
          <div className="card-body" style={{ color: '#dc2626' }}>
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

              {/* Queue this denial as work */}
              {selectedDenial.ai_analysis_id && (
                <div style={{ marginBottom: 20 }}>
                  <h4 style={{ marginBottom: 8 }}>📋 Send to appeals queue</h4>
                  {selectedDenial.appeal_id ? (
                    <p style={{ color: '#6b7280' }}>
                      Already queued — status{' '}
                      <span className="badge">{selectedDenial.appeal_status || 'queued'}</span>.
                      Track it on the Appeals page.
                    </p>
                  ) : (
                    <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
                      {selectedDenial.required_action && RESOLUTION_FOR[selectedDenial.required_action] && (
                        <button className="btn btn-primary btn-sm" disabled={queueing}
                                onClick={() => handleQueueAppeal(selectedDenial, RESOLUTION_FOR[selectedDenial.required_action])}>
                          {queueing ? 'Queueing…' : `Queue as ${RESOLUTION_FOR[selectedDenial.required_action].replace(/_/g, ' ')} (recommended)`}
                        </button>
                      )}
                      {['appeal_letter', 'corrected_claim', 'clinical_docs', 'payer_contact', 'write_off']
                        .filter(t => t !== RESOLUTION_FOR[selectedDenial.required_action])
                        .map(t => (
                          <button key={t} className="btn btn-sm" disabled={queueing}
                                  onClick={() => handleQueueAppeal(selectedDenial, t)}>
                            {t.replace(/_/g, ' ')}
                          </button>
                        ))}
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
