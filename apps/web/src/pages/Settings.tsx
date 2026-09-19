import React, { useState, useEffect, useCallback } from 'react'
import { useAuth } from '../contexts/AuthContext'

const API_BASE = '/api/v1'
const REFRESH_MS = 30000

type AnyRecord = Record<string, any>
type ReferenceResult = { total: number; page: number; pages: number; items: Array<AnyRecord> }
type ReferencePreview = AnyRecord
type Health = { overall?: string; checked_at?: string; services?: AnyRecord[]; urls?: AnyRecord }
type AppealWindow = AnyRecord & { payer_name: string; appeal_window_days?: number; is_default?: boolean }

const STATUS_LOOK: Record<string, { icon: string; word: string; tone: string }> = {
  ok:       { icon: '✅', word: 'Running',      tone: 'success' },
  degraded: { icon: '⚠️', word: 'Degraded',     tone: 'warning' },
  down:     { icon: '❌', word: 'Not working',  tone: 'danger' },
  starting: { icon: '⏳', word: 'Starting',      tone: 'warning' },
  disabled: { icon: '⏸️', word: 'Turned off',   tone: '' },
}

const KIND_LABEL: Record<string, string> = {
  carc: 'CARC — adjustment reason codes',
  rarc: 'RARC — remark codes',
  icd10: 'ICD-10 — diagnosis codes',
  cpt: 'CPT — procedure codes',
  hcpcs: 'HCPCS — Level II codes',
  modifier: 'Modifiers — HCPCS modifiers',
}

const ACTION_LABEL: Record<string, string> = {
  add: 'new',
  update: 'updated',
  deactivate: 'deactivated',
  reactivate: 'reactivated',
  unchanged: 'unchanged',
}

