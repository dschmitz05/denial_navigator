import { useCallback, useEffect, useState } from 'react'

const API_BASE = '/api/v1'
const POLL_MS = 120_000

interface Notification {
  id: string
  title: string
  body: string
  read_at?: string | null
}

export default function NotificationBell() {
  const [items, setItems] = useState<Notification[]>([])
  const [open, setOpen] = useState(false)

  const load = useCallback(() => {
    void fetch(`${API_BASE}/notifications?limit=20`)
      .then(response => (response.ok ? response.json() : []))
      .then(data => setItems(Array.isArray(data) ? data as Notification[] : []))
      .catch(() => {})
  }, [])

  useEffect(() => {
    load()
    const timer = setInterval(load, POLL_MS)
    return () => clearInterval(timer)
  }, [load])

  const unread = items.filter(item => !item.read_at)
  const markAll = async () => {
    await fetch(`${API_BASE}/notifications/read-all`, { method: 'POST' }).catch(() => {})
    load()
  }

  return (
    <div style={{ padding: '0 16px 12px' }}>
      <button className="btn" style={{ width: '100%', textAlign: 'left' }} onClick={() => setOpen(value => !value)}>
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
            <p style={{ padding: 12, fontSize: '0.8rem', color: 'var(--text-muted)' }}>Nothing right now.</p>
          ) : (
            <>
              {items.map(item => (
                <div key={item.id} style={{
                  padding: 10, borderBottom: '1px solid var(--border)',
                  fontSize: '0.8rem', opacity: item.read_at ? 0.55 : 1,
                }}>
                  <div style={{ fontWeight: 600 }}>{item.title}</div>
                  <div style={{ color: 'var(--text-muted)', marginTop: 2 }}>{item.body}</div>
                </div>
              ))}
              {unread.length > 0 && (
                <button className="btn btn-sm" style={{ width: '100%', border: 'none' }} onClick={markAll}>
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
