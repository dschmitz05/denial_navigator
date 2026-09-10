import React, { useState, useEffect } from 'react'
import { useNavigate, type NavigateFunction } from 'react-router-dom'

const API_BASE = '/api/v1'

type DashboardStats = Record<string, number | undefined>
type PriorityDenial = { claim_number?: string; patient_name?: string; payer_name?: string; cpt_code?: string; charge_amount?: number; appeal_deadline?: string; days_until_deadline?: number | null; deadline_passed?: boolean; denial_category?: string }
type CarcSummary = { carc_code?: string; carc_description?: string; denial_count?: number; total_denied_amount?: number; avg_denial_amount?: number }
type PayerSummary = { payer_name?: string; denial_count?: number; total_denied_amount?: number; avg_denial_amount?: number; overdue_count?: number; nearest_appeal_deadline?: string }
type RootCauseSummary = { root_cause?: string; denial_count?: number; total_denied_amount?: number; avg_denial_amount?: number }
type AgingBucket = { bucket?: string; denial_count?: number; total_denied_amount?: number; avg_denial_amount?: number }
type FinancialSummary = { denied_dollars?: number; recovered_dollars?: number; not_recovered_dollars?: number; unresolved_dollars?: number; recovery_rate?: number | null; outcome_known_count?: number }
type ResolutionTiming = { resolved_count?: number; average_resolution_days?: number | null; median_resolution_days?: number | null }
type FeedbackRow = { required_action?: string; feedback_count?: number; avg_rating?: number | null; paid_count?: number; outcome_known?: number; success_rate?: number | null; carc_code?: string; carc_description?: string }
type Feedback = { total_feedback?: number; total_analyses?: number; coverage_rate?: number | null; acceptance_rate?: number | null; success_rate?: number | null; success_count?: number; outcome_known_count?: number; avg_rating?: number | null; by_required_action?: FeedbackRow[]; by_carc?: FeedbackRow[] }

function formatCurrency(value?: number) {
  return new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD' }).format(value || 0)
}

function formatPercent(value?: number | null) {
  // null means "not enough data to say", which is different from 0%.
  if (value === null || value === undefined) return '—'
  return `${Math.round(value * 100)}%`
}

function formatRating(value?: number | null) {
  return value === null || value === undefined ? '—' : `${value.toFixed(1)} / 5`
}

function formatDate(dateStr?: string) {
  if (!dateStr) return '—'
  return new Date(dateStr).toLocaleDateString('en-US')
}

/**
 * A stat card that navigates to the tab it summarises.
 *
 * Rendered as a real button so it is keyboard reachable and announced as
 * clickable, rather than a div with an onClick that only a mouse can find.
 */
