import React, { useState } from 'react'
import { useAuth } from '../contexts/AuthContext'

export default function Login({ onLogin }) {
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [error, setError] = useState('')
  const [loading, setLoading] = useState(false)

  const handleSubmit = async (e) => {
    e.preventDefault()
    setError('')
    setLoading(true)
    try {
      await onLogin(username, password)
    } catch (err) {
      setError(err.message || 'Invalid credentials')
    } finally {
      setLoading(false)
    }
  }

  return (
    <div style={{
      minHeight: '100vh',
      display: 'flex',
      alignItems: 'center',
      justifyContent: 'center',
      background: 'linear-gradient(135deg, #0f172a 0%, #1e293b 100%)',
    }}>
      <div className="card" style={{
        width: '100%',
        maxWidth: 420,
        margin: 20,
      }}>
        <div className="card-body" style={{ textAlign: 'center', padding: '40px 32px' }}>
          <h1 style={{ fontSize: '1.8rem', marginBottom: 8 }}>🧭 Denial Navigator</h1>
          <p style={{ color: '#6b7280', marginBottom: 32 }}>Healthcare Denial Management</p>

          <form onSubmit={handleSubmit}>
            {error && (
              <div className="card" style={{
                background: '#fef2f2',
                border: '1px solid #fca5a5',
                marginBottom: 20,
                fontSize: '0.85rem',
                color: '#dc2626',
              }}>
                {error}
              </div>
            )}

            <label style={{ display: 'block', textAlign: 'left', marginBottom: 6, fontWeight: 600, fontSize: '0.9rem' }}>
              Username
            </label>
            <input
              type="text"
              className="form-input"
              value={username}
              onChange={e => setUsername(e.target.value)}
              required
              autoFocus
              style={{ width: '100%', marginBottom: 20 }}
              placeholder="Enter your username"
            />

            <label style={{ display: 'block', textAlign: 'left', marginBottom: 6, fontWeight: 600, fontSize: '0.9rem' }}>
              Password
            </label>
            <input
              type="password"
              className="form-input"
              value={password}
              onChange={e => setPassword(e.target.value)}
              required
              style={{ width: '100%', marginBottom: 24 }}
              placeholder="Enter your password"
            />

            <button
              type="submit"
              className="btn btn-primary"
              style={{ width: '100%' }}
              disabled={loading}
            >
              {loading ? 'Signing in...' : 'Sign In'}
            </button>
          </form>

          <p style={{ marginTop: 24, fontSize: '0.8rem', color: '#6b7280' }}>
            Default: admin / admin123
          </p>
        </div>
      </div>
    </div>
  )
}
