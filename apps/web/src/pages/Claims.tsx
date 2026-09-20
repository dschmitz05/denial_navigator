import React, { useState, useEffect } from 'react'
import AiStatusBanner, { FALLBACK_LABEL } from '../components/AiStatusBanner'
import { reasonLabel, type ProviderAdjustment } from '../components/ProviderAdjustments'

const API_BASE = '/api/v1'

type Claim = Record<string, any> & { id: string; claim_number?: string; status?: string; denial_count?: number; open_denial_count?: number }
type ClaimDetail = Claim & { denials?: Array<Record<string, any>>; analyses?: Array<Record<string, any>> }

function formatCurrency(value?: number) {
  return new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD' }).format(value || 0)
}

function formatDate(dateStr?: string) {
  if (!dateStr) return '—'
  return new Date(dateStr).toLocaleDateString('en-US')
}

// Timestamptz fields (parsed_at, created_at, updated_at) carry a time the
// date-only formatter would quietly drop.
function formatDateTime(dateStr?: string) {
  if (!dateStr) return '—'
  return new Date(dateStr).toLocaleString('en-US', { dateStyle: 'medium', timeStyle: 'short' })
}

// A single label/value tile for the detail grids. Empty or null reads as an
// em dash, matching the rest of the app.
function Field({ label, value }: { label: string; value: React.ReactNode }) {
  const empty = value === null || value === undefined || value === ''
  return (
    <div className="detail-item">
      <div className="detail-label">{label}</div>
      <div className="detail-value">{empty ? '—' : value}</div>
    </div>
  )
}

function denialProgress(claim: Claim) {
  const total = Number(claim.denial_count || 0)
  if (!total) return <span style={{ color: 'var(--gray-400)' }}>—</span>
  const open = Number(claim.open_denial_count || 0)
  const closed = total - open
  return (
    <span style={{ color: open ? 'var(--warning-text)' : 'var(--success)', whiteSpace: 'nowrap' }}>
      {closed}/{total} closed{open ? ` · ${open} open` : ''}
    </span>
  )
}