function StatCard({ label, value, subtitle, to, tone, navigate }: { label: string; value: React.ReactNode; subtitle: React.ReactNode; to?: string; tone?: string; navigate?: NavigateFunction }) {
  const clickable = Boolean(to)
  return (
    <div
      className={`stat-card${tone ? ' ' + tone : ''}${clickable ? ' stat-card-link' : ''}`}
      role={clickable ? 'button' : undefined}
      tabIndex={clickable ? 0 : undefined}
      onClick={clickable ? () => navigate!(to!) : undefined}
      onKeyDown={clickable ? (e: React.KeyboardEvent<HTMLDivElement>) => {
        if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); navigate!(to!) }
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
  const [stats, setStats] = useState<DashboardStats | null>(null)
  const [priorityDenials, setPriorityDenials] = useState<PriorityDenial[]>([])
  const [carcSummary, setCarcSummary] = useState<CarcSummary[]>([])
  const [payerSummary, setPayerSummary] = useState<PayerSummary[]>([])
  const [rootCauseSummary, setRootCauseSummary] = useState<RootCauseSummary[]>([])
  const [agingBuckets, setAgingBuckets] = useState<AgingBucket[]>([])
  const [financialSummary, setFinancialSummary] = useState<FinancialSummary | null>(null)
  const [resolutionTiming, setResolutionTiming] = useState<ResolutionTiming | null>(null)
  const [feedback, setFeedback] = useState<Feedback | null>(null)
  const [loading, setLoading] = useState(true)
  const exportClaims = async () => {
    const resp = await fetch(`${API_BASE}/claims/export.csv`, { headers: { Authorization: `Bearer ${localStorage.getItem('auth_token')}` } })
    if (!resp.ok) return
    const url = URL.createObjectURL(await resp.blob()); const a = document.createElement('a'); a.href = url; a.download = 'claims-export.csv'; a.click(); URL.revokeObjectURL(url)
  }

  useEffect(() => {
    Promise.all([
      fetch(`${API_BASE}/claims/dashboard/stats`).then(r => r.json()),
      fetch(`${API_BASE}/denials?priority=true&limit=5`).then(r => r.json()).then(d => Array.isArray(d) ? d : (d.items || [])),
      fetch(`${API_BASE}/denials/bulk-carc`).then(r => r.json()),
      fetch(`${API_BASE}/denials/by-payer`).then(r => r.json()),
      fetch(`${API_BASE}/denials/by-root-cause`).then(r => r.json()),
      fetch(`${API_BASE}/denials/aging-buckets`).then(r => r.json()),
      fetch(`${API_BASE}/denials/financial-summary`).then(r => r.json()),
      fetch(`${API_BASE}/denials/resolution-timing`).then(r => r.json()),
      fetch(`${API_BASE}/feedback/analytics`).then(r => r.json()),
    ])
      .then(([statsData, priorityData, carcData, payerData, rootCauseData, agingData, financialData, timingData, feedbackData]: [unknown, unknown, unknown, unknown, unknown, unknown, unknown, unknown, unknown]) => {
        setStats((statsData || {}) as DashboardStats)
        setPriorityDenials(Array.isArray(priorityData) ? priorityData as PriorityDenial[] : [])
        setCarcSummary(Array.isArray(carcData) ? carcData as CarcSummary[] : [])
        setPayerSummary(Array.isArray(payerData) ? payerData as PayerSummary[] : [])
        setRootCauseSummary(Array.isArray(rootCauseData) ? rootCauseData as RootCauseSummary[] : [])
        setAgingBuckets(Array.isArray(agingData) ? agingData as AgingBucket[] : [])
        setFinancialSummary((financialData || null) as FinancialSummary | null)
        setResolutionTiming((timingData || null) as ResolutionTiming | null)
        setFeedback((feedbackData || null) as Feedback | null)
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
      <div style={{ display: 'flex', justifyContent: 'flex-end', marginBottom: 12 }}>
        <button className="btn" onClick={exportClaims}>⇩ Export claims CSV</button>
      </div>
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

      <div className="card" style={{ marginBottom: 24 }}>
        <div className="card-header"><h3>💵 Denial Recovery</h3></div>
        <div className="card-body">
          <div className="stats-grid">
            <div className="stat-card"><div className="stat-label">Denied dollars</div><div className="stat-value">{formatCurrency(financialSummary?.denied_dollars)}</div><div className="stat-subtitle">all recorded denials</div></div>
            <div className="stat-card"><div className="stat-label">Recovered</div><div className="stat-value">{formatCurrency(financialSummary?.recovered_dollars)}</div><div className="stat-subtitle">paid after resubmission</div></div>
            <div className="stat-card"><div className="stat-label">Recovery rate</div><div className="stat-value">{formatPercent(financialSummary?.recovery_rate)}</div><div className="stat-subtitle">{financialSummary?.outcome_known_count || 0} known outcomes</div></div>
            <div className="stat-card"><div className="stat-label">Still unresolved</div><div className="stat-value">{formatCurrency(financialSummary?.unresolved_dollars)}</div><div className="stat-subtitle">no outcome recorded</div></div>
            <div className="stat-card"><div className="stat-label">Median resolution</div><div className="stat-value">{resolutionTiming?.median_resolution_days == null ? '—' : `${Math.round(resolutionTiming.median_resolution_days)} days`}</div><div className="stat-subtitle">{resolutionTiming?.resolved_count || 0} resolved denials</div></div>
          </div>
        </div>
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
                <tr><td colSpan={7} style={{ textAlign: 'center', padding: 20, color: 'var(--text-muted)' }}>
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

              {(feedback.by_required_action ?? []).length > 0 && (
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
                      {(feedback.by_required_action ?? []).map((r, i) => (
                        <tr key={i}>
                          <td>{r.required_action?.replace(/_/g, ' ') || '—'}</td>
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

              {(feedback.by_carc ?? []).length > 0 && (
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
                      {(feedback.by_carc ?? []).map((r, i) => (
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
      <div className="card" style={{ marginBottom: 24 }}>
        <div className="card-header"><h3>🏥 Denials by Payer</h3></div>
        <div className="table-container">
          <table>
            <thead><tr><th>Payer</th><th>Open denials</th><th>Denied amount</th><th>Average</th><th>Overdue</th><th>Nearest deadline</th></tr></thead>
            <tbody>
              {payerSummary.length === 0 ? (
                <tr><td colSpan={6} style={{ textAlign: 'center', padding: 20 }}>No open denial data yet</td></tr>
              ) : payerSummary.map((payer, i) => (
                <tr key={i}>
                  <td><strong>{payer.payer_name}</strong></td>
                  <td>{payer.denial_count}</td>
                  <td>{formatCurrency(payer.total_denied_amount)}</td>
                  <td>{formatCurrency(payer.avg_denial_amount)}</td>
                  <td style={{ color: (payer.overdue_count || 0) > 0 ? 'var(--danger)' : undefined }}>{payer.overdue_count || 0}</td>
                  <td>{formatDate(payer.nearest_appeal_deadline)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>

      <div className="card" style={{ marginBottom: 24 }}>
        <div className="card-header"><h3>🧩 Denials by Root Cause</h3></div>
        <div className="table-container">
          <table>
            <thead><tr><th>Root cause</th><th>Open denials</th><th>Denied amount</th><th>Average</th></tr></thead>
            <tbody>
              {rootCauseSummary.length === 0 ? (
                <tr><td colSpan={4} style={{ textAlign: 'center', padding: 20 }}>No analyzed denial data yet</td></tr>
              ) : rootCauseSummary.map((cause, i) => (
                <tr key={i}>
                  <td>{cause.root_cause}</td>
                  <td>{cause.denial_count}</td>
                  <td>{formatCurrency(cause.total_denied_amount)}</td>
                  <td>{formatCurrency(cause.avg_denial_amount)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>

      <div className="card" style={{ marginBottom: 24 }}>
        <div className="card-header"><h3>⏳ Denial Aging</h3></div>
        <div className="table-container">
          <table>
            <thead><tr><th>Age</th><th>Open denials</th><th>Denied amount</th><th>Average</th></tr></thead>
            <tbody>
              {agingBuckets.length === 0 ? (
                <tr><td colSpan={4} style={{ textAlign: 'center', padding: 20 }}>No active denial data yet</td></tr>
              ) : agingBuckets.map((bucket, i) => (
                <tr key={i}>
                  <td><strong>{bucket.bucket}</strong></td>
                  <td>{bucket.denial_count}</td>
                  <td>{formatCurrency(bucket.total_denied_amount)}</td>
                  <td>{formatCurrency(bucket.avg_denial_amount)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>

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
                <tr><td colSpan={5} style={{ textAlign: 'center', padding: 20 }}>No data yet</td></tr>
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
