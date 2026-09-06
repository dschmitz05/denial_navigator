import React from 'react'
import { BrowserRouter, Routes, Route, Navigate, useNavigate } from 'react-router-dom'
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

function ProtectedRoute({ children, roles }) {
  const { user, loading, hasRole } = useAuth()
  if (loading) return <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', height: '100vh' }}>Loading...</div>
  if (!user) return <Navigate to="/login" replace />
  // Hiding a nav link is presentation; this is the check that matters when
  // someone types the path. The API enforces the same rule independently.
  if (roles && !hasRole(roles)) {
    return (
      <Layout>
        <div className="page-body">
          <div className="card">
            <div className="card-body">
              <h3 style={{ marginBottom: 8 }}>🔒 Not available for your role</h3>
              <p style={{ color: 'var(--gray-500)' }}>
                Your role (<strong>{user.role?.replace(/_/g, ' ')}</strong>) does not have access
                to this page. Contact an administrator if you need it.
              </p>
            </div>
          </div>
        </div>
      </Layout>
    )
  }
  return <Layout>{children}</Layout>
}

const MANAGER_UP = ['billing_manager', 'rcm_director', 'admin']

function LoginScreen() {
  const { login } = useAuth()
  const navigate = useNavigate()

  const handleLogin = async (username, password) => {
    await login(username, password)
    navigate('/')
  }

  return <Login onLogin={handleLogin} />
}

function AppContent() {
  const { user, loading } = useAuth()

  // On a page refresh the token is validated asynchronously, so `user` is
  // briefly null even for a signed-in user. Rendering the logged-out routes
  // during that window fired <Navigate to="/login" replace />, which REWROTE
  // the URL - so /claims became /login, and once auth resolved, /login was not
  // a route in the tree below and fell through to "*" -> the dashboard.
  // That is why refreshing any tab landed on Dashboard. Wait for the answer.
  if (loading) {
    return (
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', height: '100vh' }}>
        Loading...
      </div>
    )
  }

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
      <Route path="/audit" element={<ProtectedRoute roles={MANAGER_UP}><Audit /></ProtectedRoute>} />
      <Route path="/users" element={<ProtectedRoute roles={['admin']}><Users /></ProtectedRoute>} />
      <Route path="/profile" element={<ProtectedRoute><Profile /></ProtectedRoute>} />
      <Route path="/settings" element={<ProtectedRoute><Settings /></ProtectedRoute>} />
      <Route path="*" element={<Navigate to="/" replace />} />
    </Routes>
  )
}

function App() {
  return (
    <BrowserRouter>
      {/* Theme wraps auth: the login screen should honour the choice too. */}
      <ThemeProvider>
        <AuthProvider>
          <AppContent />
        </AuthProvider>
      </ThemeProvider>
    </BrowserRouter>
  )
}

export default App
