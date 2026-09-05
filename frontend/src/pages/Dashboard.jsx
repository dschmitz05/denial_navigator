import React, { useState, useEffect } from 'react'

const API_BASE = '/api/v1'

function formatCurrency(value) {
  return new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD' }).format(value)
}

function formatPercent(value) {
  // null means "not enough data to say", which is different from 0%.
  if (value === null || value === undefined) return '—'
  return `${Math.round(value * 100)}%`
}

function formatRating(value) {
  return value === null || value === undefined ? '—' : `${value.toFixed(1)} / 5`
}

function formatDate(dateStr) {
  if (!dateStr) return '—'
  return new Date(dateStr).toLocaleDateString('en-US')
}

export default function Dashboard() {
  const [stats, setStats] = useState(null)
  const [priorityDenials, setPriorityDenials] = useState([])
  const [carcSummary, setCarcSummary] = useState([])
  const [feedback, setFeedback] = useState(null)
  const [loading, setLoading] = useState(true)

  useEffect(() => {
    Promise.all([
      fetch(`${API_BASE}/claims/dashboard/stats`).then(r => r.json()),
      fetch(`${API_BASE}/denials?priority=true&limit=5`).then(r => r.json()),
      fetch(`${API_BASE}/denials/bulk-carc`).then(r => r.json()),
      fetch(`${API_BASE}/feedback/analytics`).then(r => r.json()),
    ])
      .then(([statsData, priorityData, carcData, feedbackData]) => {
        setStats(statsData)
        setPriorityDenials(priorityData)
        setCarcSummary(carcData)
        setFeedback(feedbackData)
        setLoading(false)
      })
      .catch(err => {
        console.error('Dashboard load error:', err)
        setLoading(false)
      })
  }, [])

  if (loading) return <div className="page-body"><div className="loading">Loading dashboard</div></div>

  return (
    <div className="page-body">
      <div className="stats-grid">
        <div className="stat-card">
          <div className="stat-label">Total Claims</div>
          <div className="stat-value">{stats?.total_claims || 0}</div>
          <div className="stat-subtitle">All time</div>
        </div>
        <div className="stat-card danger">
          <div className="stat-label">Denied Claims</div>
          <div className="stat-value">{stats?.denied_claims || 0}</div>
          <div className="stat-subtitle">{stats?.pending_denials || 0} denial lines open</div>
        </div>
        <div className="stat-card warning">
          <div className="stat-label">Pending Appeals</div>
          <div className="stat-value">{stats?.pending_appeals || 0}</div>
          <div className="stat-subtitle">In queue</div>
        </div>
        <div className="stat-card">
          <div className="stat-label">Total Denied</div>
          <div className="stat-value">{formatCurrency(stats?.total_denied || 0)}</div>
          <div className="stat-subtitle">
            {formatCurrency(stats?.open_denied || 0)} still open
          </div>
        </div>
      </div>

      {/* Priority Denials */}
      <div className="card" style={{ marginBottom: 24 }}>
        <div className="card-header">
          <h3>⚠️ Priority Denials — Appeal Deadlines</h3>
        </div>
        <div className="table-container">
          <table>
            <thead>
              <tr>
                <th>Claim #</th>
                <th>Patient</th>
                <th>Payer</th>
                <th>CPT</th>
                <th>Amount</th>
                <th>Deadline</th>
                <th>Category</th>
              </tr>
            </thead>
            <tbody>
              {priorityDenials.length === 0 ? (
                <tr><td colSpan="7" style={{ textAlign: 'center', padding: 20 }}>No urgent denials</td></tr>
              ) : (
                priorityDenials.map((d, i) => (
                  <tr key={i}>
                    <td>{d.claim_number}</td>
                    <td>{d.patient_name || '—'}</td>
                    <td>{d.payer_name}</td>
                    <td>{d.cpt_code || '—'}</td>
                    <td>{formatCurrency(d.charge_amount)}</td>
                    <td>{formatDate(d.appeal_deadline)}</td>
                    <td>
                      <span className={`badge badge-${d.denial_category?.replace(' ', '-') || 'open'}`}>
                        {d.denial_category || 'Open'}
                      </span>
                    </td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>
      </div>

      {/* AI Performance — the feedback loop */}
      <div className="card" style={{ marginBottom: 24 }}>
        <div className="card-header">
          <h3>🎯 AI Recommendation Performance</h3>
        </div>
        <div className="card-body">
          {!feedback || feedback.total_feedback === 0 ? (
            <p style={{ color: '#6b7280' }}>
              No feedback recorded yet. Ratings and outcomes are captured when an appeal is
              resolved on the Appeals page — {feedback?.total_analyses ?? 0} analyses so far
              have no outcome logged against them.
            </p>
          ) : (
            <>
              <div className="stats-grid" style={{ marginBottom: 16 }}>
                <div className="stat-card">
                  <div className="stat-label">Reviewed</div>
                  <div className="stat-value">{formatPercent(feedback.coverage_rate)}</div>
                  <div className="stat-subtitle">{feedback.total_feedback} of {feedback.total_analyses} analyses</div>
                </div>
                <div className="stat-card">
                  <div className="stat-label">Accepted</div>
                  <div className="stat-value">{formatPercent(feedback.acceptance_rate)}</div>
                  <div className="stat-subtitle">biller agreed with the plan</div>
                </div>
                <div className="stat-card">
                  <div className="stat-label">Paid on Resubmit</div>
                  <div className="stat-value">{formatPercent(feedback.success_rate)}</div>
                  <div className="stat-subtitle">
                    {feedback.success_count} of {feedback.outcome_known_count} with a known outcome
                  </div>
                </div>
                <div className="stat-card">
                  <div className="stat-label">Avg Rating</div>
                  <div className="stat-value">{formatRating(feedback.avg_rating)}</div>
                  <div className="stat-subtitle">1 = unusable, 5 = resolved it</div>
                </div>
              </div>

              {feedback.by_required_action?.length > 0 && (
                <div className="table-container">
                  <table>
                    <thead>
                      <tr>
                        <th>Recommended Action</th>
                        <th>Reviewed</th>
                        <th>Avg Rating</th>
                        <th>Paid / Known</th>
                        <th>Success</th>
                      </tr>
                    </thead>
                    <tbody>
                      {feedback.by_required_action.map((r, i) => (
                        <tr key={i}>
                          <td>{r.required_action.replace(/_/g, ' ')}</td>
                          <td>{r.feedback_count}</td>
                          <td>{formatRating(r.avg_rating)}</td>
                          <td>{r.paid_count} / {r.outcome_known}</td>
                          <td>{formatPercent(r.success_rate)}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}

              {feedback.by_carc?.length > 0 && (
                <div className="table-container" style={{ marginTop: 16 }}>
                  <table>
                    <thead>
                      <tr>
                        <th>CARC</th>
                        <th>Description</th>
                        <th>Reviewed</th>
                        <th>Avg Rating</th>
                        <th>Success</th>
                      </tr>
                    </thead>
                    <tbody>
                      {feedback.by_carc.map((r, i) => (
                        <tr key={i}>
                          <td><strong>{r.carc_code}</strong></td>
                          <td>{r.carc_description}</td>
                          <td>{r.feedback_count}</td>
                          <td>{formatRating(r.avg_rating)}</td>
                          <td>{formatPercent(r.success_rate)}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}
            </>
          )}
        </div>
      </div>

      {/* Denial Summary by CARC */}
      <div className="card">
        <div className="card-header">
          <h3>📈 Denials by CARC Code</h3>
        </div>
        <div className="table-container">
          <table>
            <thead>
              <tr>
                <th>CARC Code</th>
                <th>Description</th>
                <th>Count</th>
                <th>Total Amount</th>
                <th>Avg Amount</th>
              </tr>
            </thead>
            <tbody>
              {carcSummary.length === 0 ? (
                <tr><td colSpan="5" style={{ textAlign: 'center', padding: 20 }}>No data yet</td></tr>
              ) : (
                carcSummary.map((c, i) => (
                  <tr key={i}>
                    <td><strong>{c.carc_code}</strong></td>
                    <td>{c.carc_description}</td>
                    <td>{c.denial_count}</td>
                    <td>{formatCurrency(c.total_denied_amount)}</td>
                    <td>{formatCurrency(c.avg_denial_amount)}</td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>
      </div>
    </div>
  )
}