// Full record for one claim: everything GET /claims/{id} returns — the whole
// claim row, every denial line, and the AI analyses — so a biller can verify
// the numbers against the source document without hunting through the EDI.
function ClaimDetails({ claim, onRefresh, refreshing }: { claim: ClaimDetail; onRefresh: () => void; refreshing: boolean }) {
  const denials = claim.denials || []
  const analyses = claim.analyses || []

  return (
    <div style={{ padding: '4px 16px 20px' }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 10, flexWrap: 'wrap', marginBottom: 12 }}>
        <strong>{claim.claim_number}</strong>
        <span className={`badge badge-${(claim.status || '').replace(/_/g, '-')}`}>{(claim.status || 'unknown').replace(/_/g, ' ')}</span>
        <span style={{ color: 'var(--gray-500)', fontSize: '0.8rem' }}>
          Full claim record · fetched {new Date().toLocaleTimeString('en-US')}
        </span>
        <button className="btn btn-sm" style={{ marginLeft: 'auto' }} onClick={onRefresh} disabled={refreshing}>
          {refreshing ? 'Refreshing…' : '↻ Refresh details'}
        </button>
      </div>

      <h4 style={{ margin: '4px 0 8px' }}>Patient · Provider · Payer</h4>
      <div className="detail-grid">
        <Field label="Patient" value={claim.patient_name} />
        <Field label="Patient ID" value={claim.patient_id} />
        <Field label="Date of birth" value={formatDate(claim.date_of_birth)} />
        <Field label="Provider" value={claim.provider_name} />
        <Field label="Provider NPI" value={claim.provider_npi} />
        <Field label="Payer" value={claim.payer_name} />
        <Field label="Payer ID" value={claim.payer_id_number} />
      </div>

      <h4 style={{ margin: '12px 0 8px' }}>Claim & Service</h4>
      <div className="detail-grid">
        <Field label="Claim type" value={(claim.claim_type || '').replace(/_/g, ' ')} />
        <Field label="Frequency code" value={claim.frequency_code} />
        <Field label="837 correlation" value={claim.correlation_status === 'matched'
          ? `Matched (${Math.round(Number(claim.correlation_confidence || 0) * 100)}% exact claim number)`
          : 'No matched 837 context'} />
        <Field label="Service from" value={formatDate(claim.service_from)} />
        <Field label="Service to" value={formatDate(claim.service_to)} />
        <Field label="Admission date" value={formatDate(claim.admission_date)} />
        <Field label="Discharge date" value={formatDate(claim.discharge_date)} />
        <Field label="Parsed at" value={formatDateTime(claim.parsed_at)} />
        <Field label="Created" value={formatDateTime(claim.created_at)} />
        <Field label="Updated" value={formatDateTime(claim.updated_at)} />
      </div>

      <h4 style={{ margin: '12px 0 8px' }}>Diagnosis</h4>
      <div style={{ marginBottom: 12 }}>
        {claim.icd_10_codes && claim.icd_10_codes.length > 0 ? (
          <span>
            {claim.icd_10_codes.map((code: string) => (
              <span key={code} className="badge" style={{ marginRight: 6, marginBottom: 4 }}>{code}</span>
            ))}
          </span>
        ) : (
          <span style={{ color: 'var(--gray-500)' }}>—</span>
        )}
        {claim.diagnosis_pointer && (
          <span style={{ marginLeft: 12, color: 'var(--gray-500)', fontSize: '0.85rem' }}>
            Diagnosis pointers: {Array.isArray(claim.diagnosis_pointer) ? claim.diagnosis_pointer.join(', ') : claim.diagnosis_pointer}
          </span>
        )}
      </div>

      <h4 style={{ margin: '12px 0 8px' }}>Financials</h4>
      <div className="detail-grid" style={{ marginBottom: 12 }}>
        <Field label="Total charge" value={formatCurrency(claim.total_charge)} />
        <Field label="Total paid" value={formatCurrency(claim.total_paid)} />
        <Field label="Patient paid" value={formatCurrency(claim.patient_paid)} />
        <Field label="Total adjustment" value={formatCurrency(claim.total_adjustment)} />
      </div>
      <p style={{ margin: '-6px 0 12px', color: 'var(--gray-500)', fontSize: '0.78rem' }}>
        Patient paid reflects PR (patient-responsibility) adjustments reported on the remittance.
      </p>

      {(claim.provider_adjustments as ProviderAdjustment[] | undefined)?.length ? (
        <>
          <h4 style={{ margin: '12px 0 8px' }}>Provider-level adjustments naming this claim</h4>
          <p style={{ margin: '-4px 0 8px', color: 'var(--gray-500)', fontSize: '0.78rem' }}>
            PLB lines on a remittance that reference this claim, such as recovering an earlier overpayment from
            another payment. A positive amount was taken out of that payment.
          </p>
          <div className="table-container" style={{ marginBottom: 12 }}>
            <table>
              <thead><tr><th>Date</th><th>Payer</th><th>Reason</th><th>Trace</th><th>Amount</th></tr></thead>
              <tbody>
                {(claim.provider_adjustments as ProviderAdjustment[]).map(pa => (
                  <tr key={pa.id}>
                    <td>{pa.payment_date || '—'}</td>
                    <td>{pa.payer_name || '—'}</td>
                    <td>{reasonLabel(pa.reason_code)}</td>
                    <td>{pa.trace_number || '—'}</td>
                    <td>{formatCurrency(pa.amount)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </>
      ) : null}

      <h4 style={{ margin: '12px 0 8px' }}>Denial lines ({denials.length})</h4>
      {denials.length === 0 ? (
        <p style={{ color: 'var(--gray-500)' }}>No denial lines on this claim.</p>
      ) : (
        <div className="table-container" style={{ marginBottom: 4 }}>
          <table>
            <thead>
              <tr>
                <th>Line</th>
                <th>CPT</th>
                <th>HCPCS</th>
                <th>Mods</th>
                <th>Charge</th>
                <th>Paid</th>
                <th>Adjusted</th>
                <th>CAGC</th>
                <th>CARC</th>
                <th>RARC</th>
                <th>Reason</th>
                <th>Denial date</th>
                <th>Status</th>
                <th>Appeal deadline</th>
              </tr>
            </thead>
            <tbody>
              {denials.map(d => (
                <tr key={d.id}>
                  <td>{d.service_line_number ?? '—'}</td>
                  <td>{d.cpt_code || '—'}</td>
                  <td>{d.hcpcs_code || '—'}</td>
                  <td>{[d.modifier_1, d.modifier_2].filter(Boolean).join(' ') || '—'}</td>
                  <td>{formatCurrency(d.charge_amount)}</td>
                  <td>{formatCurrency(d.payment_amount)}</td>
                  <td>{formatCurrency(d.adjustment_amount)}</td>
                  <td>{d.cagc || '—'}</td>
                  <td>{d.carc_code || '—'}</td>
                  <td>{d.rarc_code || '—'}</td>
                  <td>{d.adjustment_reason || d.denial_reason_code || '—'}</td>
                  <td>{formatDate(d.denial_date)}</td>
                  <td>
                    <span className={`badge badge-${(d.status || 'open').replace(/ /g, '-')}`}>{(d.status || 'open').replace(/_/g, ' ')}</span>
                  </td>
                  <td>{formatDate(d.appeal_deadline)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {analyses.length > 0 && (
        <>
          <h4 style={{ margin: '16px 0 4px' }}>🤖 AI analyses ({analyses.length})</h4>
          <p style={{ color: 'var(--gray-500)', fontSize: '0.8rem', margin: '0 0 10px' }}>
            AI-generated and <strong>advisory only</strong> — the AI can be wrong.
            Verify against the claim and payer rules before acting.
          </p>
          {analyses.map(a => (
            <div key={a.id} className="card" style={{ marginBottom: 10 }}>
              <div className="card-body">
                <div style={{ display: 'flex', gap: 10, flexWrap: 'wrap', alignItems: 'center', marginBottom: 8 }}>
                  <span style={{ fontWeight: 600 }}>{a.model_name || 'AI analysis'}</span>
                  {a.denial_category && (
                    <span className="badge badge-analyzed">{a.denial_category.replace(/_/g, ' ')}</span>
                  )}
                  {a.fallback_reason && FALLBACK_LABEL[a.fallback_reason] && (
                    <span className="badge badge-warning" title="This analysis is degraded">{FALLBACK_LABEL[a.fallback_reason]}</span>
                  )}
                  {a.required_action && (
                    <span className="badge">{a.required_action.replace(/_/g, ' ')}</span>
                  )}
                  {a.confidence_score != null && (
                    <span style={{ color: 'var(--gray-500)', fontSize: '0.8rem' }}>
                      confidence {Math.round(Number(a.confidence_score) * 100)}%
                    </span>
                  )}
                  <span style={{ color: 'var(--gray-400)', fontSize: '0.8rem' }}>{formatDateTime(a.created_at)}</span>
                </div>
                {a.root_cause_summary && (
                  <p style={{ marginBottom: 8 }}><strong>Root cause:</strong> {a.root_cause_summary}</p>
                )}
                {a.explanation && <p style={{ marginBottom: 8 }}><strong>Explanation:</strong> {a.explanation}</p>}
                {Array.isArray(a.steps) && a.steps.length > 0 && (
                  <div style={{ marginBottom: 8 }}>
                    <strong>Resolution steps:</strong>
                    <ol style={{ marginTop: 6, paddingLeft: 20 }}>
                      {a.steps.map((s, i) => (
                        <li key={i} style={{ marginBottom: 4 }}>{typeof s === 'string' ? s : s.action}</li>
                      ))}
                    </ol>
                  </div>
                )}
                {a.needs_appeal && (
                  <p style={{ color: 'var(--warning-text)', fontSize: '0.9rem' }}>
                    ⚖️ Analysis recommends an appeal.
                  </p>
                )}
                {a.draft_appeal_letter && (
                  <details style={{ marginTop: 6 }}>
                    <summary style={{ cursor: 'pointer', color: 'var(--gray-500)', fontSize: '0.85rem' }}>
                      Draft appeal letter
                    </summary>
                    <div className="appeal-letter" style={{ marginTop: 8 }}>
                      {a.draft_appeal_letter}
                    </div>
                  </details>
                )}
              </div>
            </div>
          ))}
        </>
      )}

      {claim.raw_835_data && (
        <details style={{ marginTop: 12 }}>
          <summary style={{ cursor: 'pointer', color: 'var(--gray-500)', fontSize: '0.85rem' }}>
            Raw 835 data (JSON)
          </summary>
          <pre style={{
            background: 'var(--gray-50)', padding: 12, borderRadius: 'var(--radius)',
            overflowX: 'auto', fontSize: '0.8rem', marginTop: 8,
          }}>
            {JSON.stringify(claim.raw_835_data, null, 2)}
          </pre>
        </details>
      )}
    </div>
  )
}

export default function Claims() {
  const [claims, setClaims] = useState<Claim[]>([])
  const [statusFilter, setStatusFilter] = useState('')
  const [search, setSearch] = useState('')
  const [loading, setLoading] = useState(true)
  const [expandedId, setExpandedId] = useState<string | null>(null)
  const [details, setDetails] = useState<Record<string, ClaimDetail>>({})   // claim.id -> full claim record
  const [detailLoading, setDetailLoading] = useState<string | null>(null)
  const [detailError, setDetailError] = useState<string | null>(null)

  const loadClaims = () => {
    const params = new URLSearchParams({ limit: '50' })
    if (statusFilter) params.set('status', statusFilter)
    if (search.trim()) params.set('q', search.trim())

    fetch(`${API_BASE}/claims?${params}`)
      .then(r => r.json())
      .then((data: unknown) => {
        const list = Array.isArray(data) ? data as Claim[] : []
        setClaims(list)
        // The list moved, so any expanded panel must refetch to stay truthful.
        setDetails({})
        setDetailError(null)
        setLoading(false)
        if (expandedId && list.some(c => c.id === expandedId)) loadDetail(expandedId, true)
      })
      .catch(err => { console.error(err); setLoading(false) })
  }

  const loadDetail = (id: string, force = false) => {
    if (!force && details[id]) return
    setDetailLoading(id)
    setDetailError(null)
    fetch(`${API_BASE}/claims/${id}`)
      .then(r => { if (!r.ok) throw new Error(`HTTP ${r.status}`); return r.json() })
      .then((data: unknown) => setDetails(prev => ({ ...prev, [id]: data as ClaimDetail })))
      .catch((err: unknown) => setDetailError(err instanceof Error ? err.message : 'Could not load claim details'))
      .finally(() => setDetailLoading(prev => (prev === id ? null : prev)))
  }

  const toggleExpand = (claim: Claim) => {
    if (expandedId === claim.id) {
      setExpandedId(null)
      return
    }
    setExpandedId(claim.id)
    setDetailError(null)
    if (!details[claim.id]) loadDetail(claim.id)
  }

  // Debounced so a search is one request, not one per keystroke.
  useEffect(() => {
    const t = setTimeout(loadClaims, search ? 300 : 0)
    return () => clearTimeout(t)
  }, [statusFilter, search])

  return (
    <div className="page-body">
      <AiStatusBanner />
      <section className="workspace-heading">
        <div>
          <p>Claim inventory</p>
          <h1>See every claim in context.</h1>
          <span>Search the full lifecycle, review payment status, and open the records that need attention.</span>
        </div>
      </section>
      <div className="filters-bar denials-filters">
        <select className="form-select" value={statusFilter} onChange={e => setStatusFilter(e.target.value)}>
          <option value="">All Statuses</option>
          <option value="ingested">Ingested</option>
          <option value="parsed">Parsed</option>
          <option value="denied">Denied</option>
          <option value="partially_paid">Partially Paid</option>
          <option value="resolved">Resolved</option>
          <option value="appealed">Appealed</option>
        </select>
        <input className="form-input" style={{ maxWidth: 280 }}
               placeholder="Search claim number, patient, ID…"
               value={search} onChange={e => setSearch(e.target.value)} />
        {(search || statusFilter) && (
          <button className="btn" onClick={() => { setSearch(''); setStatusFilter('') }}>Clear</button>
        )}
        <button className="btn btn-primary" onClick={loadClaims}>Refresh</button>
      </div>

      <div className="card">
        <div className="table-container">
          <table>
            <thead>
              <tr>
                <th style={{ width: 36 }} />
                <th>Claim #</th>
                <th>Patient</th>
                <th>Payer</th>
                <th>Charge</th>
                <th>Paid</th>
                <th>Adjustment</th>
                <th>Status</th>
                <th>Denials</th>
                <th>Date</th>
              </tr>
            </thead>
            <tbody>
              {claims.length === 0 ? (
                <tr><td colSpan={10} style={{ textAlign: 'center', padding: 20 }}>No claims found</td></tr>
              ) : (
                claims.map(c => {
                  const open = expandedId === c.id
                  return (
                    <React.Fragment key={c.id}>
                      <tr style={open ? { background: 'var(--gray-50)' } : undefined}>
                        <td>
                          <button
                            className="btn btn-sm"
                            aria-expanded={open}
                            aria-label={`${open ? 'Collapse' : 'Expand'} claim ${c.claim_number}`}
                            title={open ? 'Collapse full details' : 'Expand full claim details'}
                            onClick={() => toggleExpand(c)}
                          >
                            {open ? '▾' : '▸'}
                          </button>
                        </td>
                        <td>{c.claim_number}</td>
                        <td>{c.patient_name || c.patient_id}</td>
                        <td>{c.payer_name}</td>
                        <td>{formatCurrency(c.total_charge)}</td>
                        <td>{formatCurrency(c.total_paid)}</td>
                        <td>{formatCurrency(c.total_adjustment)}</td>
                        <td><span className={`badge badge-${(c.status || 'unknown').replace(/_/g, '-')}`}>{(c.status || 'unknown').replace(/_/g, ' ')}</span></td>
                        <td>{denialProgress(c)}</td>
                        <td>{formatDate(c.created_at)}</td>
                      </tr>
                      {open && (
                        <tr>
                          <td colSpan={10} style={{ padding: 0, background: 'var(--gray-50)' }}>
                            {detailError ? (
                              <div style={{ padding: 16 }}>
                                <div className="callout callout-danger">
                                  <strong>Could not load claim details.</strong> {detailError}
                                </div>
                                <button className="btn btn-sm" onClick={() => loadDetail(c.id, true)}>Retry</button>
                              </div>
                            ) : !details[c.id] ? (
                              <div style={{ padding: 16, color: 'var(--gray-500)' }}>
                                {detailLoading === c.id ? 'Loading full claim details…' : ' '}
                              </div>
                            ) : (
                              <ClaimDetails
                                claim={details[c.id]}
                                refreshing={detailLoading === c.id}
                                onRefresh={() => loadDetail(c.id, true)}
                              />
                            )}
                          </td>
                        </tr>
                      )}
                    </React.Fragment>
                  )
                })
              )}
            </tbody>
          </table>
        </div>
      </div>
    </div>
  )
}
