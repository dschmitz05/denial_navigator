import { type ReactNode } from 'react'
import { Link, useLocation, useNavigate } from 'react-router-dom'
import { useAuth } from '../contexts/AuthContext'
import NotificationBell from './NotificationBell'

interface LayoutProps {
  children: ReactNode
  showNav?: boolean
}

interface NavigationItem {
  path: string
  label: string
  icon: string
}

function Layout({ children, showNav = true }: LayoutProps) {
  const location = useLocation()
  const navigate = useNavigate()
  const { user, logout, can } = useAuth()

  const handleLogout = () => {
    logout()
    navigate('/login')
  }

  const navItems: NavigationItem[] = [
    { path: '/', label: 'Dashboard', icon: '📊' },
    ...(can.ingestFiles() ? [{ path: '/upload', label: 'Upload', icon: '📁' }] : []),
    { path: '/claims', label: 'Claims', icon: '📋' },
    { path: '/denials', label: 'Denials', icon: '🚫' },
    { path: '/appeals', label: 'Appeals', icon: '⚖️' },
    { path: '/worklist', label: 'Worklist', icon: '🛠️' },
    { path: '/overpayments', label: 'Overpayments', icon: '💸' },
    { path: '/knowledge', label: 'Knowledge Base', icon: '📚' },
    { path: '/insights', label: 'AI Insights', icon: '📈' },
    ...(can.managePlaybooks() ? [{ path: '/playbooks', label: 'Playbooks', icon: '📘' }] : []),
    ...(can.viewAudit() ? [{ path: '/audit', label: 'Audit Log', icon: '🔍' }] : []),
    ...(can.manageUsers() ? [{ path: '/users', label: 'Users', icon: '👥' }] : []),
    { path: '/profile', label: 'My Profile', icon: '👤' },
    { path: '/settings', label: 'Settings', icon: '⚙️' },
  ]

  return (
    <div className="app-container">
      {showNav && (
        <aside className="sidebar">
          <div className="sidebar-header">
            <h1>🧭 OpenClaim Navigator</h1>
            <p>{user?.full_name || user?.username}</p>
            <span style={{ fontSize: '0.7rem', color: 'var(--gray-400)', textTransform: 'uppercase', letterSpacing: 1 }}>
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
          <NotificationBell />
          <div style={{ padding: 16, borderTop: '1px solid var(--sidebar-border)' }}>
            <button className="btn" onClick={handleLogout} style={{ width: '100%', textAlign: 'center' }}>
              🚪 Sign Out
            </button>
          </div>
        </aside>
      )}
      <main className="main-content">{children}</main>
    </div>
  )
}

export default Layout
