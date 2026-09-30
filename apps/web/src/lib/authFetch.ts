/**
 * Mark every API request as coming from this app (the session itself is an
 * httpOnly cookie), and make sure a failure response is always JSON.
 *
 * The audit log answers "who opened this patient's claim". It can only do that
 * if the request carries an identity: without this, every page except Users,
 * Audit and Upload called fetch() bare, so the server had nobody to attribute
 * the access to and wrote `anonymous`.
 *
 * Every page calls `resp.json()` on an error response without checking its
 * content-type, because the API always returns one. A request that never
 * reaches the API - the reverse proxy's own 413 on a file over its upload
 * cap, a 502 while a container restarts - returns that proxy's own HTML
 * error page instead, and `.json()` throws "Unexpected token '<' ... is not
 * valid JSON" instead of showing the actual problem. Rewriting a non-JSON
 * failure into the shape the API would have used fixes every call site at
 * once, the same reasoning as the auth header below. A *successful*
 * non-JSON response (a PDF or CSV download) is left alone.
 *
 * This wraps fetch once at startup instead of touching ~40 call sites, for the
 * same reason the server audits in middleware: a page added later is covered
 * automatically, and there is no call site left to forget.
 *
 * Deliberately narrow — it only touches same-origin /api/ requests, and never
 * overwrites headers a caller set itself (the MFA and password-change steps send
 * their own restricted bearer token).
 */
declare global {
  interface Window {
    __authFetchInstalled?: boolean
  }
}

export function installAuthFetch(): void {
  if (typeof window === 'undefined' || window.__authFetchInstalled) return
  window.__authFetchInstalled = true

  const nativeFetch = window.fetch.bind(window)

  window.fetch = (input, init = {}) => {
    let url = ''
    try {
      url = typeof input === 'string' ? input : input instanceof Request ? input.url : input.toString()
    } catch {
      return nativeFetch(input, init)
    }

    // Same-origin API calls only. A relative '/api/...' path, or an absolute
    // URL pointing back at this origin.
    const isApiCall = url.startsWith('/api/')
      || (url.startsWith(window.location.origin) && url.slice(window.location.origin.length).startsWith('/api/'))
    if (!isApiCall) return nativeFetch(input, init)

    // The browser attaches the httpOnly session cookie itself. The marker
    // header is what the server requires on a cookie-authenticated write: a
    // cross-site form cannot set it, and CORS keeps cross-origin scripts from
    // doing so.
    const requestHeaders = input instanceof Request ? input.headers : undefined
    const headers = new Headers(init.headers || requestHeaders || {})
    if (!headers.has('X-Requested-With')) headers.set('X-Requested-With', 'fetch')

    // An expired or revoked session turns every page into a wall of errors.
    // Treat a 401 as the session ending and go to login - except the probes
    // that legitimately answer 401 while signed out.
    return nativeFetch(input, { ...init, headers }).then(async response => {
      if (response.status === 401 && !url.includes('/auth/login') && !url.includes('/auth/me')) {
        if (window.location.pathname !== '/login') window.location.assign('/login')
      }
      return response.ok ? response : normalizeErrorResponse(response)
    })
  }
}

/** A failed response the API itself sent is already `{"detail": ...}` JSON. */
async function normalizeErrorResponse(response: Response): Promise<Response> {
  const contentType = response.headers.get('content-type') || ''
  if (contentType.includes('application/json')) return response

  const text = await response.text()
  const detail = statusDetail(response.status) || text.replace(/<[^>]*>/g, ' ').trim().slice(0, 200)
    || `Request failed (HTTP ${response.status})`
  return new Response(JSON.stringify({ detail }), {
    status: response.status,
    statusText: response.statusText,
    headers: { 'content-type': 'application/json' },
  })
}

function statusDetail(status: number): string | null {
  // The reverse proxy's own error page for these carries no useful detail
  // beyond the status itself, and stripping its HTML leaves boilerplate
  // ("nginx", "openresty") rather than anything about the actual request.
  switch (status) {
    case 413: return 'File too large for the server to accept.'
    case 502: return 'The server is temporarily unavailable. Try again shortly.'
    case 503: return 'The server is temporarily unavailable. Try again shortly.'
    case 504: return 'The request timed out.'
    default: return null
  }
}
