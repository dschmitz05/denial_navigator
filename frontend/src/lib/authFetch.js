/**
 * Attach the logged-in user's bearer token to every API request.
 *
 * The audit log answers "who opened this patient's claim". It can only do that
 * if the request carries an identity: without this, every page except Users,
 * Audit and Upload called fetch() bare, so the server had nobody to attribute
 * the access to and wrote `anonymous`.
 *
 * This wraps fetch once at startup instead of touching ~25 call sites, for the
 * same reason the server audits in middleware: a page added later is covered
 * automatically, and there is no call site left to forget.
 *
 * Deliberately narrow — it only touches same-origin /api/ requests, and never
 * overwrites an Authorization header a caller set itself.
 */
export function installAuthFetch() {
  if (typeof window === 'undefined' || window.__authFetchInstalled) return
  window.__authFetchInstalled = true

  const nativeFetch = window.fetch.bind(window)

  window.fetch = (input, init = {}) => {
    let url = ''
    try {
      url = typeof input === 'string' ? input : (input && input.url) || ''
    } catch {
      return nativeFetch(input, init)
    }

    // Same-origin API calls only. A relative '/api/...' path, or an absolute
    // URL pointing back at this origin.
    const isApiCall = url.startsWith('/api/')
      || (url.startsWith(window.location.origin) && url.slice(window.location.origin.length).startsWith('/api/'))
    if (!isApiCall) return nativeFetch(input, init)

    const token = localStorage.getItem('auth_token')
    if (!token) return nativeFetch(input, init)

    const headers = new Headers(init.headers || (typeof input === 'object' ? input.headers : undefined) || {})
    if (!headers.has('Authorization')) {
      headers.set('Authorization', `Bearer ${token}`)
    }

    // The API now REFUSES unauthenticated requests rather than serving them
    // anonymously, so an expired token turns every page into a wall of errors.
    // Treat a 401 as the session ending: drop the dead token and go to login.
    return nativeFetch(input, { ...init, headers }).then(response => {
      if (response.status === 401 && !url.includes('/auth/login')) {
        localStorage.removeItem('auth_token')
        if (window.location.pathname !== '/login') {
          window.location.assign('/login')
        }
      }
      return response
    })
  }
}
