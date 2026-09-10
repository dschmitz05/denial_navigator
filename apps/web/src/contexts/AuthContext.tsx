import { createContext, useCallback, useContext, useEffect, useState, type ReactNode } from 'react'

const API_BASE = '/api/v1'
const MANAGER_UP = ['billing_manager', 'rcm_director', 'admin'] as const

export interface AuthUser {
  id?: string
  username?: string
  full_name?: string
  email?: string
  role?: string
  [key: string]: unknown
}

interface SessionResponse {
  user: AuthUser
  access_token: string
}

interface EnrollmentResponse {
  qr_svg: string
  secret: string
}

type LoginResult =
  | { user: AuthUser }
  | { mfa: 'totp_required' | 'enrollment_required'; mfaToken: string; username?: string }

interface AuthContextValue {
  user: AuthUser | null
  loading: boolean
  login: (username: string, password: string) => Promise<LoginResult>
  completeTotp: (mfaToken: string, code: string, options?: { enrolling?: boolean }) => Promise<AuthUser>
  startEnrollment: (mfaToken: string) => Promise<EnrollmentResponse>
  logout: () => void
  hasRole: (roles: readonly string[]) => boolean
  can: {
    manageKnowledge: () => boolean
    ingestFiles: () => boolean
    viewAudit: () => boolean
    managePlaybooks: () => boolean
    editClaims: () => boolean
    manageUsers: () => boolean
    assignWork: () => boolean
  }
}

const AuthContext = createContext<AuthContextValue | null>(null)

function errorDetail(value: unknown, fallback: string): string {
  if (value && typeof value === 'object' && 'detail' in value && typeof value.detail === 'string') {
    return value.detail
  }
  return fallback
}

export function AuthProvider({ children }: { children: ReactNode }) {
  const [user, setUser] = useState<AuthUser | null>(null)
  const [loading, setLoading] = useState(true)
  const [token, setToken] = useState<string | null>(() => localStorage.getItem('auth_token'))

  const logout = useCallback(() => {
    setUser(null)
    setToken(null)
    localStorage.removeItem('auth_token')
  }, [])

  const fetchUser = useCallback(async (jwtToken: string) => {
    try {
      const resp = await fetch(`${API_BASE}/auth/me`, {
        headers: { Authorization: `Bearer ${jwtToken}` },
      })
      if (resp.ok) {
        const data = await resp.json() as AuthUser
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
  }, [logout])

  useEffect(() => {
    if (token) void fetchUser(token)
    else setLoading(false)
  }, [fetchUser, token])

  const acceptSession = (data: SessionResponse): AuthUser => {
    setUser(data.user)
    setToken(data.access_token)
    localStorage.setItem('auth_token', data.access_token)
    return data.user
  }

  const login = async (username: string, password: string): Promise<LoginResult> => {
    const resp = await fetch(`${API_BASE}/auth/login`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username, password }),
    })
    const data = await resp.json() as Record<string, unknown>
    if (!resp.ok) throw new Error(errorDetail(data, 'Login failed'))

    if (data.status === 'totp_required' || data.status === 'enrollment_required') {
      if (typeof data.mfa_token !== 'string') throw new Error('The sign-in response is missing its MFA token')
      return {
        mfa: data.status,
        mfaToken: data.mfa_token,
        username: typeof data.username === 'string' ? data.username : undefined,
      }
    }
    return { user: acceptSession(data as unknown as SessionResponse) }
  }

  const completeTotp = async (
    mfaToken: string,
    code: string,
    { enrolling = false }: { enrolling?: boolean } = {},
  ): Promise<AuthUser> => {
    const path = enrolling ? '/auth/totp/confirm' : '/auth/login/totp'
    const resp = await fetch(`${API_BASE}${path}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${mfaToken}` },
      body: JSON.stringify({ code }),
    })
    const data = await resp.json()
    if (!resp.ok) throw new Error(errorDetail(data, 'That code is not valid'))
    return acceptSession(data as SessionResponse)
  }

  const startEnrollment = async (mfaToken: string): Promise<EnrollmentResponse> => {
    const resp = await fetch(`${API_BASE}/auth/totp/enroll`, {
      method: 'POST',
      headers: { Authorization: `Bearer ${mfaToken}` },
    })
    const data = await resp.json()
    if (!resp.ok) throw new Error(errorDetail(data, 'Could not start enrolment'))
    return data as EnrollmentResponse
  }

  const hasRole = (roles: readonly string[]) => Boolean(user && user.role && roles.includes(user.role))
  const can = {
    manageKnowledge: () => hasRole(MANAGER_UP),
    ingestFiles: () => hasRole(MANAGER_UP),
    viewAudit: () => hasRole([...MANAGER_UP, 'auditor']),
    managePlaybooks: () => hasRole(MANAGER_UP),
    editClaims: () => hasRole(MANAGER_UP),
    manageUsers: () => hasRole(['admin']),
    assignWork: () => hasRole(MANAGER_UP),
  }

  return (
    <AuthContext.Provider value={{ user, loading, login, completeTotp, startEnrollment, logout, hasRole, can }}>
      {children}
    </AuthContext.Provider>
  )
}

export function useAuth(): AuthContextValue {
  const context = useContext(AuthContext)
  if (!context) throw new Error('useAuth must be used within AuthProvider')
  return context
}
