import React, { useState } from 'react'
import { useAuth } from '../contexts/AuthContext'

type LoginResult =
  | { user: unknown }
  | { organizations: Array<{ id: string; name: string }> }
  | { mfa: 'totp_required' | 'enrollment_required'; mfaToken: string }

type Enrollment = { qr_svg: string; secret: string }

type LoginProps = {
  onLogin: (username: string, password: string, organizationId?: string) => Promise<LoginResult>
  onComplete?: () => void
}

export default function Login({ onLogin, onComplete }: LoginProps) {
  const { completeTotp, startEnrollment } = useAuth()
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [error, setError] = useState('')
  const [loading, setLoading] = useState(false)

  // 'password' -> 'code', or -> 'enroll' -> 'code' the first time.
  const [stage, setStage] = useState('password')
  const [mfaToken, setMfaToken] = useState<string | null>(null)
  const [code, setCode] = useState('')
  const [enrollment, setEnrollment] = useState<Enrollment | null>(null)
  const [organizations, setOrganizations] = useState<Array<{ id: string; name: string }>>([])
  const [organizationId, setOrganizationId] = useState('')

  const handleSubmit = async (e: React.FormEvent<HTMLFormElement>) => {
    e.preventDefault()
    setError('')
    setLoading(true)
    try {
      const result = await onLogin(username, password, organizationId || undefined)
      if ('organizations' in result) {
        setOrganizations(result.organizations)
        setStage('organization')
        return
      }
      if ('mfa' in result) {
        setMfaToken(result.mfaToken)
        if (result.mfa === 'enrollment_required') {
          setEnrollment(await startEnrollment(result.mfaToken))
          setStage('enroll')
        } else {
          setStage('code')
        }
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Invalid credentials')
    } finally {
      setLoading(false)
    }
  }

  const handleCode = async (e: React.FormEvent<HTMLFormElement>) => {
    e.preventDefault()
    setError('')
    setLoading(true)
    try {
      if (!mfaToken) throw new Error('Your sign-in session has expired. Please start again.')
      await completeTotp(mfaToken, code, { enrolling: stage === 'enroll' })
      onComplete?.()
    } catch (err) {
      setError(err instanceof Error ? err.message : 'That code is not valid')
      setCode('')
    } finally {
      setLoading(false)
    }
  }

  const restart = () => {
    setStage('password'); setMfaToken(null); setCode(''); setEnrollment(null)
    setError(''); setPassword('')
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
          <p style={{ color: 'var(--gray-500)', marginBottom: 32 }}>Healthcare Denial Management</p>

          {stage === 'organization' ? (
            <form onSubmit={handleSubmit}>
              <h2>Select organization</h2>
              <p>Choose where you want to work.</p>
              <select value={organizationId} onChange={(e) => setOrganizationId(e.target.value)} required>
                <option value="">Select an organization</option>
                {organizations.map((organization) => <option key={organization.id} value={organization.id}>{organization.name}</option>)}
              </select>
              <button className="btn btn-primary" type="submit" disabled={loading || !organizationId}>Continue</button>
            </form>
          ) : stage !== 'password' ? (
            <form onSubmit={handleCode}>
              {error && (
                <div style={{
                  background: 'var(--danger-light)', color: 'var(--danger-text)',
                  border: '1px solid var(--danger)', borderRadius: 8,
                  padding: 10, marginBottom: 20, fontSize: '0.9rem',
                }}>{error}</div>
              )}

              {stage === 'enroll' && enrollment && (
                <div style={{ marginBottom: 20, textAlign: 'left' }}>
                  <h3 style={{ fontSize: '1.05rem', marginBottom: 6 }}>Set up two-factor authentication</h3>
                  <p style={{ color: 'var(--text-muted)', fontSize: '0.85rem', marginBottom: 12 }}>
                    Your administrator requires a second factor on this account. Scan this
                    with an authenticator app, then enter the six-digit code it shows.
                  </p>
                  {/* Rendered by the server as inline SVG, so this works with
                      no internet and the secret never reaches a third party. */}
                  {/* Deliberately white in both themes: a QR code needs light quiet
                          zones and dark modules to scan reliably, so this one does
                          not follow the palette. */}
                  <div style={{ background: '#ffffff', padding: 12, borderRadius: 8, display: 'flex', justifyContent: 'center' }}
                       dangerouslySetInnerHTML={{ __html: enrollment.qr_svg }} />
                  <details style={{ marginTop: 10 }}>
                    <summary style={{ cursor: 'pointer', color: 'var(--text-muted)', fontSize: '0.85rem' }}>
                      Can't scan it?
                    </summary>
                    <p style={{ fontSize: '0.8rem', color: 'var(--text-muted)', marginTop: 6 }}>
                      Enter this key manually:
                    </p>
                    <code style={{
                      display: 'block', wordBreak: 'break-all', padding: 8,
                      background: 'var(--surface-alt)', borderRadius: 6, fontSize: '0.85rem',
                    }}>{enrollment.secret}</code>
                    <p style={{ fontSize: '0.78rem', color: 'var(--text-muted)', marginTop: 6 }}>
                      Shown once. If you lose the device, an administrator can reset it.
                    </p>
                  </details>
                </div>
              )}

              {stage === 'code' && (
                <p style={{ color: 'var(--text-muted)', marginBottom: 16 }}>
                  Enter the six-digit code from your authenticator app.
                </p>
              )}

              <div className="form-group" style={{ textAlign: 'left' }}>
                <label className="form-label">Authentication code</label>
                <input
                  className="form-input"
                  value={code}
                  onChange={e => setCode(e.target.value.replace(/\D/g, '').slice(0, 6))}
                  placeholder="000000"
                  inputMode="numeric"
                  autoComplete="one-time-code"
                  autoFocus
                  style={{ letterSpacing: '0.3em', fontSize: '1.2rem', textAlign: 'center' }}
                />
              </div>

              <button type="submit" className="btn btn-primary"
                      disabled={loading || code.length !== 6}
                      style={{ width: '100%', marginBottom: 10 }}>
                {loading ? 'Checking…' : stage === 'enroll' ? 'Confirm and sign in' : 'Sign in'}
              </button>
              <button type="button" className="btn" onClick={restart} style={{ width: '100%' }}>
                Back
              </button>
            </form>
          ) : (
          <form onSubmit={handleSubmit}>
            {error && (
              <div className="card" style={{
                background: 'var(--danger-light)', color: 'var(--danger-text)',
                border: '1px solid var(--danger)',
                marginBottom: 20,
                fontSize: '0.85rem',
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
          )}

          {/* Deliberately does not name the default credentials: the
                login page is reachable by anyone who can reach the app.
                Settings tells the administrator to change them. */}
        </div>
      </div>
    </div>
  )
}
