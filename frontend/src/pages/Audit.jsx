import React, { useState, useEffect } from 'react'

const API_BASE = '/api/v1'

export default function Audit() {
  const [logs, setLogs] = useState([])
  const [stats, setStats] = useState(null)
  const [loading, setLoading] = useState(true)
  const [filter, setFilter] = useState('')
  const [resourceType, setResourceType] = useState('')
  const [search, setSearch] = useState('')
  const [page, setPage] = useState(0)
  const limit = 100

  const loadLogs = async () => {
    setLoading(true)
    try {
      const params = new URLSearchParams({ limit: String(limit), offset: String(page * limit) })
      if (filter) params.set('action', filter)
      if (resourceType) params.set('resource_type', resourceType)

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

  useEffect(() => { loadLogs() }, [page, filter, resourceType])
  useEffect(() => { loadStats() }, [])

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
    view_claim: '#6366f1',
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
          <p style={{ fontSize: '0.85rem', color: '#6b7280', marginTop: 4 }}>
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
                <div className="stat-value" style={{ fontSize: '1.4rem', color: '#3b82f6' }}>{stats.recent_24h}</div>
              </div>
            </div>
          )}

          {/* Filters */}
          <div style={{ display: 'flex', gap: 12, marginBottom: 20, flexWrap: 'wrap' }}>
            <select className="form-select" value={filter} onChange={e => { setFilter(e.target.value); setPage(0) }}>
              <option value="">All Actions</option>
              {uniqueActions.map(a => <option key={a} value={a}>{a}</option>)}
            </select>
            <select className="form-select" value={resourceType} onChange={e => { setResourceType(e.target.value); setPage(0) }}>
              <option value="">All Resources</option>
              {uniqueResources.map(r => <option key={r} value={r}>{r}</option>)}
            </select>
          </div>

          {/* Table */}
          {loading ? (
            <p style={{ textAlign: 'center', padding: 40 }}>Loading...</p>
          ) : logs.length === 0 ? (
            <p style={{ textAlign: 'center', padding: 40, color: '#6b7280' }}>No audit entries found</p>
          ) : (
            <>
              <table>
                <thead>
                  <tr>
                    <th>Time</th>
                    <th>User</th>
                    <th>Action</th>
                    <th>Resource</th>
                    <th>Details</th>
                  </tr>
                </thead>
                <tbody>
                  {logs.map(l => (
                    <tr key={l.id}>
                      <td style={{ whiteSpace: 'nowrap' }}>{formatDate(l.created_at)}</td>
                      <td>
                        {l.username ? (
                          <span>{l.username}</span>
                        ) : (
                          <span style={{ color: '#9ca3af', fontStyle: 'italic' }}>system</span>
                        )}
                      </td>
                      <td>
                        <span style={{
                          display: 'inline-block',
                          padding: '2px 8px',
                          borderRadius: 4,
                          fontSize: '0.8rem',
                          fontWeight: 600,
                          background: (actionColors[l.action] || '#9ca3af') + '22',
                          color: actionColors[l.action] || '#6b7280',
                        }}>
                          {l.action}
                        </span>
                      </td>
                      <td style={{ fontSize: '0.85rem', color: '#6b7280' }}>{l.resource_type}</td>
                      <td style={{ fontSize: '0.8rem', color: '#6b7280', maxWidth: 200, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                        {l.details ? JSON.stringify(l.details).substring(0, 80) : '—'}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>

              {/* Pagination */}
              <div style={{ display: 'flex', justifyContent: 'center', gap: 8, marginTop: 20 }}>
                <button className="btn" disabled={page === 0} onClick={() => setPage(p => p - 1)}>← Previous</button>
                <span style={{ padding: '8px 16px' }}>Page {page + 1}</span>
                <button className="btn" onClick={() => setPage(p => p + 1)}>Next →</button>
              </div>
            </>
          )}
        </div>
      </div>
    </div>
  )
}
