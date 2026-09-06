/**
 * Turn audit rows into something a billing manager can read.
 *
 * The log is written by middleware, so its raw shape is HTTP: `list_denial`,
 * `status_code=403`, `duration_ms=12`. That is the correct thing to STORE -
 * it is precise and complete - but it is not what a compliance reviewer or a
 * practice manager should have to decode. These functions translate a row
 * into plain English without discarding anything: the technical fields stay
 * available behind the row's expander.
 *
 * Pure functions on purpose, so the wording can be tested on its own.
 */

// Plain-English name for each action. The "who" is already its own column,
// so these read as a continuation of it: "admin — Signed in".
const ACTION_PHRASES = {
  login: 'Signed in',
  login_failed: 'Sign-in failed',
  create_user: 'Created a user',
  update_user: 'Updated a user',
  reset_password: 'Reset a password',
  deactivate_user: 'Deactivated a user',
  list_user: 'Browsed users',
  view_user: 'Opened a user',

  ingest_file: 'Uploaded a remittance file',
  list_file: 'Browsed upload history',

  list_claim: 'Browsed claims',
  view_claim: 'Opened a claim',
  edit_claim: 'Updated a claim',

  list_denial: 'Browsed denials',
  view_denial: 'Opened a denial',
  edit_denial: 'Updated a denial',

  list_appeal: 'Browsed the work queues',
  view_appeal: 'Opened a queue item',
  submit_appeal: 'Queued work on a denial',
  appeal_updated: 'Changed a queue item’s status',
  assign_appeal: 'Assigned work',

  generate_analysis: 'Ran AI analysis',
  store_analysis: 'Saved an AI analysis',
  list_analysis: 'Browsed AI analyses',

  update_knowledge: 'Added a policy document',
  delete_knowledge_doc: 'Removed a policy document',
  list_knowledge_doc: 'Browsed policy documents',
  search_knowledge: 'Searched policy documents',

  list_feedback: 'Browsed feedback',
  create_feedback: 'Rated an AI recommendation',

  view_audit_log: 'Viewed the audit log',
}

const VERB_WORDS = {
  list: 'Browsed', view: 'Opened', create: 'Created',
  edit: 'Updated', delete: 'Deleted',
}

const RESOURCE_WORDS = {
  claim: 'Claim', denial: 'Denial', appeal: 'Queue item',
  analysis: 'AI analysis', knowledge_doc: 'Policy document',
  file: 'File upload', feedback: 'Feedback', user: 'User',
  audit_log: 'Audit log', system: 'System',
}

export function describeAction(action) {
  if (!action) return 'Unknown action'
  if (ACTION_PHRASES[action]) return ACTION_PHRASES[action]
  // Fall back to the verb_resource shape the middleware generates, so an
  // action added later still reads as a sentence rather than a slug.
  const [verb, ...rest] = action.split('_')
  const resource = rest.join('_')
  if (VERB_WORDS[verb] && resource) {
    return `${VERB_WORDS[verb]} ${(RESOURCE_WORDS[resource] || resource.replace(/_/g, ' ')).toLowerCase()}`
  }
  return action.replace(/_/g, ' ').replace(/^\w/, c => c.toUpperCase())
}

/**
 * A refused request did not do the thing its action names - "Viewed the audit
 * log" is wrong for a 403. Say it was attempted.
 */
export function actionHeadline(action, details) {
  const phrase = describeAction(action)
  const outcome = describeOutcome(details)
  if (outcome && (outcome.tone === 'blocked' || outcome.tone === 'error')) {
    return `Attempted: ${phrase.charAt(0).toLowerCase()}${phrase.slice(1)}`
  }
  return phrase
}

export function describeResource(resourceType) {
  if (!resourceType) return '—'
  return RESOURCE_WORDS[resourceType] || resourceType.replace(/_/g, ' ')
}

/** Parse the jsonb details column, which may arrive as a string. */
export function parseDetails(details) {
  if (!details) return {}
  if (typeof details === 'string') {
    try { return JSON.parse(details) } catch { return {} }
  }
  return typeof details === 'object' ? details : {}
}

/**
 * What the HTTP status actually meant, in the reviewer's terms.
 * Returns null when the row carries no status (the hand-written entries).
 */
export function describeOutcome(details) {
  const d = parseDetails(details)
  const code = Number(d.status_code)
  if (!code) return null
  if (code < 400) return { label: 'OK', tone: 'ok' }
  if (code === 401) return { label: 'Blocked — not signed in', tone: 'blocked' }
  if (code === 403) return { label: 'Blocked — no permission', tone: 'blocked' }
  if (code === 404) return { label: 'Not found', tone: 'warn' }
  if (code === 409) return { label: 'Rejected — conflict', tone: 'warn' }
  if (code === 429) return { label: 'Rejected — too many requests', tone: 'warn' }
  if (code >= 500) return { label: 'Server error', tone: 'error' }
  return { label: `Rejected (${code})`, tone: 'warn' }
}

const prettyRole = (r) => (r ? String(r).replace(/_/g, ' ') : r)
const prettyWork = (r) => (r ? String(r).replace(/_/g, ' ') : 'work')

