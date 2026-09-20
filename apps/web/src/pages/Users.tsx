import React, { useState, useEffect } from 'react'
import { useAuth } from '../contexts/AuthContext'

const API_BASE = '/api/v1'
const ROLES = ['system_admin', 'security_admin', 'revenue_cycle_manager', 'billing_specialist', 'coding_specialist', 'auditor', 'read_only']

type UserRecord = Record<string, any> & { id: string; username: string; email?: string; full_name?: string; role: string; is_active: boolean; totp_required?: boolean; totp_enrolled?: boolean; last_login?: string }
type UserForm = { username: string; email: string; password: string; full_name: string; role: string }
type Message = { type: '' | 'success' | 'error'; text: string }

export default function Users() {
  const { user } = useAuth()
  const [users, setUsers] = useState<UserRecord[]>([])
  const [loading, setLoading] = useState(true)
  const [showForm, setShowForm] = useState(false)
  const [search, setSearch] = useState('')
  const [roleFilter, setRoleFilter] = useState('')
  const [form, setForm] = useState<UserForm>({
    username: '', email: '', password: '', full_name: '', role: 'billing_specialist',
  })
  const [resetForm, setResetForm] = useState({ userId: '', password: '' })
  // Permanent deletion is a modal rather than a confirm() because the admin
  // has to type the username, and a browser confirm cannot take input.
  const [purgeTarget, setPurgeTarget] = useState<UserRecord | null>(null)
  const [purgeTyped, setPurgeTyped] = useState('')
  const [purging, setPurging] = useState(false)
  const [msg, setMsg] = useState<Message>({ type: '', text: '' })

  const loadUsers = async () => {
    setLoading(true)
    try {
      const params = new URLSearchParams({ limit: '200' })
      if (search) params.set('search', search)
      if (roleFilter) params.set('role', roleFilter)
      const resp = await fetch(`${API_BASE}/users?${params}`, {
        headers: { Authorization: `Bearer ${localStorage.getItem('auth_token')}` },
      })
      const data = await resp.json()
      setUsers(Array.isArray(data) ? data as UserRecord[] : [])
    } catch (err) {
      console.error(err)
    } finally {
      setLoading(false)
    }
  }

  // Re-query whenever a filter changes. With an empty dependency array the
  // selects only set state and nothing ever refetched, so picking a role
  // appeared to do nothing. The search box is debounced so it does not fire a
  // request per keystroke.
  useEffect(() => {
    const t = setTimeout(loadUsers, search ? 300 : 0)
    return () => clearTimeout(t)
  }, [roleFilter, search])

  const handleSubmit = async (e: React.FormEvent<HTMLFormElement>) => {
    e.preventDefault()
    setMsg({ type: '', text: '' })
    try {
      const resp = await fetch(`${API_BASE}/auth/register`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          Authorization: `Bearer ${localStorage.getItem('auth_token')}`,
        },
        body: JSON.stringify(form),
      })
      const data = await resp.json()
      if (!resp.ok) throw new Error(data.detail || 'Registration failed')
      setMsg({ type: 'success', text: `User ${form.username} created` })
      setForm({ username: '', email: '', password: '', full_name: '', role: 'billing_specialist' })
      setShowForm(false)
      loadUsers()
    } catch (err) {
      setMsg({ type: 'error', text: err instanceof Error ? err.message : 'Registration failed' })
    }
  }

  const handleResetPassword = async (e: React.FormEvent<HTMLFormElement>) => {
    e.preventDefault()
    try {
      const resp = await fetch(`${API_BASE}/users/${resetForm.userId}/password`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          Authorization: `Bearer ${localStorage.getItem('auth_token')}`,
        },
        body: JSON.stringify({ password: resetForm.password }),
      })
      if (!resp.ok) throw new Error('Reset failed')
      setMsg({ type: 'success', text: 'Password reset successfully' })
      setResetForm({ userId: '', password: '' })
    } catch (err) {
      setMsg({ type: 'error', text: err instanceof Error ? err.message : 'Reset failed' })
    }
  }

  const handleToggleActive = async (userId: string) => {
    const target = users.find(u => u.id === userId)
    if (!target) return

    // Deactivating blocks sign-in AND hands their open work back to the pool,
    // so it is worth confirming. Reactivating is harmless and is not.
    if (target.is_active && !confirm(
      `Deactivate ${target.username}?\n\n` +
      'They will not be able to sign in, and any open queue items assigned to ' +
      'them return to the unassigned pool so the work is not stranded.'
    )) return

    try {
      const resp = await fetch(`${API_BASE}/users/${userId}`, {
        method: 'PATCH',
        headers: {
          'Content-Type': 'application/json',
          Authorization: `Bearer ${localStorage.getItem('auth_token')}`,
        },
        body: JSON.stringify({ is_active: !target.is_active }),
      })
      const data = await resp.json().catch(() => ({}))
      if (!resp.ok) throw new Error(data.detail || 'Update failed')

      // The API reports how many items it released; saying so is how the
      // admin knows work was redistributed rather than lost.
      const released = Number(data.queue_items_released || 0)
      setMsg({
        type: 'success',
        text: target.is_active
          ? `${target.username} deactivated` +
            (released ? `; ${released} open item${released === 1 ? '' : 's'} returned to the pool` : '')
          : `${target.username} reactivated`,
      })
      loadUsers()
    } catch (err) {
      setMsg({ type: 'error', text: err instanceof Error ? err.message : 'Update failed' })
    }
  }

  const handleTotpPolicy = async (u: UserRecord, required: boolean) => {
    if (!required && !confirm(
      `Turn off two-factor authentication for ${u.username}?\n\n` +
      'Their authenticator is discarded. If you turn it back on later they will ' +
      'set up a new one.'
    )) return
    try {
      const resp = await fetch(`${API_BASE}/users/${u.id}/totp`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${localStorage.getItem('auth_token')}` },
        body: JSON.stringify({ required }),
      })
      const data = await resp.json()
      if (!resp.ok) throw new Error(data.detail || 'Update failed')
      setMsg({
        type: 'success',
        text: required
          ? `${u.username} must now use an authenticator` +
            (data.status === 'enrollment_pending' ? ' — they will set one up at their next sign-in' : '')
          : `Two-factor authentication turned off for ${u.username}`,
      })
      loadUsers()
    } catch (err) { setMsg({ type: 'error', text: err instanceof Error ? err.message : 'Update failed' }) }
  }

  const handleTotpReset = async (u: UserRecord) => {
    if (!confirm(
      `Reset ${u.username}'s authenticator?\n\n` +
      'Use this when they have lost or replaced their device. They will set up a ' +
      'new one at their next sign-in, two-factor stays required, and any session ' +
      'they currently have open is ended.'
    )) return
    try {
      const resp = await fetch(`${API_BASE}/users/${u.id}/totp/reset`, {
        method: 'POST',
        headers: { Authorization: `Bearer ${localStorage.getItem('auth_token')}` },
      })
      const data = await resp.json()
      if (!resp.ok) throw new Error(data.detail || 'Reset failed')
      setMsg({ type: 'success', text: `${u.username} will set up a new authenticator at their next sign-in` })
      loadUsers()
    } catch (err) { setMsg({ type: 'error', text: err instanceof Error ? err.message : 'Reset failed' }) }
  }

  const handlePurge = async () => {
    if (!purgeTarget || purgeTyped !== purgeTarget.username) return
    setPurging(true)
    try {
      const params = new URLSearchParams({ purge: 'true', confirm_username: purgeTarget.username })
      const resp = await fetch(`${API_BASE}/users/${purgeTarget.id}?${params}`, {
        method: 'DELETE',
        headers: { Authorization: `Bearer ${localStorage.getItem('auth_token')}` },
      })
      const data = await resp.json().catch(() => ({}))
      if (!resp.ok) throw new Error(data.detail || `Delete failed (HTTP ${resp.status})`)

      // Report the collateral rather than a bare success: the admin should
      // see what the deletion actually cost.
      const bits = []
      if (data.queue_items_released) bits.push(`${data.queue_items_released} queue item(s) returned to the pool`)
      if (data.closed_items_unattributed) bits.push(`${data.closed_items_unattributed} completed item(s) no longer show who worked them`)
      if (data.audit_entries_orphaned) bits.push(`${data.audit_entries_orphaned} audit entries kept, still showing the name`)
      setMsg({
        type: 'success',
        text: `${data.username} permanently deleted` + (bits.length ? ` — ${bits.join('; ')}` : ''),
      })
      setPurgeTarget(null)
      setPurgeTyped('')
      loadUsers()
    } catch (err) {
      setMsg({ type: 'error', text: err instanceof Error ? err.message : 'Delete failed' })
    }
    setPurging(false)
  }

  const roleBadge = (role: string) => {
    const colors: Record<string, string> = {
        system_admin: 'var(--danger)',
        security_admin: '#7c3aed',
        revenue_cycle_manager: '#2563eb',
        coding_specialist: '#ea580c',
      billing_specialist: 'var(--gray-500)',
    }
    return {
      display: 'inline-block',
      padding: '2px 8px',
      borderRadius: 4,
      fontSize: '0.75rem',
      fontWeight: 600,
      color: '#fff',   // always white: these pills use a saturated fill in both themes
      background: colors[role] || 'var(--gray-500)',
    }
  }

  return (
    <div className="page-body">
      <section className="workspace-heading">
        <div>
          <p>Access management</p>
          <h1>Give every teammate the right access.</h1>
          <span>Manage roles, account status, and security requirements with a clear audit trail.</span>
        </div>
      </section>
      <div className="card">
        <div className="card-header">
          <h3>👥 User Management</h3>
          <button className="btn btn-primary" onClick={() => setShowForm(!showForm)} style={{ marginLeft: 'auto' }}>
            {showForm ? '✕ Cancel' : '+ Add User'}
          </button>
        </div>
        <div className="card-body">
          {/* Message */}
          {msg.text && (
            <div style={{
              padding: '10px 16px',
              borderRadius: 8,
              marginBottom: 16,
              background: msg.type === 'success' ? 'var(--success-light)' : 'var(--danger-light)',
              border: `1px solid ${msg.type === 'success' ? 'var(--success)' : 'var(--danger)'}`,
              color: msg.type === 'success' ? 'var(--success-text)' : 'var(--danger-text)',
              fontSize: '0.85rem',
            }}>
              {msg.text}
            </div>
          )}

          {/* Add User Form */}
          {showForm && (
            <form onSubmit={handleSubmit} style={{
              padding: 20,
              background: 'var(--gray-50)',
              borderRadius: 8,
              marginBottom: 24,
              border: '1px solid var(--border)',
            }}>
              <h4 style={{ marginTop: 0, fontSize: '0.95rem', marginBottom: 16 }}>New User</h4>
              <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(180px, 1fr))', gap: 12 }}>
                <div>
                  <label style={{ fontSize: '0.8rem', fontWeight: 600, display: 'block', marginBottom: 4 }}>Username *</label>
                  <input className="form-input" required value={form.username} onChange={e => setForm({...form, username: e.target.value})} style={{ width: '100%' }} placeholder="jdoe" />
                </div>
                <div>
                  <label style={{ fontSize: '0.8rem', fontWeight: 600, display: 'block', marginBottom: 4 }}>Email *</label>
                  <input className="form-input" type="email" required value={form.email} onChange={e => setForm({...form, email: e.target.value})} style={{ width: '100%' }} placeholder="jdoe@company.com" />
                </div>
                <div>
                  <label style={{ fontSize: '0.8rem', fontWeight: 600, display: 'block', marginBottom: 4 }}>Password *</label>
                  <input className="form-input" type="password" required value={form.password} onChange={e => setForm({...form, password: e.target.value})} style={{ width: '100%' }} placeholder="Min 12 characters; they must change it at first sign-in" minLength={12} />
                </div>
                <div>
                  <label style={{ fontSize: '0.8rem', fontWeight: 600, display: 'block', marginBottom: 4 }}>Full Name</label>
                  <input className="form-input" value={form.full_name} onChange={e => setForm({...form, full_name: e.target.value})} style={{ width: '100%' }} placeholder="John Doe" />
                </div>
                <div>
                  <label style={{ fontSize: '0.8rem', fontWeight: 600, display: 'block', marginBottom: 4 }}>Role</label>
                  <select className="form-select" value={form.role} onChange={e => setForm({...form, role: e.target.value})} style={{ width: '100%' }}>
                    {ROLES.map(r => <option key={r} value={r}>{r.replace(/_/g, ' ').replace(/\b\w/g, c => c.toUpperCase())}</option>)}
                  </select>
                </div>
              </div>
              <button type="submit" className="btn btn-primary" style={{ marginTop: 16 }}>Create User</button>
            </form>
          )}

          {/* Search */}
          <div style={{ display: 'flex', gap: 12, marginBottom: 16 }}>
            <input
              className="form-input"
              placeholder="Search users..."
              value={search}
              onChange={e => setSearch(e.target.value)}
              style={{ maxWidth: 300 }}
            />
            <select className="form-select" value={roleFilter} onChange={e => setRoleFilter(e.target.value)}>
              <option value="">All Roles</option>
              {ROLES.map(r => <option key={r} value={r}>{r.replace(/_/g, ' ').replace(/\b\w/g, c => c.toUpperCase())}</option>)}
            </select>
            {(roleFilter || search) && (
              <button className="btn" onClick={() => { setRoleFilter(''); setSearch('') }}>Clear</button>
            )}
          </div>

          {/* User Table */}
          {loading ? (
            <p style={{ textAlign: 'center', padding: 40 }}>Loading...</p>
          ) : users.length === 0 ? (
            <p style={{ textAlign: 'center', padding: 40, color: 'var(--gray-500)' }}>No users found</p>
          ) : (
            <table>
              <thead>
                <tr>
                  <th>Username</th>
                  <th>Email</th>
                  <th>Full Name</th>
                  <th>Role</th>
                  <th>Status</th>
                  <th>2FA</th>
                  <th>Last Login</th>
                  <th>Actions</th>
                </tr>
              </thead>
              <tbody>
                {users.map(u => (
                  <tr key={u.id} style={{ opacity: u.is_active ? 1 : 0.5 }}>
                    <td>{u.username}</td>
                    <td>{u.email}</td>
                    <td>{u.full_name || '—'}</td>
                    <td><span style={roleBadge(u.role)}>{u.role.replace(/_/g, ' ').replace(/\b\w/g, c => c.toUpperCase())}</span></td>
                    <td>
                      <span className={`badge ${u.is_active ? 'badge-success' : 'badge-error'}`}>
                        {u.is_active ? 'Active' : 'Inactive'}
                      </span>
                    </td>
                    <td style={{ whiteSpace: 'nowrap' }}>
                      {/* Three distinct states, because they need different
                          actions: off, required but not yet set up, and in use. */}
                      {!u.totp_required ? (
                        <span style={{ color: 'var(--gray-400)', fontSize: '0.85rem' }}>Off</span>
                      ) : u.totp_enrolled ? (
                        <span className="badge badge-success">Enrolled</span>
                      ) : (
                        <span className="badge badge-queued" title="Will set up an authenticator at next sign-in">
                          Pending setup
                        </span>
                      )}
                    </td>
                    <td style={{ fontSize: '0.85rem', color: 'var(--gray-500)' }}>
                      {u.last_login ? new Date(u.last_login).toLocaleDateString() : 'Never'}
                    </td>
                    <td style={{ whiteSpace: 'nowrap' }}>
                      {u.id !== user?.id ? (
                        <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap' }}>
                          <button className="btn btn-sm"
                                  title="Set a new password for this user"
                                  onClick={() => setResetForm({ ...resetForm, userId: u.id })}>
                            Reset password
                          </button>
                          {/* One action, not two. The trash button that used to
                              sit here called DELETE, which is a soft delete -
                              its own prompt said "Deactivate this user?" - so
                              it did exactly what this button does. */}
                          <button className="btn btn-sm"
                                  style={u.is_active ? { color: 'var(--danger)' } : undefined}
                                  title={u.is_active
                                    ? 'Block sign-in and return their open queue items to the pool'
                                    : 'Allow this user to sign in again'}
                                  onClick={() => handleToggleActive(u.id)}>
                            {u.is_active ? 'Deactivate' : 'Reactivate'}
                          </button>
                          {u.totp_required ? (
                            <>
                              {u.totp_enrolled && (
                                <button className="btn btn-sm"
                                        title="Their device was lost or replaced — let them set up a new authenticator"
                                        onClick={() => handleTotpReset(u)}>
                                  Reset 2FA
                                </button>
                              )}
                              <button className="btn btn-sm"
                                      title="Stop requiring a second factor for this account"
                                      onClick={() => handleTotpPolicy(u, false)}>
                                Disable 2FA
                              </button>
                            </>
                          ) : (
                            <button className="btn btn-sm"
                                    title="Require an authenticator app for this account"
                                    onClick={() => handleTotpPolicy(u, true)}>
                              Require 2FA
                            </button>
                          )}
                          <button className="btn btn-sm"
                                  style={{ color: 'var(--danger)' }}
                                  title="Erase the account permanently. Deactivate instead unless the account should never have existed."
                                  onClick={() => { setPurgeTarget(u); setPurgeTyped('') }}>
                            Delete permanently
                          </button>
                        </div>
                      ) : (
                        <span style={{ color: 'var(--gray-400)', fontSize: '0.85rem' }}>
                          your account
                        </span>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      </div>

      {/* Reset Password Modal */}
      {resetForm.userId && (
        <div className="modal-overlay" onClick={() => setResetForm({ userId: '', password: '' })}>
          <div className="modal" onClick={e => e.stopPropagation()} style={{ maxWidth: 400 }}>
            <div className="modal-header">
              <h3>Reset Password</h3>
              <button className="btn" onClick={() => setResetForm({ userId: '', password: '' })}>✕</button>
            </div>
            <form onSubmit={handleResetPassword}>
              <div className="card-body">
                <label style={{ fontSize: '0.85rem', fontWeight: 600, display: 'block', marginBottom: 6 }}>New Password</label>
                <input
                  className="form-input"
                  type="password"
                  required
                  value={resetForm.password}
                  onChange={e => setResetForm({...resetForm, password: e.target.value})}
                  autoFocus
                  style={{ width: '100%' }}
                  placeholder="Enter new password"
                />
              </div>
              <div className="modal-footer">
                <button type="button" className="btn" onClick={() => setResetForm({ userId: '', password: '' })}>Cancel</button>
                <button type="submit" className="btn btn-primary">Reset Password</button>
              </div>
            </form>
          </div>
        </div>
      )}

      {/* Permanent deletion. Typing the username is the point: it makes an
          accidental click impossible, and forces the admin to look at which
          account they are about to erase. */}
      {purgeTarget && (
        <div className="modal-overlay" onClick={() => setPurgeTarget(null)}>
          <div className="modal" onClick={e => e.stopPropagation()} style={{ maxWidth: 560 }}>
            <div className="modal-header">
              <h3>Permanently delete {purgeTarget.username}?</h3>
              <button className="btn" onClick={() => setPurgeTarget(null)}>✕</button>
            </div>
            <div className="modal-body">
              <div className="callout callout-danger" style={{ marginBottom: 16 }}>
                <strong>This cannot be undone.</strong>
                <ul>
                  <li>The account is erased. It is not recoverable.</li>
                  <li>Their open queue items return to the unassigned pool.</li>
                  <li>Completed work will no longer show who did it.</li>
                  <li>Their audit entries are kept and still show the username,
                      but no longer link to an account.</li>
                </ul>
              </div>

              <p style={{ marginBottom: 12, color: 'var(--text-muted)' }}>
                In almost every case <strong>Deactivate</strong> is the right choice — it
                blocks sign-in, releases their work, and keeps the record intact.
                Delete permanently only when the account should never have existed.
              </p>

              <div className="form-group">
                <label className="form-label">
                  Type <strong>{purgeTarget.username}</strong> to confirm
                </label>
                <input className="form-input" autoFocus
                       value={purgeTyped}
                       placeholder={purgeTarget.username}
                       onChange={e => setPurgeTyped(e.target.value)} />
              </div>
            </div>
            <div className="modal-footer" style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
              <button className="btn" onClick={() => setPurgeTarget(null)}>Cancel</button>
              <button className="btn btn-danger"
                      disabled={purgeTyped !== purgeTarget.username || purging}
                      onClick={handlePurge}>
                {purging ? 'Deleting…' : 'Delete permanently'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
