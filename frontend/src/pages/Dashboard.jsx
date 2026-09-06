import React, { useState, useEffect } from 'react'
import { useNavigate } from 'react-router-dom'

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

/**
 * A stat card that navigates to the tab it summarises.
 *
 * Rendered as a real button so it is keyboard reachable and announced as
 * clickable, rather than a div with an onClick that only a mouse can find.
 */
function StatCard({ label, value, subtitle, to, tone, navigate }) {
  const clickable = Boolean(to)
  return (
    <div
      className={`stat-card${tone ? ' ' + tone : ''}${clickable ? ' stat-card-link' : ''}`}
      role={clickable ? 'button' : undefined}
      tabIndex={clickable ? 0 : undefined}
      onClick={clickable ? () => navigate(to) : undefined}
      onKeyDown={clickable ? (e) => {
        if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); navigate(to) }
      } : undefined}
      title={clickable ? `View ${label}` : undefined}
    >
      <div className="stat-label">{label}</div>
      <div className="stat-value">{value}</div>
      <div className="stat-subtitle">{subtitle}</div>
    </div>
  )
}

export default function Dashboard() {
  const navigate = useNavigate()
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
        <StatCard
          label="Total Claims"
          value={stats?.total_claims || 0}
          subtitle="All time"
          to="/claims"
          navigate={navigate}
        />
        <StatCard
          label="Denied Claims"
          value={stats?.denied_claims || 0}
          subtitle={`${stats?.pending_denials || 0} denial lines open`}
          tone="danger"
          to="/denials"
          navigate={navigate}
        />
        <StatCard
          label="Pending Appeals"
          value={stats?.pending_appeals || 0}
          subtitle={`${stats?.pending_worklist || 0} also in worklist`}
          tone="warning"
          to="/appeals"
          navigate={navigate}
        />
        <StatCard
          label="Total Denied"
          value={formatCurrency(stats?.total_denied || 0)}
          subtitle={`${formatCurrency(stats?.open_denied || 0)} still open`}
        />
      </div>

      {/* Priority Denials */}
      <div className="card" style={{ marginBottom: 24 }}>
        <div className="card-header">
          <h3>⚠️ Priority Denials — Appeal Deadlines</h3>
          <span style={{ color: 'var(--text-muted)', fontSize: '0.8rem' }}>
            Open denials whose filing deadline falls within 14 days, soonest first
          </span>
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
                <tr><td colSpan="7" style={{ textAlign: 'center', padding: 20, color: 'var(--text-muted)' }}>
                  Nothing due in the next 14 days.
                  {' '}Deadlines come from each payer's filing window — set them under Settings.
                </td></tr>
              ) : (
                priorityDenials.map((d, i) => (
                  <tr key={i} style={d.deadline_passed ? { background: 'var(--danger-light)' } : undefined}>
                    <td>{d.claim_number}</td>
                    <td>{d.patient_name || '—'}</td>
                    <td>{d.payer_name}</td>
                    <td>{d.cpt_code || '—'}</td>
                    <td>{formatCurrency(d.charge_amount)}</td>
                    <td style={{ whiteSpace: 'nowrap' }}>
                      {formatDate(d.appeal_deadline)}
                      {d.days_until_deadline !== null && d.days_until_deadline !== undefined && (
                        <div style={{
                          fontSize: '0.75rem', fontWeight: 600,
                          color: d.deadline_passed ? 'var(--danger)'
                               : d.days_until_deadline <= 3 ? 'var(--danger)'
                               : 'var(--warning-text)',
                        }}>
                          {d.deadline_passed
                            ? `${Math.abs(d.days_until_deadline)} days overdue`
                            : d.days_until_deadline === 0 ? 'due today'
                            : `${d.days_until_deadline} days left`}
                        </div>
                      )}
                    </td>
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
            <p style={{ color: 'var(--gray-500)' }}>
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
