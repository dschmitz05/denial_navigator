import React, { useState } from 'react'
import { useAuth } from '../contexts/AuthContext'

const API_BASE = '/api/v1'

/**
 * The Owner column for a queue item.
 *
 * Managers and above get an inline picker; everyone else sees plain text.
 * Assignment is a supervisory act, and the server enforces that independently
 * (PATH_PERMISSIONS in services/access.py) - hiding the control here only
 * avoids handing a specialist a dropdown that answers 403.
 */
export default function AssigneeCell({ item, users, onAssigned }) {
  const { can } = useAuth()
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState(null)

  const label = item.assigned_username
    ? item.assigned_username
    : <span style={{ color: 'var(--gray-400)' }}>unassigned</span>

  if (!can.assignWork()) {
    return <td style={{ fontSize: '0.85rem' }}>{label}</td>
  }

  const handleChange = async (e) => {
    const value = e.target.value
    setSaving(true)
    setError(null)
    try {
      const resp = await fetch(`${API_BASE}/appeals/${item.id}/assign`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        // An empty selection means "return it to the pool", which is a real
        // instruction, so send an explicit null rather than omitting the key.
        body: JSON.stringify({ assigned_user_id: value || null }),
      })
      const data = await resp.json()
      if (!resp.ok) throw new Error(data.detail || `Assign failed (HTTP ${resp.status})`)
      onAssigned?.(data)
    } catch (err) {
      setError(err.message)
    }
    setSaving(false)
  }

  return (
    <td style={{ fontSize: '0.85rem', whiteSpace: 'nowrap' }}>
      <select
        className="form-select"
        style={{ fontSize: '0.8rem', padding: '2px 6px', maxWidth: 160 }}
        value={item.assigned_user_id || ''}
        disabled={saving}
        onClick={e => e.stopPropagation()}
        onChange={handleChange}
      >
        <option value="">— unassigned (pool) —</option>
        {users.map(u => (
          <option key={u.id} value={u.id}>
            {u.full_name || u.username}{u.role === 'billing_specialist' ? '' : ` (${u.role.replace(/_/g, ' ')})`}
          </option>
        ))}
      </select>
      {saving && <span style={{ marginLeft: 6, color: 'var(--gray-500)' }}>…</span>}
      {error && <div style={{ color: 'var(--danger)', fontSize: '0.75rem', marginTop: 2 }}>{error}</div>}
    </td>
  )
}

/** Fetch the users a manager may assign to. Returns [] for everyone else. */
export function useAssignableUsers(enabled) {
  const [users, setUsers] = useState([])
  React.useEffect(() => {
    if (!enabled) return
    fetch(`${API_BASE}/users/assignable`)
      .then(r => (r.ok ? r.json() : []))
      .then(data => setUsers(Array.isArray(data) ? data : []))
      .catch(() => setUsers([]))
  }, [enabled])
  return users
}
