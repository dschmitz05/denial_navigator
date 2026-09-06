import React, { createContext, useState, useContext, useEffect, useCallback } from 'react'

const API_BASE = '/api/v1'

const AuthContext = createContext(null)

export function AuthProvider({ children }) {
  const [user, setUser] = useState(null)
  const [loading, setLoading] = useState(true)
  const [token, setToken] = useState(() => localStorage.getItem('auth_token'))

  const fetchUser = useCallback(async (jwtToken) => {
    try {
      const resp = await fetch(`${API_BASE}/auth/me`, {
        headers: { Authorization: `Bearer ${jwtToken}` },
      })
      if (resp.ok) {
        const data = await resp.json()
        setUser(data)
        setToken(jwtToken)
        localStorage.setItem('auth_token', jwtToken)
      } else {
        logout()
      }
    } catch {
      logout()
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    if (token) {
      fetchUser(token)
    } else {
      setLoading(false)
    }
  }, [])

  const acceptSession = (data) => {
    setUser(data.user)
    setToken(data.access_token)
    localStorage.setItem('auth_token', data.access_token)
    return data.user
  }

  /**
   * Step one. Returns either a session, or what the account still needs.
   *
   * The mfa_token it hands back is NOT a session: the API confines it to the
   * second-factor endpoints, so holding it grants nothing on its own.
   */
  const login = async (username, password) => {
    const resp = await fetch(`${API_BASE}/auth/login`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username, password }),
    })
    const data = await resp.json()
    if (!resp.ok) throw new Error(data.detail || 'Login failed')

    if (data.status === 'totp_required' || data.status === 'enrollment_required') {
      return { mfa: data.status, mfaToken: data.mfa_token, username: data.username }
    }
    return { user: acceptSession(data) }
  }

  /** Step two: the six-digit code, or the code that confirms a new device. */
  const completeTotp = async (mfaToken, code, { enrolling = false } = {}) => {
    const path = enrolling ? '/auth/totp/confirm' : '/auth/login/totp'
    const resp = await fetch(`${API_BASE}${path}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${mfaToken}` },
      body: JSON.stringify({ code }),
    })
    const data = await resp.json()
    if (!resp.ok) throw new Error(data.detail || 'That code is not valid')
    return acceptSession(data)
  }

  /** Fetch the secret and QR for an account that has to enrol. */
  const startEnrollment = async (mfaToken) => {
    const resp = await fetch(`${API_BASE}/auth/totp/enroll`, {
      method: 'POST',
      headers: { Authorization: `Bearer ${mfaToken}` },
    })
    const data = await resp.json()
    if (!resp.ok) throw new Error(data.detail || 'Could not start enrolment')
    return data
  }

  const logout = () => {
    setUser(null)
    setToken(null)
    localStorage.removeItem('auth_token')
  }

  const hasRole = (roles) => !!user && roles.includes(user.role)

  // Mirrors PERMISSIONS in api-gateway/services/access.py. The server is the
  // authority — this only decides what to SHOW, so a user is not handed a
  // button that answers 403. Keep the two in step when either changes.
  const MANAGER_UP = ['billing_manager', 'rcm_director', 'admin']
  const can = {
    manageKnowledge: () => hasRole(MANAGER_UP),   // add/upload/archive policy docs
    ingestFiles: () => hasRole(MANAGER_UP),       // upload remittance files
    viewAudit: () => hasRole(MANAGER_UP),
    editClaims: () => hasRole(MANAGER_UP),
    manageUsers: () => hasRole(['admin']),
    assignWork: () => hasRole(MANAGER_UP),        // route work to other people
  }

  return (
    <AuthContext.Provider value={{ user, loading, login, completeTotp, startEnrollment, logout, hasRole, can }}>
      {children}
    </AuthContext.Provider>
  )
}

export function useAuth() {
  const ctx = useContext(AuthContext)
  if (!ctx) throw new Error('useAuth must be used within AuthProvider')
  return ctx
}
