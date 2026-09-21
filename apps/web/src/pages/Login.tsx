import React, { useState } from 'react'
import { Check, ShieldCheck, Lock } from 'lucide-react'
import { useAuth } from '../contexts/AuthContext'

type LoginResult =
  | { user: unknown }
  | { organizations: Array<{ id: string; name: string }> }
  | { mfa: 'totp_required' | 'enrollment_required'; mfaToken: string }
  | { passwordChangeToken: string }

type Enrollment = { qr_svg: string; secret: string }

type LoginProps = {
  onLogin: (username: string, password: string, organizationId?: string) => Promise<LoginResult>
  onComplete?: () => void
}

export default function Login({ onLogin, onComplete }: LoginProps) {
  const { completeTotp, completePasswordChange, startEnrollment } = useAuth()
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
  const [passwordChangeToken, setPasswordChangeToken] = useState<string | null>(null)
  const [newPassword, setNewPassword] = useState('')
  const [confirmPassword, setConfirmPassword] = useState('')

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
      if ('passwordChangeToken' in result) {
        setPasswordChangeToken(result.passwordChangeToken)
        setStage('change-password')
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
      const step = await completeTotp(mfaToken, code, { enrolling: stage === 'enroll' })
      if ('passwordChangeToken' in step) {
        setPasswordChangeToken(step.passwordChangeToken)
        setStage('change-password')
        return
      }
      onComplete?.()
    } catch (err) {
      setError(err instanceof Error ? err.message : 'That code is not valid')
      setCode('')
    } finally {
      setLoading(false)
    }
  }

  const handlePasswordChange = async (e: React.FormEvent<HTMLFormElement>) => {
    e.preventDefault()
    setError('')
    if (newPassword !== confirmPassword) {
      setError('The two new passwords do not match')
      return
    }
    setLoading(true)
    try {
      if (!passwordChangeToken) throw new Error('Your sign-in session has expired. Please start again.')
      await completePasswordChange(passwordChangeToken, password, newPassword)
      onComplete?.()
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Could not change the password')
    } finally {
      setLoading(false)
    }
  }

  const restart = () => {
    setStage('password'); setMfaToken(null); setCode(''); setEnrollment(null)
    setPasswordChangeToken(null); setNewPassword(''); setConfirmPassword('')
    setError(''); setPassword('')
  }

  return (
    <main className="login-shell">
      <section className="login-brand" aria-label="Denial Navigator overview">
        <div className="login-brand-content">
          <div className="login-mark" aria-hidden="true"><ShieldCheck size={22} strokeWidth={2.25} /></div>
          <p className="login-eyebrow">Denial Navigator</p>
          <h1>Revenue cycle workspace</h1>
          <p className="login-brand-copy">
            Prioritize recovery, coordinate the next action, and keep a clear record of every decision.
          </p>
          <div className="login-points" aria-label="Product capabilities">
            <div><span className="login-point-icon"><Check size={13} strokeWidth={3} /></span><span>One queue for claims, denials, and appeals</span></div>
            <div><span className="login-point-icon"><Check size={13} strokeWidth={3} /></span><span>Evidence-backed recommendations and deadlines</span></div>
            <div><span className="login-point-icon"><Check size={13} strokeWidth={3} /></span><span>Secure, role-based access to your workflow</span></div>
          </div>
        </div>
        <p className="login-brand-footer">Built for teams protecting every earned dollar.</p>
      </section>

      <section className="login-panel">
        <div className="login-card">
          <div className="login-mobile-mark" aria-hidden="true"><ShieldCheck size={20} strokeWidth={2.25} /></div>
          <header className="login-header">
            <p className="login-kicker">
              {stage === 'password' ? 'Welcome back' : 'Secure sign-in'}
            </p>
            <h2>
              {stage === 'organization' ? 'Choose your workspace'
                : stage === 'change-password' ? 'Create your password'
                  : stage === 'enroll' ? 'Protect your account'
                    : stage === 'code' ? 'Verify it’s you'
                      : 'Sign in to your workspace'}
            </h2>
            <p>
              {stage === 'organization' ? 'Select the organization you want to work in.'
                : stage === 'change-password' ? 'Your administrator set the initial password. Choose a private replacement to continue.'
                  : stage === 'enroll' ? 'Set up your authenticator app, then confirm the code it provides.'
                    : stage === 'code' ? 'Enter the six-digit code from your authenticator app.'
                      : 'Use your credentials to continue to Denial Navigator.'}
            </p>
          </header>

          {error && <div className="login-error" role="alert">{error}</div>}

          {stage === 'organization' ? (
            <form className="login-form" onSubmit={handleSubmit}>
              <label className="login-label" htmlFor="organization">Organization</label>
              <select id="organization" className="login-input" value={organizationId} onChange={(e) => setOrganizationId(e.target.value)} required autoFocus>
                <option value="">Select an organization</option>
                {organizations.map((organization) => <option key={organization.id} value={organization.id}>{organization.name}</option>)}
              </select>
              <button className="login-submit" type="submit" disabled={loading || !organizationId}>{loading ? 'Continuing…' : 'Continue'}</button>
              <button type="button" className="login-secondary" onClick={restart}>Back to sign in</button>
            </form>
          ) : stage === 'change-password' ? (
            <form className="login-form" onSubmit={handlePasswordChange}>
              <div className="login-note">Use at least 12 characters. Avoid your username and common passwords.</div>
              <label className="login-label" htmlFor="new-password">New password</label>
              <input id="new-password" className="login-input" type="password" value={newPassword} autoFocus autoComplete="new-password" minLength={12} required onChange={e => setNewPassword(e.target.value)} />
              <label className="login-label" htmlFor="confirm-password">Confirm new password</label>
              <input id="confirm-password" className="login-input" type="password" value={confirmPassword} autoComplete="new-password" minLength={12} required onChange={e => setConfirmPassword(e.target.value)} />
              <button type="submit" className="login-submit" disabled={loading}>{loading ? 'Saving…' : 'Set password and sign in'}</button>
              <button type="button" className="login-secondary" onClick={restart}>Back to sign in</button>
            </form>
          ) : stage !== 'password' ? (
            <form className="login-form" onSubmit={handleCode}>
              {stage === 'enroll' && enrollment && (
                <div className="login-enrollment">
                  <div className="login-qr" dangerouslySetInnerHTML={{ __html: enrollment.qr_svg }} />
                  <details>
                    <summary>Can’t scan the QR code?</summary>
                    <p>Enter this key in your authenticator app:</p>
                    <code>{enrollment.secret}</code>
                  </details>
                </div>
              )}
              <label className="login-label" htmlFor="authentication-code">Authentication code</label>
              <input id="authentication-code" className="login-input login-code" value={code} onChange={e => setCode(e.target.value.replace(/\D/g, '').slice(0, 6))} placeholder="000000" inputMode="numeric" autoComplete="one-time-code" autoFocus required />
              <button type="submit" className="login-submit" disabled={loading || code.length !== 6}>{loading ? 'Checking…' : stage === 'enroll' ? 'Confirm and sign in' : 'Sign in'}</button>
              <button type="button" className="login-secondary" onClick={restart}>Back to sign in</button>
            </form>
          ) : (
            <form className="login-form" onSubmit={handleSubmit}>
              <label className="login-label" htmlFor="username">Username</label>
              <input id="username" type="text" className="login-input" value={username} onChange={e => setUsername(e.target.value)} required autoFocus autoComplete="username" placeholder="Enter your username" />
              <label className="login-label" htmlFor="password">Password</label>
              <input id="password" type="password" className="login-input" value={password} onChange={e => setPassword(e.target.value)} required autoComplete="current-password" placeholder="Enter your password" />
              <button type="submit" className="login-submit" disabled={loading}>{loading ? 'Signing in…' : 'Sign in'}</button>
            </form>
          )}
          <p className="login-security"><Lock size={12} strokeWidth={2.25} aria-hidden="true" /> Your access is protected with secure authentication.</p>
        </div>
      </section>
    </main>
  )
}
