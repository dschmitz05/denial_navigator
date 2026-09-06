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

  const login = async (username, password) => {
    const resp = await fetch(`${API_BASE}/auth/login`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username, password }),
    })
    const data = await resp.json()
    if (!resp.ok) throw new Error(data.detail || 'Login failed')
    setUser(data.user)
    setToken(data.access_token)
    localStorage.setItem('auth_token', data.access_token)
    return data.user
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
    <AuthContext.Provider value={{ user, loading, login, logout, hasRole, can }}>
      {children}
    </AuthContext.Provider>
  )
}

export function useAuth() {
  const ctx = useContext(AuthContext)
  if (!ctx) throw new Error('useAuth must be used within AuthProvider')
  return ctx
}
