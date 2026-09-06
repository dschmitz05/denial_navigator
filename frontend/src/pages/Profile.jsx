import React, { useState } from 'react'
import { useAuth } from '../contexts/AuthContext'
import { useTheme } from '../contexts/ThemeContext'

const API_BASE = '/api/v1'
const MIN_PASSWORD_LENGTH = 8

export default function Profile() {
  const { user } = useAuth()
  const { theme, setTheme } = useTheme()
  const [form, setForm] = useState({ current_password: '', new_password: '', confirm: '' })
  const [saving, setSaving] = useState(false)
  const [notice, setNotice] = useState(null)

  // Checked here as well as on the server, so the mistake is caught before a
  // round trip - the server remains the authority.
  const problems = []
  if (form.new_password && form.new_password.length < MIN_PASSWORD_LENGTH) {
    problems.push(`New password must be at least ${MIN_PASSWORD_LENGTH} characters`)
  }
  if (form.confirm && form.new_password !== form.confirm) {
    problems.push('New password and confirmation do not match')
  }
  if (form.new_password && form.new_password === form.current_password) {
    problems.push('New password must be different from the current one')
  }
  const canSubmit = form.current_password && form.new_password && form.confirm && problems.length === 0

  const handleSubmit = async (e) => {
    e.preventDefault()
    if (!canSubmit) return
    setSaving(true)
    setNotice(null)
    try {
      const resp = await fetch(`${API_BASE}/auth/change-password`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          current_password: form.current_password,
          new_password: form.new_password,
        }),
      })
      const data = await resp.json()
      if (!resp.ok) throw new Error(data.detail || `Could not change password (HTTP ${resp.status})`)
      // The server invalidates every session opened before this change and
      // returns a replacement for this one; storing it keeps you signed in
      // here while signing out anywhere the old password was used.
      if (data.access_token) localStorage.setItem('auth_token', data.access_token)
      setNotice({
        error: false,
        text: 'Password changed. Any other session using the old password has been signed out.',
      })
      setForm({ current_password: '', new_password: '', confirm: '' })
    } catch (err) {
      setNotice({ error: true, text: err.message })
    }
    setSaving(false)
  }

  return (
    <div className="page-body">
      <div style={{ display: 'grid', gap: 20, gridTemplateColumns: 'repeat(auto-fit, minmax(340px, 1fr))', alignItems: 'start' }}>

        {/* Who you are */}
        <div className="card">
          <div className="card-header"><h3>👤 Your account</h3></div>
          <div className="card-body">
            <div className="detail-grid">
              <div className="detail-item">
                <div className="detail-label">Username</div>
                <div className="detail-value">{user?.username}</div>
              </div>
              <div className="detail-item">
                <div className="detail-label">Name</div>
                <div className="detail-value">{user?.full_name || '—'}</div>
              </div>
              <div className="detail-item">
                <div className="detail-label">Role</div>
                <div className="detail-value">
                  <span className="badge">{user?.role?.replace(/_/g, ' ')}</span>
                </div>
              </div>
            </div>
            <p style={{ color: 'var(--text-muted)', fontSize: '0.85rem', marginTop: 12 }}>
              Your role decides which tabs and actions you can use. Only an administrator
              can change it.
            </p>
          </div>
        </div>

        {/* Appearance */}
        <div className="card">
          <div className="card-header"><h3>🎨 Appearance</h3></div>
          <div className="card-body">
            <p style={{ color: 'var(--text-muted)', marginBottom: 12 }}>
              Applies immediately and is remembered on this device.
            </p>
            <div style={{ display: 'flex', gap: 10 }}>
              {[
                { value: 'light', label: '☀️ Light' },
                { value: 'dark', label: '🌙 Dark' },
              ].map(opt => (
                <button
                  key={opt.value}
                  type="button"
                  className={`btn ${theme === opt.value ? 'btn-primary' : ''}`}
                  aria-pressed={theme === opt.value}
                  onClick={() => setTheme(opt.value)}
                  style={{ flex: 1 }}
                >
                  {opt.label}{theme === opt.value ? ' ✓' : ''}
                </button>
              ))}
            </div>
          </div>
        </div>

        {/* Password */}
        <div className="card">
          <div className="card-header"><h3>🔒 Change password</h3></div>
          <div className="card-body">
            {notice && (
              <div style={{
                marginBottom: 12, padding: 10, borderRadius: 6,
                background: notice.error ? 'var(--danger-light)' : 'var(--success-light)',
                color: notice.error ? 'var(--danger-text)' : 'var(--success-text)',
              }}>{notice.text}</div>
            )}
            <form onSubmit={handleSubmit}>
              <div className="form-group">
                <label className="form-label">Current password</label>
                <input className="form-input" type="password" autoComplete="current-password"
                       value={form.current_password}
                       onChange={e => setForm({ ...form, current_password: e.target.value })} />
              </div>
              <div className="form-group">
                <label className="form-label">New password</label>
                <input className="form-input" type="password" autoComplete="new-password"
                       value={form.new_password}
                       onChange={e => setForm({ ...form, new_password: e.target.value })} />
              </div>
              <div className="form-group">
                <label className="form-label">Confirm new password</label>
                <input className="form-input" type="password" autoComplete="new-password"
                       value={form.confirm}
                       onChange={e => setForm({ ...form, confirm: e.target.value })} />
              </div>

              {problems.length > 0 && (
                <ul style={{ color: 'var(--danger)', fontSize: '0.85rem', margin: '0 0 12px 18px' }}>
                  {problems.map(p => <li key={p}>{p}</li>)}
                </ul>
              )}

              <button className="btn btn-primary" type="submit" disabled={!canSubmit || saving}>
                {saving ? 'Saving…' : 'Change password'}
              </button>
              <p style={{ color: 'var(--text-muted)', fontSize: '0.8rem', marginTop: 10 }}>
                You must enter your current password. The change is recorded in the audit log.
              </p>
            </form>
          </div>
        </div>
      </div>
    </div>
  )
}
