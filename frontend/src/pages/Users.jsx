import React, { useState, useEffect } from 'react'
import { useAuth } from '../contexts/AuthContext'

const API_BASE = '/api/v1'
const ROLES = ['billing_specialist', 'billing_manager', 'rcm_director', 'admin']

export default function Users() {
  const { user } = useAuth()
  const [users, setUsers] = useState([])
  const [loading, setLoading] = useState(true)
  const [showForm, setShowForm] = useState(false)
  const [editingUser, setEditingUser] = useState(null)
  const [search, setSearch] = useState('')
  const [roleFilter, setRoleFilter] = useState('')
  const [form, setForm] = useState({
    username: '', email: '', password: '', full_name: '', role: 'billing_specialist',
  })
  const [resetForm, setResetForm] = useState({ userId: '', password: '' })
  const [msg, setMsg] = useState({ type: '', text: '' })

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
      setUsers(data)
    } catch (err) {
      console.error(err)
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => { loadUsers() }, [])

  const handleSubmit = async (e) => {
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
      setMsg({ type: 'error', text: err.message })
    }
  }

  const handleResetPassword = async (e) => {
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
      setMsg({ type: 'error', text: err.message })
    }
  }

  const handleToggleActive = async (userId) => {
    try {
      const user = users.find(u => u.id === userId)
      const resp = await fetch(`${API_BASE}/users/${userId}`, {
        method: 'PATCH',
        headers: {
          'Content-Type': 'application/json',
          Authorization: `Bearer ${localStorage.getItem('auth_token')}`,
        },
        body: JSON.stringify({ is_active: !user.is_active }),
      })
      if (!resp.ok) throw new Error('Update failed')
      loadUsers()
    } catch (err) {
      setMsg({ type: 'error', text: err.message })
    }
  }

  const handleDelete = async (userId) => {
    if (!confirm('Deactivate this user?')) return
    try {
      const resp = await fetch(`${API_BASE}/users/${userId}`, {
        method: 'DELETE',
        headers: { Authorization: `Bearer ${localStorage.getItem('auth_token')}` },
      })
      if (!resp.ok) throw new Error('Delete failed')
      loadUsers()
    } catch (err) {
      setMsg({ type: 'error', text: err.message })
    }
  }

  const roleBadge = (role) => {
    const colors = {
      admin: '#dc2626',
      billing_manager: '#ea580c',
      rcm_director: '#2563eb',
      billing_specialist: '#6b7280',
    }
    return {
      display: 'inline-block',
      padding: '2px 8px',
      borderRadius: 4,
      fontSize: '0.75rem',
      fontWeight: 600,
      color: '#fff',
      background: colors[role] || '#6b7280',
    }
  }

  return (
    <div className="page-body">
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
              background: msg.type === 'success' ? '#f0fdf4' : '#fef2f2',
              border: `1px solid ${msg.type === 'success' ? '#86efac' : '#fca5a5'}`,
              color: msg.type === 'success' ? '#166534' : '#dc2626',
              fontSize: '0.85rem',
            }}>
              {msg.text}
            </div>
          )}

          {/* Add User Form */}
          {showForm && (
            <form onSubmit={handleSubmit} style={{
              padding: 20,
              background: '#f9fafb',
              borderRadius: 8,
              marginBottom: 24,
              border: '1px solid #e5e7eb',
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
                  <input className="form-input" type="password" required value={form.password} onChange={e => setForm({...form, password: e.target.value})} style={{ width: '100%' }} placeholder="Min 8 characters" />
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
          </div>

          {/* User Table */}
          {loading ? (
            <p style={{ textAlign: 'center', padding: 40 }}>Loading...</p>
          ) : users.length === 0 ? (
            <p style={{ textAlign: 'center', padding: 40, color: '#6b7280' }}>No users found</p>
          ) : (
            <table>
              <thead>
                <tr>
                  <th>Username</th>
                  <th>Email</th>
                  <th>Full Name</th>
                  <th>Role</th>
                  <th>Status</th>
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
                    <td style={{ fontSize: '0.85rem', color: '#6b7280' }}>
                      {u.last_login ? new Date(u.last_login).toLocaleDateString() : 'Never'}
                    </td>
                    <td style={{ whiteSpace: 'nowrap' }}>
                      {u.id !== user?.id && (
                        <div style={{ display: 'flex', gap: 4 }}>
                          <button className="btn btn-sm" onClick={() => setResetForm({...resetForm, userId: u.id})}>🔑</button>
                          <button className="btn btn-sm" onClick={() => handleToggleActive(u.id)}>
                            {u.is_active ? '⏸' : '▶️'}
                          </button>
                          <button className="btn btn-sm" onClick={() => handleDelete(u.id)} style={{ color: '#dc2626' }}>🗑</button>
                        </div>
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
    </div>
  )
}
