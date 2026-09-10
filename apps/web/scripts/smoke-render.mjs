/**
 * Server-render every page component once, to catch the errors a build cannot.
 *
 * `vite build` only type-checks nothing and parses syntax; it happily compiles
 * code that throws the moment it runs. That is how the Audit page shipped a
 * "Cannot access 'username' before initialization" - a useState declared below
 * the useEffect whose dependency array read it - and rendered a white screen.
 *
 * Rendering the component body is enough to catch that whole class of bug:
 * temporal-dead-zone reads, undefined identifiers, and anything that throws
 * during render. Effects do not run under SSR, so this is a smoke test, not a
 * substitute for using the app.
 */
import { renderToString } from 'react-dom/server'
import React from 'react'
import { MemoryRouter } from 'react-router-dom'
import { AuthProvider } from '../src/contexts/AuthContext.jsx'
import { ThemeProvider } from '../src/contexts/ThemeContext.jsx'

// Imported explicitly rather than globbed: a glob also picks up the .bak.*
// snapshots sitting next to the real files.
import Dashboard from '../src/pages/Dashboard.jsx'
import Claims from '../src/pages/Claims.jsx'
import Denials from '../src/pages/Denials.jsx'
import Appeals from '../src/pages/Appeals.jsx'
import Worklist from '../src/pages/Worklist.jsx'
import KnowledgeBase from '../src/pages/KnowledgeBase.jsx'
import Audit from '../src/pages/Audit.jsx'
import Users from '../src/pages/Users.jsx'
import Upload from '../src/pages/Upload.jsx'
import Settings from '../src/pages/Settings.jsx'
import Login from '../src/pages/Login.jsx'
import Profile from '../src/pages/Profile.jsx'
import Insights from '../src/pages/Insights.jsx'
import Playbooks from '../src/pages/Playbooks.jsx'

// Minimal browser surface the components touch at render time.
globalThis.localStorage = {
  getItem: () => null, setItem: () => {}, removeItem: () => {},
}
globalThis.fetch = () => new Promise(() => {})   // never resolves; effects are inert in SSR
globalThis.window = globalThis.window || { location: { pathname: '/', origin: 'http://x' } }
globalThis.window.matchMedia = globalThis.window.matchMedia || (() => ({ matches: false }))
globalThis.document = globalThis.document || { documentElement: { setAttribute: () => {} } }

const pages = {
  Dashboard, Claims, Denials, Appeals, Worklist,
  KnowledgeBase, Audit, Users, Upload, Settings, Login, Profile, Insights, Playbooks,
}

let failures = 0
for (const [name, Page] of Object.entries(pages)) {
  try {
    renderToString(
      React.createElement(MemoryRouter, null,
        React.createElement(ThemeProvider, null,
          React.createElement(AuthProvider, null,
            React.createElement(Page, { onLogin: () => {} }))))
    )
    console.log(`  ok    ${name}`)
  } catch (err) {
    failures++
    console.log(`  FAIL  ${name}: ${err.message}`)
  }
}
console.log(failures ? `\n${failures} page(s) throw during render` : '\nall pages render')
process.exit(failures ? 1 : 0)
