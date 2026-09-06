import React, { useState, useEffect, useCallback } from 'react'

const API_BASE = '/api/v1'
const POLL_MS = 120000

/**
 * Unread notifications, in the sidebar.
 *
 * Filing deadlines only mattered to whoever opened the dashboard; this puts
 * them where someone will see them without going looking. Polled rather than
 * pushed — a two-minute delay costs nothing against a deadline measured in
 * days, and it needs no websocket to keep alive.
 */
export default function NotificationBell() {
  const [items, setItems] = useState([])
  const [open, setOpen] = useState(false)

  const load = useCallback(() => {
    fetch(`${API_BASE}/notifications?limit=20`)
      .then(r => (r.ok ? r.json() : []))
      .then(d => setItems(Array.isArray(d) ? d : []))
      .catch(() => {})
  }, [])

  useEffect(() => {
    load()
    const t = setInterval(load, POLL_MS)
    return () => clearInterval(t)
  }, [load])

  const unread = items.filter(n => !n.read_at)

  const markAll = async () => {
    await fetch(`${API_BASE}/notifications/read-all`, { method: 'POST' }).catch(() => {})
    load()
  }

  return (
    <div style={{ padding: '0 16px 12px' }}>
      <button className="btn" style={{ width: '100%', textAlign: 'left' }}
              onClick={() => setOpen(o => !o)}>
        🔔 Alerts
        {unread.length > 0 && (
          <span style={{
            marginLeft: 8, padding: '1px 7px', borderRadius: 10,
            background: 'var(--danger)', color: 'var(--on-accent)',
            fontSize: '0.75rem', fontWeight: 700,
          }}>{unread.length}</span>
        )}
      </button>

      {open && (
        <div style={{
          marginTop: 8, background: 'var(--surface)', color: 'var(--text)',
          border: '1px solid var(--border)', borderRadius: 8,
          maxHeight: 320, overflowY: 'auto',
        }}>
          {items.length === 0 ? (
            <p style={{ padding: 12, fontSize: '0.8rem', color: 'var(--text-muted)' }}>
              Nothing right now.
            </p>
          ) : (
            <>
              {items.map(n => (
                <div key={n.id} style={{
                  padding: 10, borderBottom: '1px solid var(--border)',
                  fontSize: '0.8rem',
                  opacity: n.read_at ? 0.55 : 1,
                }}>
                  <div style={{ fontWeight: 600 }}>{n.title}</div>
                  <div style={{ color: 'var(--text-muted)', marginTop: 2 }}>{n.body}</div>
                </div>
              ))}
              {unread.length > 0 && (
                <button className="btn btn-sm" style={{ width: '100%', border: 'none' }}
                        onClick={markAll}>
                  Mark all read
                </button>
              )}
            </>
          )}
        </div>
      )}
    </div>
  )
}
