import React, { useState, useEffect } from 'react'

const API_BASE = '/api/v1'

const money = (v) => new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD', maximumFractionDigits: 0 }).format(v || 0)
const pct = (v) => (v === null || v === undefined ? '—' : `${Math.round(v * 100)}%`)
const rating = (v) => (v === null || v === undefined ? '—' : `${v.toFixed(1)} / 5`)

/**
 * Is the AI earning its place?
 *
 * Every rate here divides by a denominator that is actually known. An outcome
 * only counts once someone recorded whether the claim was paid, so work still
 * in flight is excluded rather than counted as a failure — a rate that falls
 * because people are mid-appeal would be worse than no rate at all.
 */
export default function Insights() {
  const [data, setData] = useState(null)
  const [error, setError] = useState(null)

  useEffect(() => {
    fetch(`${API_BASE}/feedback/analytics`)
      .then(r => (r.ok ? r.json() : Promise.reject(new Error(`HTTP ${r.status}`))))
      .then(setData)
      .catch(err => setError(err.message))
  }, [])

  if (error) return <div className="page-body"><div className="callout callout-danger">Could not load: {error}</div></div>
  if (!data) return <div className="page-body"><p style={{ color: 'var(--text-muted)' }}>Loading…</p></div>

  const coverage = data.total_analyses ? data.total_feedback / data.total_analyses : 0
  const thin = data.outcome_known_count < 10

  return (
    <div className="page-body">
      {/* Say plainly when there is not enough data to conclude anything. A
          confident-looking percentage over four outcomes is misleading. */}
      {thin && (
        <div className="callout callout-warning" style={{ marginBottom: 16 }}>
          <strong>Not enough outcomes yet to draw conclusions.</strong> {data.outcome_known_count} of
          {' '}{data.total_feedback} ratings have a recorded result. Rates below are shown for
          completeness, not as a measurement.
        </div>
      )}

      <div className="stats-grid">
        <div className="stat-card">
          <div className="stat-label">Recommendations rated</div>
          <div className="stat-value">{data.total_feedback}</div>
          <div className="stat-subtitle">
            of {data.total_analyses ?? '—'} analyses · {pct(coverage)} reviewed
          </div>
        </div>
        <div className="stat-card success">
          <div className="stat-label">Paid after following the advice</div>
          <div className="stat-value">{pct(data.success_rate)}</div>
          <div className="stat-subtitle">
            {data.success_count} of {data.outcome_known_count} with a known outcome
          </div>
        </div>
        <div className="stat-card">
          <div className="stat-label">Accepted as written</div>
          <div className="stat-value">{pct(data.acceptance_rate)}</div>
          <div className="stat-subtitle">avg rating {rating(data.avg_rating)}</div>
        </div>
        <div className="stat-card success">
          <div className="stat-label">Recovered</div>
          <div className="stat-value">{money(data.money?.recovered)}</div>
          <div className="stat-subtitle">
            {money(data.money?.still_open)} still open · {money(data.money?.not_recovered)} not recovered
          </div>
        </div>
      </div>

      <Section title="By payer" hint="A model that reads one payer badly is a different problem from one that is uniformly mediocre.">
        <Table
          head={['Payer', 'Rated', 'Paid', 'Success', 'Recovered']}
          rows={(data.by_payer || []).map(p => [
            p.payer_name, p.feedback_count,
            `${p.paid_count}/${p.outcome_known}`, pct(p.success_rate), money(p.recovered_amount),
          ])}
        />
      </Section>

      <Section title="By recommended action" hint="Where the advice holds up, and where it does not.">
        <Table
          head={['Action', 'Rated', 'Paid', 'Success', 'Avg rating']}
          rows={(data.by_required_action || []).map(a => [
            (a.required_action || '').replace(/_/g, ' '), a.feedback_count,
            `${a.paid_count}/${a.outcome_known}`, pct(a.success_rate), rating(a.avg_rating),
          ])}
        />
      </Section>

      <Section title="By denial reason" hint="The reason codes worth trusting the model on.">
        <Table
          head={['CARC', 'Description', 'Rated', 'Success']}
          rows={(data.by_carc || []).map(c => [
            c.carc_code, (c.carc_description || '').slice(0, 60), c.feedback_count, pct(c.success_rate),
          ])}
        />
      </Section>

      <Section title="Over time" hint="A lifetime average cannot tell a model that improved from one that never worked.">
        <Table
          head={['Month', 'Rated', 'Known outcomes', 'Success', 'Avg rating']}
          rows={(data.trend || []).map(t => [
            new Date(t.month).toLocaleDateString('en-US', { month: 'short', year: 'numeric' }),
            t.feedback_count, t.outcome_known, pct(t.success_rate), rating(t.avg_rating),
          ])}
        />
      </Section>

      <p style={{ color: 'var(--text-muted)', fontSize: '0.85rem', marginTop: 20 }}>
        These numbers come from what billers record when they close an item. If coverage is
        low, the rates describe a small self-selected sample — rating a recommendation when
        you close it is what makes this page mean anything.
      </p>
    </div>
  )
}

function Section({ title, hint, children }) {
  return (
    <div className="card" style={{ marginTop: 24 }}>
      <div className="card-header">
        <h3>{title}</h3>
        <span style={{ color: 'var(--text-muted)', fontSize: '0.8rem' }}>{hint}</span>
      </div>
      <div className="table-container">{children}</div>
    </div>
  )
}

function Table({ head, rows }) {
  if (!rows.length) {
    return <p style={{ padding: 16, color: 'var(--text-muted)' }}>Nothing recorded yet.</p>
  }
  return (
    <table>
      <thead><tr>{head.map(h => <th key={h}>{h}</th>)}</tr></thead>
      <tbody>
        {rows.map((r, i) => (
          <tr key={i}>{r.map((cell, j) => <td key={j}>{cell}</td>)}</tr>
        ))}
      </tbody>
    </table>
  )
}