// Reference-list refresh (CARC, RARC, ICD-10, CPT). The server does the
// parsing and the diffing; this is a preview-then-apply form around it, so
// a bad file is always seen before it touches the list everyone else reads.
function ReferenceCodes({ canEdit }: { canEdit: boolean }) {
  const [summary, setSummary] = useState<AnyRecord | null>(null)
  const [kind, setKind] = useState('carc')
  const [file, setFile] = useState<File | null>(null)
  const [preview, setPreview] = useState<ReferencePreview | null>(null)   // dry-run result for file+kind
  const [busy, setBusy] = useState<'preview' | 'apply' | null>(null)         // 'preview' | 'apply'
  const [error, setError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  // Search & manage: one shared `kind` drives both the import target and the
  // list being searched, so there is a single selector for the section.
  const [query, setQuery] = useState('')
  const [page, setPage] = useState(1)
  const [pageSize, setPageSize] = useState(10)
  const [results, setResults] = useState<ReferenceResult | null>(null)   // search response for kind+query+page
  const [selected, setSelected] = useState<string[]>([])   // codes checked in the current page
  const [bump, setBump] = useState(0)            // force a re-search after mutations
  const [busyDelete, setBusyDelete] = useState<'delete' | 'clear' | null>(null)  // 'delete' | 'clear'

  const loadSummary = useCallback(async () => {
    try {
      const resp = await fetch(`${API_BASE}/reference/summary`)
      if (!resp.ok) throw new Error(`Could not load reference status (HTTP ${resp.status})`)
      setSummary(await resp.json())
    } catch {
      // Status is a nicety; the import form still works without it.
    }
  }, [])

  useEffect(() => { loadSummary() }, [loadSummary])

  // Debounced search: typing settles for 300 ms before the request goes out,
  // and a settled list is just a search with an empty query.
  useEffect(() => {
    const controller = new AbortController()
    const t = setTimeout(async () => {
      try {
        const params = new URLSearchParams({ page: String(page), page_size: String(pageSize) })
        if (query.trim()) params.set('q', query.trim())
        const resp = await fetch(`${API_BASE}/reference/${kind}/search?${params}`, { signal: controller.signal })
        if (resp.ok) {
          setResults(await resp.json())
          setSelected([])
        }
      } catch {
        // Browsing is a nicety; the import form does not depend on it.
      }
    }, 300)
    return () => {
      clearTimeout(t)
      controller.abort()
    }
  }, [kind, query, page, pageSize, bump])

  const invalidate = () => { setPreview(null); setNotice(null) }

  const doDelete = async (clearAll: boolean) => {
    if (busyDelete || !results) return
    if (clearAll && !window.confirm(`Delete ALL ${results.total} code(s) in ${KIND_LABEL[kind]}?`)) return
    if (!clearAll && selected.length === 0) return
    if (!clearAll && !window.confirm(`Delete ${selected.length} selected code(s) from ${KIND_LABEL[kind]}?`)) return
    setBusyDelete(clearAll ? 'clear' : 'delete')
    setError(null)
    try {
      const url = clearAll ? `${API_BASE}/reference/${kind}/clear` : `${API_BASE}/reference/${kind}/delete`
      const resp = await fetch(url, clearAll ? { method: 'POST' } : {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ codes: selected }),
      })
      const data = await resp.json()
      if (!resp.ok) throw new Error(data.detail || `Request failed (HTTP ${resp.status})`)
      setNotice(clearAll
        ? `Cleared ${KIND_LABEL[kind]}: ${data.deleted} code(s) deleted.`
        : `Deleted ${data.deleted} code(s)${data.not_found ? `; ${data.not_found} not in the list` : ''}.`)
      setSelected([])
      setPage(1)
      setBump(b => b + 1)
      await loadSummary()
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Delete failed')
    }
    setBusyDelete(null)
  }

  const toggleSelect = (code: string) => {
    setSelected(s => s.includes(code) ? s.filter(c => c !== code) : [...s, code])
  }

  const run = async (apply: boolean) => {
    if (!file || busy) return
    setBusy(apply ? 'apply' : 'preview')
    setError(null)
    try {
      const fd = new FormData()
      fd.append('file', file)
      fd.append('apply', String(apply))
      const resp = await fetch(`${API_BASE}/reference/${kind}/import`, { method: 'POST', body: fd })
      const data = await resp.json()
      if (!resp.ok) throw new Error(data.detail || `Import request failed (HTTP ${resp.status})`)
      if (apply) {
        const c = data.changes
        setNotice(`Imported ${data.filename}: ${c.add} added, ` +
          `${c.update + c.deactivate + c.reactivate} changed, ` +
          `${c.unchanged} unchanged. ${data.codes_not_in_file} code(s) not in the file were left untouched.`)
        setPreview(null)
        setFile(null)
        await loadSummary()
      } else {
        setPreview(data)
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Import failed')
    }
    setBusy(null)
  }

  return (
    <>
      <h4 style={{ marginTop: 24, marginBottom: 8 }}>Reference codes (CARC / RARC / ICD-10 / CPT / HCPCS / Modifiers)</h4>
      <p style={{ color: 'var(--text-muted)', marginBottom: 12, fontSize: '0.9rem' }}>
        X12 revises the claim adjustment reason codes a few times a year, the remark code
        list grows as payers are added, ICD-10 and CPT are updated every October, and CMS
        refreshes the HCPCS and modifier lists the same way. Upload the updated list as CSV
        — at minimum a code and a description per row, plus optionally category, status/active,
        applicable CAGC, and effective/expiration dates. The official ICD-10-CM, CPT, HCPCS and
        modifier download files (up to ~50 MB) work too. Codes missing from the file are left
        untouched. Preview first; nothing changes until you apply. You can also search any list
        below and delete individual codes or clear a whole list.
      </p>
      {error && <div className="callout callout-danger" style={{ marginBottom: 12 }}>{error}</div>}
      {notice && <div className="callout callout-info" style={{ marginBottom: 12 }}>{notice}</div>}

      {summary && (
        <div className="stats-grid" style={{ marginBottom: 16 }}>
          {Object.entries(summary).map(([k, s]) => (
            <div key={k} className="stat-card">
              <div className="stat-label">{KIND_LABEL[k]}</div>
              <div className="stat-value">{s.total}</div>
              <div className="stat-subtitle" style={{ wordBreak: 'break-word' }}>
                {s.active} active
                {s.last_import && (
                  <> · last import {new Date(s.last_import.at).toLocaleString()} by {s.last_import.by}
                    {s.last_import.filename ? ` (${s.last_import.filename})` : ''}</>
                )}
              </div>
            </div>
          ))}
        </div>
      )}

      <div style={{ border: '1px solid var(--gray-200)', borderRadius: 'var(--radius)', padding: 16, marginBottom: 16 }}>
        <div style={{ display: 'flex', gap: 16, flexWrap: 'wrap', alignItems: 'flex-end', marginBottom: 12 }}>
          <label>
            <div style={{ fontSize: '0.8rem', color: 'var(--text-muted)', marginBottom: 4 }}>List</div>
            <select className="form-select" style={{ width: 250 }}
                    value={kind}
                    onChange={e => { setKind(e.target.value); setPage(1); invalidate() }}>
              {Object.entries(KIND_LABEL).map(([value, label]) => (
                <option key={value} value={value}>{label}</option>
              ))}
            </select>
          </label>
          <div style={{ flex: 1, minWidth: 200 }}>
            <div style={{ fontSize: '0.8rem', color: 'var(--text-muted)', marginBottom: 4 }}>Search</div>
            <input className="form-input" style={{ width: '100%' }}
                   placeholder="Search by code or description…"
                   value={query}
                   onChange={e => { setQuery(e.target.value); setPage(1) }} />
          </div>
          <label>
            <div style={{ fontSize: '0.8rem', color: 'var(--text-muted)', marginBottom: 4 }}>Results per page</div>
            <select className="form-select" value={pageSize}
                    onChange={e => { setPageSize(Number(e.target.value)); setPage(1) }}>
              {[10, 25, 50, 100].map(size => <option key={size} value={size}>{size}</option>)}
            </select>
          </label>
          {canEdit && results && results.total > 0 && (
            <>
              <button className="btn btn-danger" disabled={selected.length === 0 || !!busyDelete}
                      onClick={() => doDelete(false)}>
                {busyDelete === 'delete' ? 'Deleting…' : `Delete selected (${selected.length})`}
              </button>
              <button className="btn btn-danger" disabled={!!busyDelete} onClick={() => doDelete(true)}>
                {busyDelete === 'clear' ? 'Clearing…' : `Clear all ${results.total}`}
              </button>
            </>
          )}
        </div>
        {results ? (
          <>
            <div className="table-container">
              <table>
                <thead>
                  <tr>
                    {canEdit && <th style={{ width: 32 }} />}
                    <th>Code</th>
                    <th>Description</th>
                    <th>Status</th>
                    <th>Effective</th>
                    <th>Expires</th>
                  </tr>
                </thead>
                <tbody>
                  {results.items.map(i => (
                    <tr key={i.code}>
                      {canEdit && (
                        <td>
                          <input type="checkbox"
                                 checked={selected.includes(i.code)}
                                 onChange={() => toggleSelect(i.code)} />
                        </td>
                      )}
                      <td style={{ fontFamily: 'monospace' }}>{i.code}</td>
                      <td>{i.description}</td>
                      <td>
                        <span style={{
                          background: i.is_active ? 'var(--success-light)' : 'var(--danger-light)',
                          color: i.is_active ? 'var(--success-text)' : 'var(--danger-text)',
                          borderRadius: 999, padding: '2px 10px', fontSize: '0.75rem', fontWeight: 600,
                        }}>
                          {i.is_active ? 'active' : 'inactive'}
                        </span>
                      </td>
                      <td>{i.effective_date || '—'}</td>
                      <td>{i.expiration_date || '—'}</td>
                    </tr>
                  ))}
                  {results.items.length === 0 && (
                    <tr>
                      <td colSpan={canEdit ? 6 : 5} style={{ color: 'var(--text-muted)' }}>
                        No codes found{query.trim() ? ` for “${query.trim()}”` : ''}.
                      </td>
                    </tr>
                  )}
                </tbody>
              </table>
            </div>
            <div style={{ display: 'flex', gap: 8, alignItems: 'center', marginTop: 8, fontSize: '0.85rem' }}>
              <button className="btn btn-sm" disabled={results.page <= 1} onClick={() => setPage(p => p - 1)}>← Prev</button>
              <span style={{ color: 'var(--text-muted)' }}>
                Page {results.page} of {Math.max(results.pages, 1)} · {results.total} code(s)
              </span>
              <button className="btn btn-sm" disabled={results.page >= results.pages} onClick={() => setPage(p => p + 1)}>Next →</button>
            </div>
          </>
        ) : (
          <div style={{ color: 'var(--text-muted)', fontSize: '0.9rem' }}>Loading list…</div>
        )}
      </div>

      {canEdit ? (
        <div style={{ border: '1px solid var(--gray-200)', borderRadius: 'var(--radius)', padding: 16, marginBottom: 24 }}>
          <div style={{ display: 'flex', gap: 16, flexWrap: 'wrap', alignItems: 'flex-end', marginBottom: 12 }}>
            <label>
              <div style={{ fontSize: '0.8rem', color: 'var(--text-muted)', marginBottom: 4 }}>
                File (CSV) — imported into {KIND_LABEL[kind].split(' — ')[0]}
              </div>
              <input type="file" accept=".csv,.txt,text/csv" className="form-input"
                     onChange={e => { setFile(e.target.files?.[0] || null); invalidate() }} />
            </label>
            <button className="btn" disabled={!file || !!busy} onClick={() => run(false)}>
              {busy === 'preview' ? 'Previewing…' : 'Preview changes'}
            </button>
            <button className="btn btn-primary" disabled={!preview || !!busy} onClick={() => run(true)}>
              {busy === 'apply' ? 'Importing…' : 'Apply import'}
            </button>
          </div>

          {preview && (
            <div>
              <div style={{ display: 'flex', gap: 10, flexWrap: 'wrap', alignItems: 'center', marginBottom: 8, fontSize: '0.9rem' }}>
                <span>{preview.valid_rows} of {preview.rows_parsed} row(s) usable</span>
                <span style={{ background: 'var(--success-light)', color: 'var(--success-text)', borderRadius: 999, padding: '2px 10px', fontWeight: 600 }}>
                  +{preview.changes.add} new
                </span>
                {preview.changes.update > 0 && (
                  <span style={{ background: 'var(--warning-light)', color: 'var(--warning-text)', borderRadius: 999, padding: '2px 10px', fontWeight: 600 }}>
                    {preview.changes.update} updated
                  </span>
                )}
                {preview.changes.deactivate > 0 && (
                  <span style={{ background: 'var(--danger-light)', color: 'var(--danger-text)', borderRadius: 999, padding: '2px 10px', fontWeight: 600 }}>
                    {preview.changes.deactivate} deactivated
                  </span>
                )}
                {preview.changes.reactivate > 0 && (
                  <span style={{ background: 'var(--success-light)', color: 'var(--success-text)', borderRadius: 999, padding: '2px 10px', fontWeight: 600 }}>
                    {preview.changes.reactivate} reactivated
                  </span>
                )}
                <span style={{ color: 'var(--text-muted)' }}>
                  {preview.changes.unchanged} unchanged · {preview.codes_not_in_file} existing code(s) not in file (left untouched)
                </span>
              </div>

              {preview.row_errors.length > 0 && (
                <details style={{ marginBottom: 8 }}>
                  <summary style={{ cursor: 'pointer', color: 'var(--danger)', fontSize: '0.85rem' }}>
                    {preview.row_errors.length} row(s) rejected
                  </summary>
                  <ul style={{ fontSize: '0.85rem', color: 'var(--gray-700)' }}>
                    {preview.row_errors.slice(0, 50).map((e: AnyRecord, i: number) => (
                      <li key={i}>row {e.row}{e.code ? ` (${e.code})` : ''}: {e.reason}</li>
                    ))}
                  </ul>
                </details>
              )}

              {preview.sample.length > 0 && (
                <details open>
                  <summary style={{ cursor: 'pointer', fontSize: '0.85rem', color: 'var(--text-muted)' }}>
                    First {preview.sample.length} row(s)
                  </summary>
                  <div className="table-container" style={{ marginTop: 8 }}>
                    <table>
                      <thead><tr><th>Code</th><th>Description</th><th>Action</th></tr></thead>
                      <tbody>
                        {preview.sample.map((s: AnyRecord) => (
                          <tr key={s.code}>
                            <td style={{ fontFamily: 'monospace' }}>{s.code}</td>
                            <td>{s.description}</td>
                            <td>{ACTION_LABEL[s.action] || s.action}</td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                </details>
              )}
            </div>
          )}
        </div>
      ) : (
        <p style={{ color: 'var(--text-muted)', fontSize: '0.85rem', marginTop: -8, marginBottom: 24 }}>
          Reference lists are updated by managers and above.
        </p>
      )}
    </>
  )
}

/** The amount at or above which a write-off needs a manager's approval.
 *  Admin-only, like the API behind it. */
function WriteOffThreshold() {
  const [value, setValue] = useState('')
  const [saved, setSaved] = useState<number | null>(null)
  const [message, setMessage] = useState<{ error: boolean; text: string } | null>(null)

  useEffect(() => {
    fetch(`${API_BASE}/settings/write-off-approval`)
      .then(r => (r.ok ? r.json() : Promise.reject(new Error(`HTTP ${r.status}`))))
      .then(d => { setSaved(d.threshold); setValue(String(d.threshold)) })
      .catch(() => setMessage({ error: true, text: 'Could not load the write-off approval threshold' }))
  }, [])

  const save = async () => {
    const threshold = Number(value)
    if (!Number.isFinite(threshold) || threshold < 0) {
      setMessage({ error: true, text: 'Enter an amount of 0 or more' })
      return
    }
    const resp = await fetch(`${API_BASE}/settings/write-off-approval`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ threshold }),
    })
    if (resp.ok) {
      setSaved(threshold)
      setMessage({ error: false, text: 'Saved' })
    } else {
      setMessage({ error: true, text: `Could not save (HTTP ${resp.status})` })
    }
  }

  return (
    <div style={{ marginBottom: 24 }}>
      <h4 style={{ marginBottom: 8 }}>Write-off approval</h4>
      <p style={{ color: 'var(--text-muted)', fontSize: '0.85rem', marginTop: 0 }}>
        Write-offs at or above this amount wait for a revenue cycle manager or administrator other than the
        person who asked. 0 means every write-off needs approval.
      </p>
      <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
        <span>$</span>
        <input className="form-input" style={{ maxWidth: 160 }} type="number" min={0} step="0.01"
          value={value} onChange={e => setValue(e.target.value)} />
        <button className="btn btn-primary" disabled={saved !== null && Number(value) === saved} onClick={save}>Save</button>
        {message && <span style={{ color: message.error ? 'var(--danger)' : 'var(--success-text)' }}>{message.text}</span>}
      </div>
    </div>
  )
}