/**
 * Which record the entry is about, in the terms people use for it.
 *
 * The log stores a UUID, which answers nothing on its own - "who opened this
 * patient's claim" is the question, and an id does not name a claim. The
 * server resolves this when the entry is written, so it still reads correctly
 * after the record itself is gone.
 */
export function describeSubject(details) {
  const d = parseDetails(details)
  if (d.claim_number) {
    return d.patient_name ? `Claim ${d.claim_number} · ${d.patient_name}` : `Claim ${d.claim_number}`
  }
  if (d.document_title) return d.document_title
  if (d.target_username) return d.target_username
  if (d.record) return `(${d.record})`
  return ''
}

/**
 * A sentence describing what actually happened, or '' when the action name
 * already says everything (a plain successful read needs no elaboration).
 */
export function describeDetails(action, details) {
  const d = parseDetails(details)

  switch (action) {
    case 'login':
      return d.role ? `Role: ${prettyRole(d.role)}` : ''

    case 'login_failed':
      if (d.reason === 'unknown_or_inactive_user') {
        return `No active account named “${d.username}”`
      }
      if (d.reason === 'bad_password') {
        return `Wrong password for “${d.username}”`
      }
      return d.username ? `Attempted as “${d.username}”` : ''

    case 'assign_appeal': {
      const work = prettyWork(d.resolution_type)
      if (d.to && d.from) return `${work}: moved from ${d.from} to ${d.to}`
      if (d.to) return `${work}: assigned to ${d.to}`
      if (d.from) return `${work}: taken from ${d.from}, back to the unassigned pool`
      return `${work}: left unassigned`
    }

    case 'appeal_updated': {
      const work = prettyWork(d.resolution_type)
      const from = d.old_outcome ? String(d.old_outcome).replace(/_/g, ' ') : 'new'
      const to = d.outcome_status ? String(d.outcome_status).replace(/_/g, ' ') : 'unchanged'
      return `${work}: ${from} → ${to}`
    }

    case 'deactivate_user': {
      const released = Number(d.queue_items_released || 0)
      const who = d.username ? `Deactivated ${d.username}` : 'Deactivated a user'
      return released
        ? `${who}; ${released} open item${released === 1 ? '' : 's'} returned to the pool`
        : who
    }

    case 'create_user':
      return d.username ? `${d.username} — ${prettyRole(d.role)}` : ''

    case 'update_user': {
      const changes = d.changes || {}
      const parts = Object.entries(changes).map(([k, v]) => {
        if (k === 'is_active') return v ? 'reactivated' : 'deactivated'
        if (k === 'role') return `role → ${prettyRole(v)}`
        return `${k.replace(/_/g, ' ')} changed`
      })
      const released = Number(d.queue_items_released || 0)
      if (released) parts.push(`${released} open item${released === 1 ? '' : 's'} returned to the pool`)
      return parts.join(', ')
    }

    case 'reset_password':
      return d.target_username ? `For ${d.target_username}` : ''

    case 'generate_analysis': {
      const bits = []
      if (d.carc_code) bits.push(`CARC ${d.carc_code}`)
      if (d.cpt_code) bits.push(`CPT ${d.cpt_code}`)
      if (d.policies_retrieved !== undefined) {
        bits.push(d.policies_retrieved
          ? `${d.policies_retrieved} policy document(s) used`
          : 'no payer policy matched')
      }
      return bits.join(' · ')
    }
  }

  // Middleware-written rows. The outcome is shown as its own badge, so
  // repeating it here would just say the same thing twice in one row.

  // Surface a filter in words, since "what did they search for" is a real
  // question about a PHI access.
  if (d.query) {
    const interesting = String(d.query)
      .split('&')
      .map(p => p.split('='))
      .filter(([k]) => !['limit', 'offset', 'top_k'].includes(k))
      .map(([k, v]) => `${k.replace(/_/g, ' ')} ${decodeURIComponent(v || '')}`)
    if (interesting.length) return `Filtered by ${interesting.join(', ')}`
  }
  return ''
}

/** The precise, technical view — kept, just not shown by default. */
export function technicalDetails(row) {
  const d = parseDetails(row.details)
  const out = []
  if (d.method || d.path) out.push(['Request', `${d.method || ''} ${d.path || ''}`.trim()])
  if (d.query) out.push(['Query', d.query])
  if (d.status_code) out.push(['Status', String(d.status_code)])
  if (d.duration_ms !== undefined) out.push(['Duration', `${d.duration_ms} ms`])
  if (row.ip_address) out.push(['IP address', row.ip_address])
  if (row.user_agent) out.push(['Device', row.user_agent])
  if (row.resource_id) out.push(['Record id', row.resource_id])   // the raw uuid, for tracing
  const known = new Set(['method', 'path', 'query', 'status_code', 'duration_ms', 'outcome', 'username'])
  Object.entries(d).forEach(([k, v]) => {
    if (!known.has(k) && v !== null && v !== '') out.push([k.replace(/_/g, ' '), String(v)])
  })
  return out
}
