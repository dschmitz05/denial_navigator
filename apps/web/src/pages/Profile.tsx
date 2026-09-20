import { type FormEvent, type ReactNode, useState } from 'react'
import { useAuth } from '../contexts/AuthContext'
import { useTheme } from '../contexts/ThemeContext'

const API_BASE = '/api/v1'
const MIN_PASSWORD_LENGTH = 8
type PasswordForm = { current_password: string; new_password: string; confirm: string }
type Notice = { error: boolean; text: string }
const emptyForm: PasswordForm = { current_password: '', new_password: '', confirm: '' }

export default function Profile() {
  const { user } = useAuth()
  const { theme, setTheme } = useTheme()
  const [form, setForm] = useState<PasswordForm>(emptyForm)
  const [saving, setSaving] = useState(false)
  const [notice, setNotice] = useState<Notice | null>(null)
  const problems: string[] = []
  if (form.new_password && form.new_password.length < MIN_PASSWORD_LENGTH) problems.push(`New password must be at least ${MIN_PASSWORD_LENGTH} characters`)
  if (form.confirm && form.new_password !== form.confirm) problems.push('New password and confirmation do not match')
  if (form.new_password && form.new_password === form.current_password) problems.push('New password must be different from the current one')
  const canSubmit = Boolean(form.current_password && form.new_password && form.confirm && problems.length === 0)

  const handleSubmit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (!canSubmit) return
    setSaving(true); setNotice(null)
    try {
      const response = await fetch(`${API_BASE}/auth/change-password`, {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ current_password: form.current_password, new_password: form.new_password }),
      })
      const data = await response.json() as { detail?: string; access_token?: string }
      if (!response.ok) throw new Error(data.detail || `Could not change password (HTTP ${response.status})`)
      if (data.access_token) localStorage.setItem('auth_token', data.access_token)
      setNotice({ error: false, text: 'Password changed. Any other session using the old password has been signed out.' })
      setForm(emptyForm)
    } catch (error) {
      setNotice({ error: true, text: error instanceof Error ? error.message : 'Could not change password' })
    } finally { setSaving(false) }
  }

  return <div className="page-body">
    <section className="workspace-heading">
      <div>
        <p>Personal settings</p>
        <h1>Keep your account secure.</h1>
        <span>Manage your password, session, and workspace preferences in one place.</span>
      </div>
    </section>
    <div style={{ display: 'grid', gap: 20, gridTemplateColumns: 'repeat(auto-fit, minmax(340px, 1fr))', alignItems: 'start' }}>
    <section className="card"><div className="card-header"><h3>👤 Your account</h3></div><div className="card-body"><div className="detail-grid">
      <Field label="Username" value={user?.username} /><Field label="Name" value={user?.full_name || '—'} />
      <Field label="Role" value={<span className="badge">{user?.role?.replace(/_/g, ' ')}</span>} />
      <Field label="Email" value={user?.email || '—'} fullWidth />
    </div><p style={{ color: 'var(--text-muted)', fontSize: '0.85rem', marginTop: 12 }}>Your role decides which tabs and actions you can use. Only an administrator can change it.</p></div></section>
    <section className="card"><div className="card-header"><h3>🎨 Appearance</h3></div><div className="card-body"><p style={{ color: 'var(--text-muted)', marginBottom: 12 }}>Applies immediately and is remembered on this device.</p><div style={{ display: 'flex', gap: 10 }}>
      {([{ value: 'light', label: '☀️ Light' }, { value: 'dark', label: '🌙 Dark' }] as const).map(option => <button key={option.value} type="button" className={`btn ${theme === option.value ? 'btn-primary' : ''}`} aria-pressed={theme === option.value} onClick={() => setTheme(option.value)} style={{ flex: 1 }}>{option.label}{theme === option.value ? ' ✓' : ''}</button>)}
    </div></div></section>
    <section className="card"><div className="card-header"><h3>🔒 Change password</h3></div><div className="card-body">
      {notice && <div style={{ marginBottom: 12, padding: 10, borderRadius: 6, background: notice.error ? 'var(--danger-light)' : 'var(--success-light)', color: notice.error ? 'var(--danger-text)' : 'var(--success-text)' }}>{notice.text}</div>}
      <form onSubmit={handleSubmit}>{(['current_password', 'new_password', 'confirm'] as const).map((field, index) => <div className="form-group" key={field}><label className="form-label">{index === 0 ? 'Current password' : index === 1 ? 'New password' : 'Confirm new password'}</label><input className="form-input" type="password" autoComplete={index === 0 ? 'current-password' : 'new-password'} value={form[field]} onChange={event => setForm({ ...form, [field]: event.target.value })} /></div>)}
      {problems.length > 0 && <ul style={{ color: 'var(--danger)', fontSize: '0.85rem', margin: '0 0 12px 18px' }}>{problems.map(problem => <li key={problem}>{problem}</li>)}</ul>}
      <button className="btn btn-primary" type="submit" disabled={!canSubmit || saving}>{saving ? 'Saving…' : 'Change password'}</button><p style={{ color: 'var(--text-muted)', fontSize: '0.8rem', marginTop: 10 }}>You must enter your current password. The change is recorded in the audit log.</p></form>
    </div></section>
  </div></div>
}

function Field({ label, value, fullWidth = false }: { label: string; value: ReactNode; fullWidth?: boolean }) {
  return <div className="detail-item" style={fullWidth ? { gridColumn: '1 / -1' } : undefined}><div className="detail-label">{label}</div><div className="detail-value" style={fullWidth ? { wordBreak: 'break-word' } : undefined}>{value}</div></div>
}