/** Days from identifying an overpayment to its refund deadline. */
function OverpaymentRefundDays() {
  const [value, setValue] = useState('')
  const [message, setMessage] = useState<{ error: boolean; text: string } | null>(null)

  useEffect(() => {
    fetch(`${API_BASE}/settings/overpayment-refund`)
      .then(r => (r.ok ? r.json() : Promise.reject(new Error(`HTTP ${r.status}`))))
      .then(d => setValue(String(d.days)))
      .catch(() => setMessage({ error: true, text: 'Could not load the refund window' }))
  }, [])

  const save = async () => {
    const days = Number(value)
    if (!Number.isInteger(days) || days < 1 || days > 3650) {
      setMessage({ error: true, text: 'Enter a whole number of days between 1 and 3650' })
      return
    }
    const resp = await fetch(`${API_BASE}/settings/overpayment-refund`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ days }),
    })
    setMessage(resp.ok ? { error: false, text: 'Saved' } : { error: true, text: `Could not save (HTTP ${resp.status})` })
  }

  return (
    <div style={{ marginBottom: 24 }}>
      <h4 style={{ marginBottom: 8 }}>Overpayment refund window</h4>
      <p style={{ color: 'var(--text-muted)', fontSize: '0.85rem', marginTop: 0 }}>
        Days from identifying an overpayment to its refund deadline. Many payers set this by rule (60 days
        for Medicare); confirm the value with your compliance team.
      </p>
      <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
        <input className="form-input" style={{ maxWidth: 120 }} type="number" min={1} max={3650} step={1}
          value={value} onChange={e => setValue(e.target.value)} />
        <span>days</span>
        <button className="btn btn-primary" onClick={save}>Save</button>
        {message && <span style={{ color: message.error ? 'var(--danger)' : 'var(--success-text)' }}>{message.text}</span>}
      </div>
    </div>
  )
}

