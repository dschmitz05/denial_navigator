import React, { useState, useEffect, useCallback } from 'react'

const API_BASE = '/api/v1'
const REFRESH_MS = 30000

const STATUS_LOOK = {
  ok:       { icon: '✅', word: 'Running',      tone: 'success' },
  degraded: { icon: '⚠️', word: 'Degraded',     tone: 'warning' },
  down:     { icon: '❌', word: 'Not reachable', tone: 'danger' },
}

export default function Settings() {
  const [health, setHealth] = useState(null)
  const [checking, setChecking] = useState(true)
  const [error, setError] = useState(null)

  const check = useCallback(async () => {
    setChecking(true)
    try {
      const resp = await fetch(`${API_BASE}/system/health`)
      if (!resp.ok) throw new Error(`Health check failed (HTTP ${resp.status})`)
      setHealth(await resp.json())
      setError(null)
    } catch (err) {
      // If the gateway itself cannot be reached, saying so is the status.
      setError(err.message)
      setHealth(null)
    }
    setChecking(false)
  }, [])

  useEffect(() => {
    check()
    const t = setInterval(check, REFRESH_MS)
    return () => clearInterval(t)
  }, [check])

  const overall = health?.overall
  const overallLook = STATUS_LOOK[overall] || STATUS_LOOK.down

  return (
    <div className="page-body">
      <div className="card">
        <div className="card-header"><h3>⚙️ Settings</h3></div>
        <div className="card-body">
          <div style={{ display: 'flex', alignItems: 'baseline', justifyContent: 'space-between', gap: 12, marginBottom: 16, flexWrap: 'wrap' }}>
            <h4 style={{ margin: 0 }}>
              Service Status
              {health && (
                <span style={{ marginLeft: 10, fontWeight: 500, color: 'var(--text-muted)', fontSize: '0.85rem' }}>
                  {overallLook.icon} {overall === 'ok' ? 'All services healthy'
                    : overall === 'degraded' ? 'Running, but AI features are unavailable'
                    : 'One or more essential services are down'}
                </span>
              )}
            </h4>
            <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
              <span style={{ color: 'var(--text-muted)', fontSize: '0.8rem' }}>
                {checking ? 'Checking…' : health ? `Checked ${new Date(health.checked_at).toLocaleTimeString()}` : ''}
              </span>
              <button className="btn btn-sm" onClick={check} disabled={checking}>↻ Refresh</button>
            </div>
          </div>

          {error && (
            <div className="callout callout-danger" style={{ marginBottom: 16 }}>
              Could not reach the API gateway: {error}
            </div>
          )}

          {!health && checking && (
            <p style={{ color: 'var(--text-muted)' }}>Checking services…</p>
          )}

          <div className="stats-grid">
            {(health?.services || []).map(svc => {
              const look = STATUS_LOOK[svc.status] || STATUS_LOOK.down
              return (
                <div key={svc.name} className={`stat-card ${look.tone}`}>
                  <div className="stat-label">
                    {svc.name}
                    {!svc.essential && (
                      <span style={{ color: 'var(--text-muted)', fontWeight: 400 }}> · optional</span>
                    )}
                  </div>
                  <div className="stat-value" style={{ fontSize: '1rem' }}>
                    {look.icon} {look.word}
                  </div>
                  {/* The reason matters more than the badge: "no model is
                      loaded" and "connection refused" need different fixes. */}
                  <div className="stat-subtitle" style={{ wordBreak: 'break-word' }}>
                    {svc.detail}{svc.latency_ms ? ` · ${svc.latency_ms}ms` : ''}
                  </div>
                </div>
              )
            })}
          </div>

          <h4 style={{ marginTop: 24, marginBottom: 16 }}>Quick Links</h4>
          <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
            <a href="/docs" className="btn" target="_blank" rel="noreferrer">📖 API Documentation (Swagger)</a>
            <a href="/redoc" className="btn" target="_blank" rel="noreferrer">📕 API Reference (ReDoc)</a>
            {health?.urls?.llama && (
              <a href={health.urls.llama} className="btn" target="_blank" rel="noreferrer">
                🤖 llama.cpp Server ({health.urls.llama.replace(/^https?:\/\//, '')})
              </a>
            )}
          </div>

          <h4 style={{ marginTop: 24, marginBottom: 16 }}>llama.cpp Model Management</h4>
          <p style={{ color: 'var(--gray-500)', marginBottom: 16 }}>
            Models are loaded on the remote servers below — this app does not load or
            unload them. The status cards above show what each server currently reports.
            To inspect them directly:
          </p>
          <div className="code-block">
            <span className="comment"># List the models the server currently has loaded</span>{'\n'}
            curl http://10.10.10.98:8080/v1/models{'\n\n'}
            <span className="comment"># Check the server is up</span>{'\n'}
            curl http://10.10.10.98:8080/health{'\n\n'}
            <span className="comment"># Embeddings come from Ollama, not the chat server</span>{'\n'}
            curl http://10.10.10.98:11434/api/tags
          </div>

          <h4 style={{ marginTop: 24, marginBottom: 16 }}>Security &amp; compliance</h4>

          <div className="callout callout-info" style={{ marginBottom: 12 }}>
            <strong>In place</strong>
            <ul>
              <li>All PHI stays inside this deployment — nothing is sent to a public LLM endpoint.</li>
              <li>Every API request is authenticated; unauthenticated callers are refused, not served.</li>
              <li>Role-based access: specialists work their own queue, managers assign and review,
                  only admins manage users.</li>
              <li>The audit log records every access, including refused ones, with user, IP and outcome.</li>
              <li>Passwords are stored as bcrypt hashes and changed through your profile page.</li>
            </ul>
          </div>

          <div className="callout callout-warning">
            <strong>Still on you before production</strong>
            <ul>
              <li>Change the default <code>admin</code> password and the PostgreSQL password.</li>
              <li>Terminate TLS in front of this app — tokens and PHI cross the network in the clear
                  over plain HTTP.</li>
              <li>Set up automated database backups and test restoring one.</li>
              <li>Decide how long audit entries are retained; nothing prunes them today.</li>
            </ul>
          </div>
        </div>
      </div>
    </div>
  )
}
