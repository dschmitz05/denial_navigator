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
    { path: '/', label: 'Dashboard', icon: 'OV' },
    ...(can.ingestFiles() ? [{ path: '/upload', label: 'Upload', icon: 'UP' }] : []),
    { path: '/claims', label: 'Claims', icon: 'CL' },
    { path: '/denials', label: 'Denials', icon: 'DN' },
    { path: '/appeals', label: 'Appeals', icon: 'AP' },
    { path: '/worklist', label: 'Worklist', icon: 'WL' },
    { path: '/unanswered', label: 'No Response', icon: 'NR' },
    { path: '/overpayments', label: 'Overpayments', icon: 'OP' },
    { path: '/knowledge', label: 'Knowledge Base', icon: 'KB' },
    { path: '/insights', label: 'AI Insights', icon: 'AI' },
    ...(can.managePlaybooks() ? [{ path: '/playbooks', label: 'Playbooks', icon: 'PB' }] : []),
    ...(can.viewAudit() ? [{ path: '/audit', label: 'Audit Log', icon: 'AU' }] : []),
    ...(can.manageUsers() ? [{ path: '/users', label: 'Users', icon: 'US' }] : []),
    ...(can.manageOrganizations() ? [{ path: '/organizations', label: 'Organizations', icon: 'OR' }] : []),
    { path: '/profile', label: 'My Profile', icon: 'ME' },
    { path: '/settings', label: 'Settings', icon: 'ST' },
  ]

  return (
    <div className="app-container">
      {showNav && (
        <aside className="sidebar">
          <div className="sidebar-header">
            <div className="sidebar-logo" aria-hidden="true">DN</div>
            <div className="sidebar-product">
              <h1>Denial Navigator</h1>
              <p>Revenue cycle workspace</p>
            </div>
          </div>
          <nav className="sidebar-nav">
            <p className="sidebar-nav-label">Workspace</p>
            {navItems.map(item => (
              <Link
                key={item.path}
                to={item.path}
                className={location.pathname === item.path || (item.path !== '/' && location.pathname.startsWith(item.path)) ? 'active' : ''}
              >
                <span className="icon" aria-hidden="true">{item.icon}</span>
                <span>{item.label}</span>
              </Link>
            ))}
          </nav>
          <div className="sidebar-footer">
            <NotificationBell />
            <div className="sidebar-user">
              <div className="sidebar-avatar" aria-hidden="true">{(user?.full_name || user?.username || '?').slice(0, 1).toUpperCase()}</div>
              <div>
                <strong>{user?.full_name || user?.username}</strong>
                <span>{user?.role?.replace(/_/g, ' ')}</span>
              </div>
            </div>
            <button className="sidebar-signout" onClick={handleLogout}>Sign out <span aria-hidden="true">→</span></button>
          </div>
        </aside>
      )}
      <main className="main-content">{children}</main>
    </div>
  )
}

export default Layout