const DEADLINE_TYPES: Record<string, string> = {
  timely_filing: 'Timely filing (from date of service)',
  corrected_claim: 'Corrected claim (from remittance)',
  reconsideration: 'Reconsideration (from remittance)',
  appeal_level_2: 'Second-level appeal (from first appeal decision)',
  payer_response: 'Payer response time (days before a claim needs follow-up)',
}
type DeadlineRule = { id: string; payer_name: string; deadline_type: string; days: number; notes?: string | null }

/** Payer clocks beyond the appeal window. '*' is the organization default. */
function PayerDeadlineRules({ canEdit }: { canEdit: boolean }) {
  const [rules, setRules] = useState<DeadlineRule[]>([])
  const [draft, setDraft] = useState({ payer_name: '*', deadline_type: 'timely_filing', days: '' })
  const [error, setError] = useState<string | null>(null)

  const load = useCallback(() => {
    fetch(`${API_BASE}/denials/deadline-rules`)
      .then(r => (r.ok ? r.json() : []))
      .then(setRules)
      .catch(() => setRules([]))
  }, [])
  useEffect(() => { load() }, [load])

  const save = async () => {
    setError(null)
    const resp = await fetch(`${API_BASE}/denials/deadline-rules`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ ...draft, days: Number(draft.days) }),
    })
    if (!resp.ok) {
      const data = await resp.json().catch(() => ({}))
      setError(typeof data.detail === 'string' ? data.detail : `Could not save (HTTP ${resp.status})`)
      return
    }
    setDraft({ ...draft, days: '' })
    load()
  }
  const remove = async (id: string) => {
    await fetch(`${API_BASE}/denials/deadline-rules/${id}`, { method: 'DELETE' })
    load()
  }

  return (
    <div style={{ marginBottom: 24 }}>
      <h4 style={{ marginBottom: 8 }}>Payer deadlines</h4>
      <p style={{ color: 'var(--text-muted)', fontSize: '0.85rem', marginTop: 0 }}>
        Clocks beyond the appeal window, from each payer's manual or contract. A denial shows every deadline
        that has a rule here and marks the one for its recommended action. Payer <code>*</code> is the default.
      </p>
      <div className="table-container">
        <table>
          <thead><tr><th>Payer</th><th>Deadline</th><th>Days</th><th></th></tr></thead>
          <tbody>
            {rules.length === 0 && <tr><td colSpan={4} style={{ textAlign: 'center' }}>No rules yet</td></tr>}
            {rules.map(r => (
              <tr key={r.id}>
                <td>{r.payer_name === '*' ? 'Default (*)' : r.payer_name}</td>
                <td>{DEADLINE_TYPES[r.deadline_type] || r.deadline_type}</td>
                <td>{r.days}</td>
                <td>{canEdit && <button className="btn btn-sm" onClick={() => remove(r.id)}>Remove</button>}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {canEdit && (
        <div style={{ display: 'flex', gap: 8, alignItems: 'center', marginTop: 8, flexWrap: 'wrap' }}>
          <input className="form-input" style={{ maxWidth: 240 }} placeholder="Payer name or *" value={draft.payer_name}
            onChange={e => setDraft({ ...draft, payer_name: e.target.value })} />
          <select className="form-select" value={draft.deadline_type} onChange={e => setDraft({ ...draft, deadline_type: e.target.value })}>
            {Object.entries(DEADLINE_TYPES).map(([value, label]) => <option key={value} value={value}>{label}</option>)}
          </select>
          <input className="form-input" style={{ maxWidth: 100 }} type="number" min={1} max={3650} placeholder="Days"
            value={draft.days} onChange={e => setDraft({ ...draft, days: e.target.value })} />
          <button className="btn btn-primary" disabled={!draft.payer_name.trim() || !draft.days} onClick={save}>Save rule</button>
          {error && <span style={{ color: 'var(--danger)' }}>{error}</span>}
        </div>
      )}
    </div>
  )
}

type PayerAlias = { id: string; alias: string; kind: 'name' | 'payer_id' }
type Payer = { id: string; name: string; aliases: PayerAlias[] }
type UnmappedName = { name: string; source: string; count: number }

/** Payer names and IDs that mean the same payer, so knowledge documents match claims however each spells it. */
function PayersAndAliases({ canEdit }: { canEdit: boolean }) {
  const [payers, setPayers] = useState<Payer[]>([])
  const [unmapped, setUnmapped] = useState<UnmappedName[]>([])
  const [newPayer, setNewPayer] = useState('')
  const [aliasDraft, setAliasDraft] = useState<Record<string, { alias: string; kind: 'name' | 'payer_id' }>>({})
  const [error, setError] = useState<string | null>(null)

  const load = useCallback(() => {
    fetch(`${API_BASE}/payers`)
      .then(r => (r.ok ? r.json() : { payers: [], unmapped: [] }))
      .then(data => { setPayers(data.payers || []); setUnmapped(data.unmapped || []) })
      .catch(() => { setPayers([]); setUnmapped([]) })
  }, [])
  useEffect(() => { load() }, [load])

  const send = async (url: string, method: string, body?: unknown) => {
    setError(null)
    const resp = await fetch(url, {
      method,
      headers: body ? { 'Content-Type': 'application/json' } : undefined,
      body: body ? JSON.stringify(body) : undefined,
    })
    if (!resp.ok) {
      const data = await resp.json().catch(() => ({}))
      setError(typeof data.detail === 'string' ? data.detail : `Could not save (HTTP ${resp.status})`)
      return false
    }
    load()
    return true
  }
  const createPayer = async (name: string) => {
    if (await send(`${API_BASE}/payers`, 'POST', { name })) setNewPayer('')
  }
  const addAlias = async (payerId: string, alias: string, kind: 'name' | 'payer_id' = 'name') => {
    if (await send(`${API_BASE}/payers/${payerId}/aliases`, 'POST', { alias, kind })) {
      setAliasDraft({ ...aliasDraft, [payerId]: { alias: '', kind: 'name' } })
    }
  }

  return (
    <div style={{ marginBottom: 24 }}>
      <h4 style={{ marginBottom: 8 }}>Payers &amp; aliases</h4>
      <p style={{ color: 'var(--text-muted)', fontSize: '0.85rem', marginTop: 0 }}>
        Remittances, claims and payer manuals spell payer names differently. Names and payer IDs listed under one
        payer are treated as the same payer when the AI looks for policies that apply to a denial.
      </p>
      <div className="table-container">
        <table>
          <thead><tr><th>Payer</th><th>Aliases</th>{canEdit && <th>Add alias</th>}</tr></thead>
          <tbody>
            {payers.length === 0 && <tr><td colSpan={canEdit ? 3 : 2} style={{ textAlign: 'center' }}>No payers yet</td></tr>}
            {payers.map(p => {
              const draft = aliasDraft[p.id] || { alias: '', kind: 'name' as const }
              return (
                <tr key={p.id}>
                  <td>
                    {p.name}
                    {canEdit && <button className="btn btn-sm" style={{ marginLeft: 8 }}
                      onClick={() => send(`${API_BASE}/payers/${p.id}`, 'DELETE')}>Delete</button>}
                  </td>
                  <td>
                    {p.aliases.map(a => (
                      <span key={a.id} className="badge" style={{ marginRight: 6, display: 'inline-block', marginBottom: 4 }}>
                        {a.kind === 'payer_id' ? `ID ${a.alias}` : a.alias}
                        {canEdit && <button className="btn btn-sm" style={{ marginLeft: 4, padding: '0 4px' }} title="Remove alias"
                          onClick={() => send(`${API_BASE}/payers/${p.id}/aliases/${a.id}`, 'DELETE')}>×</button>}
                      </span>
                    ))}
                  </td>
                  {canEdit && (
                    <td>
                      <div style={{ display: 'flex', gap: 4 }}>
                        <input className="form-input" style={{ maxWidth: 200 }} placeholder="Name or payer ID" value={draft.alias}
                          onChange={e => setAliasDraft({ ...aliasDraft, [p.id]: { ...draft, alias: e.target.value } })} />
                        <select className="form-select" value={draft.kind}
                          onChange={e => setAliasDraft({ ...aliasDraft, [p.id]: { ...draft, kind: e.target.value as 'name' | 'payer_id' } })}>
                          <option value="name">Name</option>
                          <option value="payer_id">Payer ID</option>
                        </select>
                        <button className="btn btn-sm" disabled={!draft.alias.trim()} onClick={() => addAlias(p.id, draft.alias, draft.kind)}>Add</button>
                      </div>
                    </td>
                  )}
                </tr>
              )
            })}
          </tbody>
        </table>
      </div>
      {canEdit && (
        <div style={{ display: 'flex', gap: 8, alignItems: 'center', marginTop: 8, flexWrap: 'wrap' }}>
          <input className="form-input" style={{ maxWidth: 280 }} placeholder="New payer name" value={newPayer}
            onChange={e => setNewPayer(e.target.value)} />
          <button className="btn btn-primary" disabled={!newPayer.trim()} onClick={() => createPayer(newPayer)}>Add payer</button>
          {error && <span style={{ color: 'var(--danger)' }}>{error}</span>}
        </div>
      )}
      {unmapped.length > 0 && (
        <details style={{ marginTop: 12 }}>
          <summary>{unmapped.length} payer name{unmapped.length === 1 ? '' : 's'} not mapped to a payer</summary>
          <table style={{ marginTop: 8 }}>
            <thead><tr><th>Name</th><th>Seen in</th><th>Count</th>{canEdit && payers.length > 0 && <th>Add as alias of</th>}</tr></thead>
            <tbody>
              {unmapped.map(u => (
                <tr key={`${u.source}:${u.name}`}>
                  <td>{u.name}</td>
                  <td>{u.source}</td>
                  <td>{u.count}</td>
                  {canEdit && payers.length > 0 && (
                    <td>
                      <select className="form-select" defaultValue="" onChange={e => e.target.value && addAlias(e.target.value, u.name)}>
                        <option value="">Choose payer…</option>
                        {payers.map(p => <option key={p.id} value={p.id}>{p.name}</option>)}
                      </select>
                    </td>
                  )}
                </tr>
              ))}
            </tbody>
          </table>
        </details>
      )}
    </div>
  )
}

export default function Settings() {
  const { can } = useAuth()
  const canEdit = can.manageKnowledge()      // policy curation, same as documents
  const [windows, setWindows] = useState<AppealWindow[]>([])
  const [draft, setDraft] = useState<Record<string, string>>({})
  const [savingWindow, setSavingWindow] = useState<string | null>(null)
  const [windowsError, setWindowsError] = useState<string | null>(null)
  const [health, setHealth] = useState<Health | null>(null)
  const [checking, setChecking] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const check = useCallback(async () => {
    setChecking(true)
    try {
      const resp = await fetch(`${API_BASE}/system/health`)
      if (!resp.ok) throw new Error(`Health check failed (HTTP ${resp.status})`)
      setHealth(await resp.json())
      setError(null)
    } catch (err) {
      // If the gateway itself cannot be reached, saying so is the status.
      setError(err instanceof Error ? err.message : 'Health check failed')
      setHealth(null)
    }
    setChecking(false)
  }, [])

  useEffect(() => {
    check()
    const t = setInterval(check, REFRESH_MS)
    return () => clearInterval(t)
  }, [check])

  const loadWindows = useCallback(async () => {
    try {
      const resp = await fetch(`${API_BASE}/denials/appeal-windows`)
      if (!resp.ok) throw new Error(`Could not load filing windows (HTTP ${resp.status})`)
      setWindows(await resp.json())
      setWindowsError(null)
    } catch (err) { setWindowsError(err instanceof Error ? err.message : 'Could not load filing windows') }
  }, [])

  useEffect(() => { loadWindows() }, [loadWindows])

  const defaultWindow = windows.find(w => w.is_default)?.appeal_window_days

  const saveWindow = async (payerName: string) => {
    const value = Number(draft[payerName] ?? windows.find(w => w.payer_name === payerName)?.appeal_window_days)
    if (!value || value < 1) { setWindowsError('Enter a number of days'); return }
    setSavingWindow(payerName)
    try {
      const resp = await fetch(`${API_BASE}/denials/appeal-windows`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ payer_name: payerName, appeal_window_days: value }),
      })
      const data = await resp.json()
      if (!resp.ok) throw new Error(data.detail || 'Could not save')
      // Say how many open denials moved: editing a window silently re-dating
      // the queue would be worse than not saying.
      setWindowsError(null)
      setDraft(d => { const { [payerName]: _, ...rest } = d; return rest })
      await loadWindows()
      alert(`Saved. ${data.denials_redated} open denial(s) re-dated.`)
    } catch (err) { setWindowsError(err instanceof Error ? err.message : 'Could not save filing window') }
    setSavingWindow(null)
  }

  const overall = health?.overall
  const overallLook = STATUS_LOOK[overall || 'down'] || STATUS_LOOK.down

  return (
    <div className="page-body">
      <div className="card">
        <div className="card-header"><h3>⚙️ Settings</h3></div>
        <div className="card-body">
          <div style={{ display: 'flex', alignItems: 'baseline', justifyContent: 'space-between', gap: 12, marginBottom: 16, flexWrap: 'wrap' }}>
            <h4 style={{ margin: 0 }}>
              Service Status
              {health && (
                <span style={{ marginLeft: 10, fontWeight: 500, color: 'var(--text-muted)', fontSize: '0.85rem' }}>
                  {overallLook.icon} {overall === 'ok' ? 'All services healthy'
                    : overall === 'degraded' ? 'Running, but AI features are unavailable'
                    : 'One or more essential services are down'}
                </span>
              )}
            </h4>
            <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
              <span style={{ color: 'var(--text-muted)', fontSize: '0.8rem' }}>
                {checking ? 'Checking…' : health?.checked_at ? `Checked ${new Date(health.checked_at).toLocaleTimeString()}` : ''}
              </span>
              <button className="btn btn-sm" onClick={check} disabled={checking}>↻ Refresh</button>
            </div>
          </div>

          {error && (
            <div className="callout callout-danger" style={{ marginBottom: 16 }}>
              Could not reach the API gateway: {error}
            </div>
          )}

          {!health && checking && (
            <p style={{ color: 'var(--text-muted)' }}>Checking services…</p>
          )}

          <div className="stats-grid">
            {(health?.services || []).map(svc => {
              const look = STATUS_LOOK[svc.status] || STATUS_LOOK.down
              return (
                <div key={svc.name} className={`stat-card ${look.tone}`}>
                  <div className="stat-label">
                    {svc.name}
                    {!svc.essential && (
                      <span style={{ color: 'var(--text-muted)', fontWeight: 400 }}> · optional</span>
                    )}
                  </div>
                  <div className="stat-value" style={{ fontSize: '1rem' }}>
                    {look.icon} {look.word}
                  </div>
                  {/* The reason matters more than the badge: "no model is
                      loaded" and "connection refused" need different fixes. */}
                  <div className="stat-subtitle" style={{ wordBreak: 'break-word' }}>
                    {svc.detail}{svc.latency_ms ? ` · ${svc.latency_ms}ms` : ''}
                  </div>
                </div>
              )
            })}
          </div>

          {(() => {
            const parser = (health?.services || []).find(svc => svc.name === 'EDI parser')
            const sources = Array.isArray(parser?.sources) ? parser.sources : []
            if (!sources.length) return null
            return (
              <>
                <h4 style={{ marginTop: 24, marginBottom: 8 }}>Import sources</h4>
                <p style={{ color: 'var(--text-muted)', marginBottom: 12, fontSize: '0.9rem' }}>
                  Status is refreshed with the service check. Source credentials and locations are never displayed here.
                </p>
                <div className="stats-grid">
                  {sources.map((source: AnyRecord) => {
                    const look = STATUS_LOOK[source.status] || STATUS_LOOK.down
                    return <div key={source.name} className={`stat-card ${look.tone}`}>
                      <div className="stat-label">{source.name}</div>
                      <div className="stat-value" style={{ fontSize: '1rem' }}>{look.icon} {look.word}</div>
                      <div className="stat-subtitle" style={{ wordBreak: 'break-word' }}>
                        {source.detail}
                        {source.last_checked_at ? ` · checked ${new Date(source.last_checked_at).toLocaleTimeString()}` : ''}
                      </div>
                    </div>
                  })}
                </div>
              </>
            )
          })()}

          <h4 style={{ marginTop: 24, marginBottom: 8 }}>Appeal filing windows</h4>
          <p style={{ color: 'var(--text-muted)', marginBottom: 12, fontSize: '0.9rem' }}>
            How long you have to contest a denial, per payer. A remittance does not
            state this — it is the payer's own rule — so the deadlines the dashboard
            warns about are only as good as what is set here.
          </p>
          {windowsError && <div className="callout callout-danger" style={{ marginBottom: 12 }}>{windowsError}</div>}
          <div className="table-container" style={{ marginBottom: 24 }}>
            <table>
              <thead>
                <tr><th>Payer</th><th>Window</th><th>Claims</th><th></th></tr>
              </thead>
              <tbody>
                {windows.length === 0 ? (
                  <tr><td colSpan={4} style={{ padding: 12, color: 'var(--text-muted)' }}>Loading…</td></tr>
                ) : windows.map(w => (
                  <tr key={w.payer_name}>
                    <td>
                      {w.is_default ? <em>All other payers (default)</em> : w.payer_name}
                      {w.using_default && (
                        <span style={{ color: 'var(--text-muted)', fontSize: '0.8rem' }}> · using the default</span>
                      )}
                    </td>
                    <td style={{ whiteSpace: 'nowrap' }}>
                      <input
                        className="form-input"
                        type="number" min="1" max="3650"
                        style={{ width: 90, padding: '2px 6px' }}
                        value={draft[w.payer_name] ?? w.appeal_window_days ?? ''}
                        placeholder={String(defaultWindow ?? 90)}
                        disabled={!canEdit}
                        onChange={e => setDraft({ ...draft, [w.payer_name]: e.target.value })}
                      /> days
                    </td>
                    <td style={{ color: 'var(--text-muted)' }}>{w.claims_covered ?? 0}</td>
                    <td>
                      {canEdit && (
                        <button className="btn btn-sm" disabled={savingWindow === w.payer_name}
                                onClick={() => saveWindow(w.payer_name)}>
                          {savingWindow === w.payer_name ? 'Saving…' : 'Save'}
                        </button>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          {!canEdit && (
            <p style={{ color: 'var(--text-muted)', fontSize: '0.85rem', marginTop: -12, marginBottom: 24 }}>
              Filing windows are edited by managers and above.
            </p>
          )}

          <PayerDeadlineRules canEdit={canEdit} />
          <PayersAndAliases canEdit={canEdit} />

          {can.manageUsers() && <WriteOffThreshold />}
          {can.manageUsers() && <OverpaymentRefundDays />}

          <ReferenceCodes canEdit={canEdit} />

          <h4 style={{ marginTop: 24, marginBottom: 16 }}>Quick Links</h4>
          <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
            <a href="/docs" className="btn" target="_blank" rel="noreferrer">📖 API Documentation (Swagger)</a>
            <a href="/redoc" className="btn" target="_blank" rel="noreferrer">📕 API Reference (ReDoc)</a>
            {health?.urls?.llama && (
              <a href={health.urls.llama} className="btn" target="_blank" rel="noreferrer">
                🤖 llama.cpp Server ({health.urls.llama.replace(/^https?:\/\//, '')})
              </a>
            )}
          </div>

          <h4 style={{ marginTop: 24, marginBottom: 16 }}>llama.cpp Model Management</h4>
          <p style={{ color: 'var(--gray-500)', marginBottom: 16 }}>
            Models are loaded on the remote servers below — this app does not load or
            unload them. The status cards above show what each server currently reports.
            To inspect them directly:
          </p>
          <div className="code-block">
            <span className="comment"># List the models the server currently has loaded</span>{'\n'}
            curl http://10.10.10.98:8080/v1/models{'\n\n'}
            <span className="comment"># Check the server is up</span>{'\n'}
            curl http://10.10.10.98:8080/health{'\n\n'}
            <span className="comment"># Embeddings come from Ollama, not the chat server</span>{'\n'}
            curl http://10.10.10.98:11434/api/tags
          </div>

          <h4 style={{ marginTop: 24, marginBottom: 16 }}>Security &amp; compliance</h4>

          <div className="callout callout-info" style={{ marginBottom: 12 }}>
            <strong>In place</strong>
            <ul>
              <li>All PHI stays inside this deployment — nothing is sent to a public LLM endpoint.</li>
              <li>Every API request is authenticated; unauthenticated callers are refused, not served.</li>
              <li>Role-based access: specialists work their own queue, managers assign and review,
                  only admins manage users.</li>
              <li>The audit log records every access, including refused ones, with user, IP and outcome.</li>
              <li>Passwords are stored as bcrypt hashes and changed through your profile page.</li>
            </ul>
          </div>

          <div className="callout callout-warning">
            <strong>Still on you before production</strong>
            <ul>
              <li>Change the default <code>admin</code> password and the PostgreSQL password.</li>
              <li>Terminate TLS in front of this app — tokens and PHI cross the network in the clear
                  over plain HTTP.</li>
              <li>Set up automated database backups and test restoring one.</li>
              <li>Decide how long audit entries are retained; nothing prunes them today.</li>
            </ul>
          </div>
        </div>
      </div>
    </div>
  )
}
