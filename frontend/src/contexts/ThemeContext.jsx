import React, { createContext, useContext, useEffect, useState } from 'react'

const ThemeContext = createContext(null)
const STORAGE_KEY = 'ui_theme'

/** Read the saved choice before first paint, so there is no light flash. */
function initialTheme() {
  try {
    const saved = localStorage.getItem(STORAGE_KEY)
    if (saved === 'light' || saved === 'dark') return saved
  } catch { /* private mode, or storage disabled */ }
  // No choice on record: follow the operating system rather than guessing.
  try {
    if (window.matchMedia?.('(prefers-color-scheme: dark)').matches) return 'dark'
  } catch { /* matchMedia missing (SSR, old browser) */ }
  return 'light'
}

export function ThemeProvider({ children }) {
  const [theme, setThemeState] = useState(initialTheme)

  useEffect(() => {
    // The whole palette hangs off this attribute (see :root[data-theme="dark"]
    // in styles/main.css), so setting it here re-themes every page at once.
    document.documentElement.setAttribute('data-theme', theme)
    try { localStorage.setItem(STORAGE_KEY, theme) } catch { /* not fatal */ }
  }, [theme])

  const setTheme = (next) => setThemeState(next === 'dark' ? 'dark' : 'light')

  return (
    <ThemeContext.Provider value={{ theme, setTheme, toggle: () => setTheme(theme === 'dark' ? 'light' : 'dark') }}>
      {children}
    </ThemeContext.Provider>
  )
}

export function useTheme() {
  // Falls back to a no-op rather than throwing, so a component rendered
  // outside the provider (a smoke test, say) does not take the page down.
  return useContext(ThemeContext) || { theme: 'light', setTheme: () => {}, toggle: () => {} }
}
