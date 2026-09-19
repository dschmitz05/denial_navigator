import { type ReactNode } from 'react'
import { BrowserRouter, Navigate, Route, Routes, useNavigate } from 'react-router-dom'
import { AuthProvider, useAuth } from './contexts/AuthContext'
import { ThemeProvider } from './contexts/ThemeContext'
import Layout from './components/Layout'
import Login from './pages/Login'
import Dashboard from './pages/Dashboard'
import Claims from './pages/Claims'
import Denials from './pages/Denials'
import Appeals from './pages/Appeals'
import Worklist from './pages/Worklist'
import KnowledgeBase from './pages/KnowledgeBase'
import Settings from './pages/Settings'
import Upload from './pages/Upload'
import Audit from './pages/Audit'
import Users from './pages/Users'
import Profile from './pages/Profile'
import Insights from './pages/Insights'
import Playbooks from './pages/Playbooks'

const MANAGER_UP = ['revenue_cycle_manager', 'system_admin'] as const

function ProtectedRoute({ children, roles }: { children: ReactNode; roles?: readonly string[] }) {
  const { user, loading, hasRole } = useAuth()
  if (loading) return <FullScreenLoading />
  if (!user) return <Navigate to="/login" replace />
  if (roles && !hasRole(roles)) {
    return (
      <Layout>
        <div className="page-body">
          <div className="card">
            <div className="card-body">
              <h3 style={{ marginBottom: 8 }}>🔒 Not available for your role</h3>
              <p style={{ color: 'var(--gray-500)' }}>
                Your role (<strong>{user.role?.replace(/_/g, ' ')}</strong>) does not have access to this page.
                Contact an administrator if you need it.
              </p>
            </div>
          </div>
        </div>
      </Layout>
    )
  }
  return <Layout>{children}</Layout>
}

function FullScreenLoading() {
  return <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', height: '100vh' }}>Loading...</div>
}

function LoginScreen() {
  const { login } = useAuth()
  const navigate = useNavigate()

  const handleLogin = async (username: string, password: string, organizationId?: string) => {
    const result = await login(username, password, organizationId)
    if ('organizations' in result) return result
    if ('mfa' in result) return result
    navigate('/')
    return result
  }

  return <Login onLogin={handleLogin} onComplete={() => navigate('/')} />
}

function AppContent() {
  const { user, loading } = useAuth()
  if (loading) return <FullScreenLoading />

  if (!user) {
    return (
      <Routes>
        <Route path="/login" element={<LoginScreen />} />
        <Route path="*" element={<Navigate to="/login" replace />} />
      </Routes>
    )
  }

  return (
    <Routes>
      <Route path="/" element={<ProtectedRoute><Dashboard /></ProtectedRoute>} />
      <Route path="/upload" element={<ProtectedRoute roles={MANAGER_UP}><Upload /></ProtectedRoute>} />
      <Route path="/claims" element={<ProtectedRoute><Claims /></ProtectedRoute>} />
      <Route path="/denials" element={<ProtectedRoute><Denials /></ProtectedRoute>} />
      <Route path="/appeals" element={<ProtectedRoute><Appeals /></ProtectedRoute>} />
      <Route path="/worklist" element={<ProtectedRoute><Worklist /></ProtectedRoute>} />
      <Route path="/knowledge" element={<ProtectedRoute><KnowledgeBase /></ProtectedRoute>} />
      <Route path="/insights" element={<ProtectedRoute><Insights /></ProtectedRoute>} />
      <Route path="/playbooks" element={<ProtectedRoute roles={MANAGER_UP}><Playbooks /></ProtectedRoute>} />
      <Route path="/audit" element={<ProtectedRoute roles={MANAGER_UP}><Audit /></ProtectedRoute>} />
      <Route path="/users" element={<ProtectedRoute roles={['system_admin', 'security_admin']}><Users /></ProtectedRoute>} />
      <Route path="/profile" element={<ProtectedRoute><Profile /></ProtectedRoute>} />
      <Route path="/settings" element={<ProtectedRoute><Settings /></ProtectedRoute>} />
      <Route path="*" element={<Navigate to="/" replace />} />
    </Routes>
  )
}

export default function App() {
  return (
    <BrowserRouter>
      <ThemeProvider>
        <AuthProvider>
          <AppContent />
        </AuthProvider>
      </ThemeProvider>
    </BrowserRouter>
  )
}
