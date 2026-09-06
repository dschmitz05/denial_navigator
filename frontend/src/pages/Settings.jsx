import React, { useState, useEffect, useCallback } from 'react'
import { useAuth } from '../contexts/AuthContext'

const API_BASE = '/api/v1'
const REFRESH_MS = 30000

const STATUS_LOOK = {
  ok:       { icon: '✅', word: 'Running',      tone: 'success' },
  degraded: { icon: '⚠️', word: 'Degraded',     tone: 'warning' },
  down:     { icon: '❌', word: 'Not reachable', tone: 'danger' },
}

export default function Settings() {
  const { can } = useAuth()
  const canEdit = can.manageKnowledge()      // policy curation, same as documents
  const [windows, setWindows] = useState([])
  const [draft, setDraft] = useState({})
  const [savingWindow, setSavingWindow] = useState(null)
  const [windowsError, setWindowsError] = useState(null)
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

  const loadWindows = useCallback(async () => {
    try {
      const resp = await fetch(`${API_BASE}/denials/appeal-windows`)
      if (!resp.ok) throw new Error(`Could not load filing windows (HTTP ${resp.status})`)
      setWindows(await resp.json())
      setWindowsError(null)
    } catch (err) { setWindowsError(err.message) }
  }, [])

  useEffect(() => { loadWindows() }, [loadWindows])

  const defaultWindow = windows.find(w => w.is_default)?.appeal_window_days

  const saveWindow = async (payerName) => {
    const value = Number(draft[payerName] ?? windows.find(w => w.payer_name === payerName)?.appeal_window_days)
    if (!value || value < 1) { setWindowsError('Enter a number of days'); return }
    setSavingWindow(payerName)
    try {
      const resp = await fetch(`${API_BASE}/denials/appeal-windows`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ payer_name: payerName, appeal_window_days: value }),
      })
      const data = await resp.json()
      if (!resp.ok) throw new Error(data.detail || 'Could not save')
      // Say how many open denials moved: editing a window silently re-dating
      // the queue would be worse than not saying.
      setWindowsError(null)
      setDraft(d => { const { [payerName]: _, ...rest } = d; return rest })
      await loadWindows()
      alert(`Saved. ${data.denials_redated} open denial(s) re-dated.`)
    } catch (err) { setWindowsError(err.message) }
    setSavingWindow(null)
  }

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

          <h4 style={{ marginTop: 24, marginBottom: 8 }}>Appeal filing windows</h4>
          <p style={{ color: 'var(--text-muted)', marginBottom: 12, fontSize: '0.9rem' }}>
            How long you have to contest a denial, per payer. A remittance does not
            state this — it is the payer's own rule — so the deadlines the dashboard
            warns about are only as good as what is set here.
          </p>
          {windowsError && <div className="callout callout-danger" style={{ marginBottom: 12 }}>{windowsError}</div>}
          <div className="table-container" style={{ marginBottom: 24 }}>
            <table>
              <thead>
                <tr><th>Payer</th><th>Window</th><th>Claims</th><th></th></tr>
              </thead>
              <tbody>
                {windows.length === 0 ? (
                  <tr><td colSpan="4" style={{ padding: 12, color: 'var(--text-muted)' }}>Loading…</td></tr>
                ) : windows.map(w => (
                  <tr key={w.payer_name}>
                    <td>
                      {w.is_default ? <em>All other payers (default)</em> : w.payer_name}
                      {w.using_default && (
                        <span style={{ color: 'var(--text-muted)', fontSize: '0.8rem' }}> · using the default</span>
                      )}
                    </td>
                    <td style={{ whiteSpace: 'nowrap' }}>
                      <input
                        className="form-input"
                        type="number" min="1" max="3650"
                        style={{ width: 90, padding: '2px 6px' }}
                        value={draft[w.payer_name] ?? w.appeal_window_days ?? ''}
                        placeholder={String(defaultWindow ?? 90)}
                        disabled={!canEdit}
                        onChange={e => setDraft({ ...draft, [w.payer_name]: e.target.value })}
                      /> days
                    </td>
                    <td style={{ color: 'var(--text-muted)' }}>{w.claims_covered ?? 0}</td>
                    <td>
                      {canEdit && (
                        <button className="btn btn-sm" disabled={savingWindow === w.payer_name}
                                onClick={() => saveWindow(w.payer_name)}>
                          {savingWindow === w.payer_name ? 'Saving…' : 'Save'}
                        </button>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          {!canEdit && (
            <p style={{ color: 'var(--text-muted)', fontSize: '0.85rem', marginTop: -12, marginBottom: 24 }}>
              Filing windows are edited by managers and above.
            </p>
          )}

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
