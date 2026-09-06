import React, { useState, useEffect } from 'react'
import {
  actionHeadline, describeAction, describeDetails, describeOutcome,
  describeResource, describeSubject, technicalDetails,
} from '../lib/auditText'

const API_BASE = '/api/v1'

export default function Audit() {
  const [logs, setLogs] = useState([])
  const [stats, setStats] = useState(null)
  const [loading, setLoading] = useState(true)
  const [filter, setFilter] = useState('')
  const [resourceType, setResourceType] = useState('')
  const [search, setSearch] = useState('')
  const [page, setPage] = useState(0)
  const [username, setUsername] = useState('')
  const [actors, setActors] = useState([])
  const [expanded, setExpanded] = useState(null)
  // Rows per page. 25 is the default because the log is scanned, not read -
  // a reviewer is usually looking for one recent entry, not a wall of them.
  const [limit, setLimit] = useState(25)

  const loadLogs = async () => {
    setLoading(true)
    try {
      const params = new URLSearchParams({ limit: String(limit), offset: String(page * limit) })
      if (filter) params.set('action', filter)
      if (resourceType) params.set('resource_type', resourceType)
      if (username) params.set('username', username)

      const resp = await fetch(`${API_BASE}/audit?${params}`, {
        headers: { Authorization: `Bearer ${localStorage.getItem('auth_token')}` },
      })
      const data = await resp.json()
      setLogs(data)
    } catch (err) {
      console.error(err)
    } finally {
      setLoading(false)
    }
  }

  const loadStats = async () => {
    try {
      const resp = await fetch(`${API_BASE}/audit/stats`, {
        headers: { Authorization: `Bearer ${localStorage.getItem('auth_token')}` },
      })
      const data = await resp.json()
      setStats(data)
    } catch { /* ignore */ }
  }

  useEffect(() => { loadLogs() }, [page, filter, resourceType, username, limit])

  // The actor list comes from the log itself, so it can only offer names that
  // will match — and it includes services and anonymous callers, which have
  // no users row to enumerate from.
  useEffect(() => {
    fetch(`${API_BASE}/audit/actors`, {
      headers: { Authorization: `Bearer ${localStorage.getItem('auth_token')}` },
    })
      .then(r => (r.ok ? r.json() : []))
      .then(data => setActors(Array.isArray(data) ? data : []))
      .catch(() => setActors([]))
  }, [])
  useEffect(() => { loadStats() }, [])

  // Tone drives the outcome pill. A reviewer scanning hundreds of rows is
  // looking for the ones that were refused or broke, so those get the colour.

  // Service callers have no user row, so their name lives in details.
  const parsedUsername = (details) => {
    try {
      const d = typeof details === 'string' ? JSON.parse(details) : (details || {})
      return d.username && d.username !== 'anonymous' ? d.username : (d.username || null)
    } catch { return null }
  }

  const OUTCOME_TONES = {
    ok: { bg: 'var(--success-light)', fg: 'var(--success-text)' },
    blocked: { bg: 'var(--danger-light)', fg: 'var(--danger-text)' },
    warn: { bg: 'var(--warning-light)', fg: 'var(--warning-text)' },
    error: { bg: 'var(--danger-light)', fg: 'var(--danger-text)' },
  }

  const actionColors = {
    login: '#22c55e',
    login_failed: '#ef4444',
    create_user: '#3b82f6',
    update_user: '#f59e0b',
    reset_password: '#f59e0b',
    deactivate_user: '#ef4444',
    ingest_file: '#8b5cf6',
    generate_analysis: '#06b6d4',
    submit_appeal: '#10b981',
    assign_appeal: '#10b981',
    appeal_updated: '#f59e0b',
    view_claim: '#6366f1',
    view_denial: '#6366f1',
  }

  const formatDate = (d) => {
    if (!d) return '—'
    return new Date(d).toLocaleString('en-US', {
      month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit',
    })
  }

  const uniqueActions = [...new Set(logs.map(l => l.action))]
  const uniqueResources = [...new Set(logs.map(l => l.resource_type))]

  return (
    <div className="page-body">
      <div className="card">
        <div className="card-header">
          <h3>🔍 Audit Log</h3>
          <p style={{ fontSize: '0.85rem', color: 'var(--gray-500)', marginTop: 4 }}>
            Track all user actions across the system for HIPAA compliance
          </p>
        </div>
        <div className="card-body">
          {/* Stats */}
          {stats && (
            <div className="stats-grid" style={{ marginBottom: 24 }}>
              <div className="stat-card">
                <div className="stat-label">Total Entries</div>
                <div className="stat-value" style={{ fontSize: '1.4rem' }}>{stats.total_entries}</div>
              </div>
              <div className="stat-card">
                <div className="stat-label">Last 24h</div>
                <div className="stat-value" style={{ fontSize: '1.4rem', color: 'var(--primary)' }}>{stats.recent_24h}</div>
              </div>
            </div>
          )}

          {/* Filters */}
          <div style={{ display: 'flex', gap: 12, marginBottom: 20, flexWrap: 'wrap' }}>
            <select className="form-select" value={filter} onChange={e => { setFilter(e.target.value); setPage(0) }}>
              <option value="">All Actions</option>
              {uniqueActions.map(a => <option key={a} value={a}>{describeAction(a)}</option>)}
            </select>
            <select className="form-select" value={resourceType} onChange={e => { setResourceType(e.target.value); setPage(0) }}>
              <option value="">All Resources</option>
              {uniqueResources.map(r => <option key={r} value={r}>{describeResource(r)}</option>)}
            </select>
            <select className="form-select" value={username} onChange={e => { setUsername(e.target.value); setPage(0) }}>
              <option value="">All Users</option>
              {actors.map(a => (
                <option key={a.actor} value={a.actor}>
                  {a.actor.startsWith('service:')
                    ? `⚙ ${a.actor.slice(8)} (service)`
                    : a.actor === 'anonymous' ? '⚠ anonymous (not signed in)' : a.actor}
                  {` — ${a.entry_count}`}
                </option>
              ))}
            </select>
            {(filter || resourceType || username) && (
              <button className="btn" onClick={() => { setFilter(''); setResourceType(''); setUsername(''); setPage(0) }}>
                Clear
              </button>
            )}
          </div>

          {/* Table */}
          {loading ? (
            <p style={{ textAlign: 'center', padding: 40 }}>Loading...</p>
          ) : logs.length === 0 ? (
            <p style={{ textAlign: 'center', padding: 40, color: 'var(--gray-500)' }}>No audit entries found</p>
          ) : (
            <>
              <table>
                <thead>
                  <tr>
                    <th>Time</th>
                    <th>User</th>
                    <th>Action</th>
                    <th>Outcome</th>
                    <th>Record</th>
                    <th>What happened</th>
                    <th></th>
                  </tr>
                </thead>
                <tbody>
                  {logs.map(l => {
                    const outcome = describeOutcome(l.details)
                    const tone = outcome ? OUTCOME_TONES[outcome.tone] : null
                    const sentence = describeDetails(l.action, l.details)
                    const subject = describeSubject(l.details)
                    const tech = technicalDetails(l)
                    const open = expanded === l.id
                    return (
                      <React.Fragment key={l.id}>
                        <tr>
                          <td style={{ whiteSpace: 'nowrap' }}>{formatDate(l.created_at)}</td>
                          <td>
                            {/* actor is computed server-side from the same expression
                                the filter uses, so what is shown is what is selectable. */}
                            {l.username ? (
                              <span>{l.username}</span>
                            ) : (
                              <span style={{ color: 'var(--gray-400)', fontStyle: 'italic' }}>
                                {(l.actor || parsedUsername(l.details) || 'system').replace(/^service:/, '⚙ ')}
                              </span>
                            )}
                          </td>
                          <td>
                            <span style={{
                              display: 'inline-block',
                              padding: '2px 8px',
                              borderRadius: 4,
                              fontSize: '0.8rem',
                              fontWeight: 600,
                              // The tint derives from the text colour rather than being a
                              // second fixed hex. The unmapped fallback was grey text on a
                              // grey tint, which on a dark card was barely legible.
                              color: actionColors[l.action] || 'var(--text-muted)',
                              background: 'color-mix(in srgb, currentColor 16%, transparent)',
                            }}>
                              {actionHeadline(l.action, l.details)}
                            </span>
                            <div style={{ fontSize: '0.72rem', color: 'var(--gray-400)', marginTop: 2 }}>
                              {describeResource(l.resource_type)}
                            </div>
                          </td>
                          <td style={{ whiteSpace: 'nowrap' }}>
                            {outcome ? (
                              <span style={{
                                display: 'inline-block', padding: '2px 8px', borderRadius: 4,
                                fontSize: '0.75rem', fontWeight: 600,
                                background: tone.bg, color: tone.fg,
                              }}>{outcome.label}</span>
                            ) : <span style={{ color: 'var(--gray-300)' }}>—</span>}
                          </td>
                          <td style={{ fontSize: '0.85rem', whiteSpace: 'nowrap' }}>
                            {subject || <span style={{ color: 'var(--gray-300)' }}>—</span>}
                          </td>
                          <td style={{ fontSize: '0.85rem', color: 'var(--gray-700)' }}>
                            {sentence || <span style={{ color: 'var(--gray-300)' }}>—</span>}
                          </td>
                          <td style={{ whiteSpace: 'nowrap' }}>
                            {tech.length > 0 && (
                              <button className="btn btn-sm"
                                      title="Show the exact request behind this entry"
                                      onClick={() => setExpanded(open ? null : l.id)}>
                                {open ? 'Hide' : 'Details'}
                              </button>
                            )}
                          </td>
                        </tr>
                        {open && (
                          <tr>
                            {/* Nothing is discarded by the plain-English view - the
                                precise record is one click away. */}
                            <td colSpan="7" style={{ background: 'var(--gray-50)', fontSize: '0.8rem' }}>
                              <div style={{ display: 'grid', gridTemplateColumns: 'max-content 1fr', gap: '4px 16px', padding: '8px 4px' }}>
                                {tech.map(([k, v]) => (
                                  <React.Fragment key={k}>
                                    <div style={{ color: 'var(--gray-500)', fontWeight: 600 }}>{k}</div>
                                    <div style={{ fontFamily: 'monospace', wordBreak: 'break-all' }}>{v}</div>
                                  </React.Fragment>
                                ))}
                              </div>
                            </td>
                          </tr>
                        )}
                      </React.Fragment>
                    )
                  })}
                </tbody>
              </table>

              {/* Pagination + page size */}
              <div style={{
                display: 'flex', justifyContent: 'space-between', alignItems: 'center',
                gap: 12, marginTop: 20, flexWrap: 'wrap',
              }}>
                <div style={{ display: 'flex', alignItems: 'center', gap: 8, fontSize: '0.85rem', color: 'var(--text-muted)' }}>
                  <label htmlFor="audit-page-size">Rows per page</label>
                  <select
                    id="audit-page-size"
                    className="form-select"
                    style={{ width: 'auto', padding: '4px 8px' }}
                    value={limit}
                    onChange={e => {
                      // Back to page 1: page 4 of 25-row pages is not page 4
                      // of 100-row pages, and silently landing somewhere else
                      // in the log looks like data going missing.
                      setLimit(Number(e.target.value))
                      setPage(0)
                    }}
                  >
                    {[25, 50, 100].map(n => <option key={n} value={n}>{n}</option>)}
                  </select>
                  <span>· showing {logs.length}</span>
                </div>

                <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
                  <button className="btn" disabled={page === 0} onClick={() => setPage(p => p - 1)}>← Previous</button>
                  <span style={{ padding: '8px 16px' }}>Page {page + 1}</span>
                  {/* A short page means there is nothing after it. */}
                  <button className="btn" disabled={logs.length < limit} onClick={() => setPage(p => p + 1)}>Next →</button>
                </div>
              </div>
            </>
          )}
        </div>
      </div>
    </div>
  )
}
