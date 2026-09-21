import React, { useState, useEffect } from 'react'

const API_BASE = '/api/v1'

type OrganizationRecord = { id: string; slug: string; name: string; is_active: boolean; created_at: string; member_count: number }
type OrgForm = { slug: string; name: string; admin_username: string; admin_email: string; admin_password: string; admin_full_name: string }
type Message = { type: '' | 'success' | 'error'; text: string }

const emptyForm: OrgForm = { slug: '', name: '', admin_username: '', admin_email: '', admin_password: '', admin_full_name: '' }

export default function Organizations() {
  const [orgs, setOrgs] = useState<OrganizationRecord[]>([])
  const [loading, setLoading] = useState(true)
  const [showForm, setShowForm] = useState(false)
  const [form, setForm] = useState<OrgForm>(emptyForm)
  const [msg, setMsg] = useState<Message>({ type: '', text: '' })

  const loadOrgs = async () => {
    setLoading(true)
    try {
      const resp = await fetch(`${API_BASE}/organizations`, {
        headers: { Authorization: `Bearer ${localStorage.getItem('auth_token')}` },
      })
      const data = await resp.json()
      setOrgs(Array.isArray(data) ? data as OrganizationRecord[] : [])
    } catch (err) {
      console.error(err)
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => { loadOrgs() }, [])

  const handleSubmit = async (e: React.FormEvent<HTMLFormElement>) => {
    e.preventDefault()
    setMsg({ type: '', text: '' })
    try {
      const resp = await fetch(`${API_BASE}/organizations`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          Authorization: `Bearer ${localStorage.getItem('auth_token')}`,
        },
        body: JSON.stringify(form),
      })
      const data = await resp.json()
      if (!resp.ok) throw new Error(data.detail || 'Organization creation failed')
      setMsg({ type: 'success', text: `Organization "${form.name}" created; ${form.admin_username} must change their password at first sign-in` })
      setForm(emptyForm)
      setShowForm(false)
      loadOrgs()
    } catch (err) {
      setMsg({ type: 'error', text: err instanceof Error ? err.message : 'Organization creation failed' })
    }
  }

  return (
    <div className="page-body">
      <h1 className="page-title">Organizations</h1>
      <div className="card">
        <div className="card-header">
          <h3>🏢 Organizations</h3>
          <button className="btn btn-primary" onClick={() => setShowForm(!showForm)} style={{ marginLeft: 'auto' }}>
            {showForm ? '✕ Cancel' : '+ Add Organization'}
          </button>
        </div>
        <div className="card-body">
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

          {showForm && (
            <form onSubmit={handleSubmit} style={{
              padding: 20,
              background: 'var(--gray-50)',
              borderRadius: 8,
              marginBottom: 24,
              border: '1px solid var(--border)',
            }}>
              <h4 style={{ marginTop: 0, fontSize: '0.95rem', marginBottom: 4 }}>New Organization</h4>
              <p style={{ fontSize: '0.8rem', color: 'var(--gray-500)', marginTop: 0, marginBottom: 16 }}>
                A new organization needs at least one member to be reachable at all, so this also creates its
                first system_admin account.
              </p>
              <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(180px, 1fr))', gap: 12 }}>
                <div>
                  <label style={{ fontSize: '0.8rem', fontWeight: 600, display: 'block', marginBottom: 4 }}>Slug *</label>
                  <input className="form-input" required value={form.slug} onChange={e => setForm({ ...form, slug: e.target.value })} style={{ width: '100%' }} placeholder="acme-health" />
                </div>
                <div>
                  <label style={{ fontSize: '0.8rem', fontWeight: 600, display: 'block', marginBottom: 4 }}>Organization Name *</label>
                  <input className="form-input" required value={form.name} onChange={e => setForm({ ...form, name: e.target.value })} style={{ width: '100%' }} placeholder="Acme Health Partners" />
                </div>
                <div>
                  <label style={{ fontSize: '0.8rem', fontWeight: 600, display: 'block', marginBottom: 4 }}>Admin Username *</label>
                  <input className="form-input" required value={form.admin_username} onChange={e => setForm({ ...form, admin_username: e.target.value })} style={{ width: '100%' }} placeholder="jdoe" />
                </div>
                <div>
                  <label style={{ fontSize: '0.8rem', fontWeight: 600, display: 'block', marginBottom: 4 }}>Admin Email *</label>
                  <input className="form-input" type="email" required value={form.admin_email} onChange={e => setForm({ ...form, admin_email: e.target.value })} style={{ width: '100%' }} placeholder="jdoe@acmehealth.com" />
                </div>
                <div>
                  <label style={{ fontSize: '0.8rem', fontWeight: 600, display: 'block', marginBottom: 4 }}>Admin Password *</label>
                  <input className="form-input" type="password" required value={form.admin_password} onChange={e => setForm({ ...form, admin_password: e.target.value })} style={{ width: '100%' }} placeholder="Min 12 characters; they must change it at first sign-in" minLength={12} />
                </div>
                <div>
                  <label style={{ fontSize: '0.8rem', fontWeight: 600, display: 'block', marginBottom: 4 }}>Admin Full Name</label>
                  <input className="form-input" value={form.admin_full_name} onChange={e => setForm({ ...form, admin_full_name: e.target.value })} style={{ width: '100%' }} placeholder="Jane Doe" />
                </div>
              </div>
              <button type="submit" className="btn btn-primary" style={{ marginTop: 16 }}>Create Organization</button>
            </form>
          )}

          {loading ? (
            <p style={{ textAlign: 'center', padding: 40 }}>Loading...</p>
          ) : orgs.length === 0 ? (
            <p style={{ textAlign: 'center', padding: 40, color: 'var(--gray-500)' }}>No organizations found</p>
          ) : (
            <table>
              <thead>
                <tr>
                  <th>Name</th>
                  <th>Slug</th>
                  <th>Members</th>
                  <th>Status</th>
                  <th>Created</th>
                </tr>
              </thead>
              <tbody>
                {orgs.map(o => (
                  <tr key={o.id}>
                    <td>{o.name}</td>
                    <td>{o.slug}</td>
                    <td>{o.member_count}</td>
                    <td>{o.is_active ? 'Active' : 'Inactive'}</td>
                    <td>{new Date(o.created_at).toLocaleDateString()}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      </div>
    </div>
  )
}
