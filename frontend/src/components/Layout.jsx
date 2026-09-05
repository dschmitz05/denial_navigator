import React from 'react'
import { Link, useLocation, useNavigate } from 'react-router-dom'
import { useAuth } from '../contexts/AuthContext'

function Layout({ children, showNav = true }) {
  const location = useLocation()
  const navigate = useNavigate()
  const { user, logout, hasRole } = useAuth()

  const handleLogout = () => {
    logout()
    navigate('/login')
  }

  const adminNavItems = [
    { path: '/', label: 'Dashboard', icon: '📊' },
    { path: '/upload', label: 'Upload', icon: '📁' },
    { path: '/claims', label: 'Claims', icon: '📋' },
    { path: '/denials', label: 'Denials', icon: '🚫' },
    { path: '/appeals', label: 'Appeals', icon: '⚖️' },
    { path: '/knowledge', label: 'Knowledge Base', icon: '📚' },
    { path: '/audit', label: 'Audit Log', icon: '🔍' },
    ...(hasRole(['admin']) ? [{ path: '/users', label: 'Users', icon: '👥' }] : []),
    { path: '/settings', label: 'Settings', icon: '⚙️' },
  ]

  const navItems = showNav ? adminNavItems : []

  return (
    <div className="app-container">
      {showNav && (
        <aside className="sidebar">
          <div className="sidebar-header">
            <h1>🧭 Denial Navigator</h1>
            <p>{user?.full_name || user?.username}</p>
            <span style={{ fontSize: '0.7rem', color: '#9ca3af', textTransform: 'uppercase', letterSpacing: 1 }}>
              {user?.role?.replace(/_/g, ' ')}
            </span>
          </div>
          <nav className="sidebar-nav">
            {navItems.map(item => (
              <Link
                key={item.path}
                to={item.path}
                className={location.pathname === item.path || (item.path !== '/' && location.pathname.startsWith(item.path)) ? 'active' : ''}
              >
                <span className="icon">{item.icon}</span>
                <span>{item.label}</span>
              </Link>
            ))}
          </nav>
          <div style={{ padding: 16, borderTop: '1px solid #e5e7eb' }}>
            <button className="btn" onClick={handleLogout} style={{ width: '100%', textAlign: 'center' }}>
              🚪 Sign Out
            </button>
          </div>
        </aside>
      )}

      <main className="main-content">
        {children}
      </main>
    </div>
  )
}

export default Layout
