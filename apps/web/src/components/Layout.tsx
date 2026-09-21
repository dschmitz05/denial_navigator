import { type ReactNode, useState } from 'react'
import { Link, useLocation, useNavigate } from 'react-router-dom'
import {
  LayoutDashboard, Upload as UploadIcon, FileText, ShieldAlert, Gavel, ListChecks,
  MailQuestion, Banknote, BookOpen, Sparkles, BookMarked, ScrollText, Users as UsersIcon,
  Building2, UserCircle, Settings as SettingsIcon, PanelLeftClose, PanelLeftOpen, LogOut,
  ShieldCheck, type LucideIcon,
} from 'lucide-react'
import { useAuth } from '../contexts/AuthContext'
import NotificationBell from './NotificationBell'

interface LayoutProps {
  children: ReactNode
  showNav?: boolean
}

interface NavigationItem {
  path: string
  label: string
  icon: LucideIcon
}

const SIDEBAR_COLLAPSED_KEY = 'dn-sidebar-collapsed'

function Layout({ children, showNav = true }: LayoutProps) {
  const location = useLocation()
  const navigate = useNavigate()
  const { user, logout, can } = useAuth()
  const [collapsed, setCollapsed] = useState(() => {
    try {
      return localStorage.getItem(SIDEBAR_COLLAPSED_KEY) === '1'
    } catch {
      return false
    }
  })

  const toggleCollapsed = () => {
    setCollapsed(current => {
      const next = !current
      try {
        localStorage.setItem(SIDEBAR_COLLAPSED_KEY, next ? '1' : '0')
      } catch {
        // Per-viewer convenience only; nothing breaks if it can't persist.
      }
      return next
    })
  }

  const handleLogout = () => {
    logout()
    navigate('/login')
  }

  const navItems: NavigationItem[] = [
    { path: '/', label: 'Dashboard', icon: LayoutDashboard },
    ...(can.ingestFiles() ? [{ path: '/upload', label: 'Upload', icon: UploadIcon }] : []),
    { path: '/claims', label: 'Claims', icon: FileText },
    { path: '/denials', label: 'Denials', icon: ShieldAlert },
    { path: '/appeals', label: 'Appeals', icon: Gavel },
    { path: '/worklist', label: 'Worklist', icon: ListChecks },
    { path: '/unanswered', label: 'No Response', icon: MailQuestion },
    { path: '/overpayments', label: 'Overpayments', icon: Banknote },
    { path: '/knowledge', label: 'Knowledge Base', icon: BookOpen },
    { path: '/insights', label: 'AI Insights', icon: Sparkles },
    ...(can.managePlaybooks() ? [{ path: '/playbooks', label: 'Playbooks', icon: BookMarked }] : []),
    ...(can.viewAudit() ? [{ path: '/audit', label: 'Audit Log', icon: ScrollText }] : []),
    ...(can.manageUsers() ? [{ path: '/users', label: 'Users', icon: UsersIcon }] : []),
    ...(can.manageOrganizations() ? [{ path: '/organizations', label: 'Organizations', icon: Building2 }] : []),
    { path: '/profile', label: 'My Profile', icon: UserCircle },
    { path: '/settings', label: 'Settings', icon: SettingsIcon },
  ]

  return (
    <div className="app-container">
      {showNav && (
        <aside className={`sidebar${collapsed ? ' sidebar-collapsed' : ''}`}>
          <div className="sidebar-header">
            <div className="sidebar-logo" aria-hidden="true"><ShieldCheck size={17} strokeWidth={2.25} /></div>
            {!collapsed && (
              <div className="sidebar-product">
                <h1>Denial Navigator</h1>
                <p>Revenue cycle workspace</p>
              </div>
            )}
            <button
              type="button"
              className="sidebar-collapse-toggle"
              onClick={toggleCollapsed}
              title={collapsed ? 'Expand sidebar' : 'Collapse sidebar'}
              aria-label={collapsed ? 'Expand sidebar' : 'Collapse sidebar'}
            >
              {collapsed ? <PanelLeftOpen size={16} /> : <PanelLeftClose size={16} />}
            </button>
          </div>
          <nav className="sidebar-nav">
            {!collapsed && <p className="sidebar-nav-label">Workspace</p>}
            {navItems.map(item => {
              const Icon = item.icon
              const active = location.pathname === item.path || (item.path !== '/' && location.pathname.startsWith(item.path))
              return (
                <Link
                  key={item.path}
                  to={item.path}
                  className={active ? 'active' : ''}
                  title={collapsed ? item.label : undefined}
                >
                  <span className="icon" aria-hidden="true"><Icon size={16} strokeWidth={2} /></span>
                  {!collapsed && <span>{item.label}</span>}
                </Link>
              )
            })}
          </nav>
          <div className="sidebar-footer">
            {!collapsed && <NotificationBell />}
            <div className="sidebar-user">
              <div className="sidebar-avatar" aria-hidden="true">{(user?.full_name || user?.username || '?').slice(0, 1).toUpperCase()}</div>
              {!collapsed && (
                <div>
                  <strong>{user?.full_name || user?.username}</strong>
                  <span>{user?.role?.replace(/_/g, ' ')}</span>
                </div>
              )}
            </div>
            <button className="sidebar-signout" onClick={handleLogout} title={collapsed ? 'Sign out' : undefined}>
              <LogOut size={15} strokeWidth={2} aria-hidden="true" />
              {!collapsed && <span>Sign out</span>}
            </button>
          </div>
        </aside>
      )}
      <main className={`main-content${showNav && collapsed ? ' main-content-collapsed' : ''}`}>{children}</main>
    </div>
  )
}

export default Layout
