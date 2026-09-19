import React, { useState, useEffect } from 'react'

const API_BASE = '/api/v1'

type Denial = Record<string, any> & { id: string; claim_number?: string; status?: string; charge_amount?: number }
type QueueFilters = { status?: string; carc_code?: string; payer_name?: string; min_amount?: string; max_amount?: string; min_age_days?: string; max_age_days?: string; owner?: string; facility_type_code?: string; cursor?: string; q?: string; sort?: string; descending?: boolean }
type CarcOption = { carc_code: string; description?: string; denial_count?: number }
type Notice = { error: boolean; text: string }
type SavedView = { name: string; status?: string; carc?: string; payer?: string; minAmount?: string; maxAmount?: string; minAgeDays?: string; maxAgeDays?: string; owner?: string; facility?: string; search?: string; sort?: string; descending?: boolean }
type Citation = { evidence_id: string; document_id: string; document_title: string; source_type: string; chunk_index: number }

function formatCurrency(value?: number) {
  return new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD' }).format(value || 0)
}

function formatDate(dateStr?: string) {
  if (!dateStr) return '—'
  return new Date(dateStr).toLocaleDateString('en-US')
}

export default function Denials() {
  const [denials, setDenials] = useState<Denial[]>([])
  const [nextCursor, setNextCursor] = useState<string | null>(null)
  const [selectedDenial, setSelectedDenial] = useState<Denial | null>(null)
  const [showDetail, setShowDetail] = useState(false)
  const [statusFilter, setStatusFilter] = useState('')
  const [carcFilter, setCarcFilter] = useState('')
  const [payerFilter, setPayerFilter] = useState('')
  const [minAmount, setMinAmount] = useState('')
  const [maxAmount, setMaxAmount] = useState('')
  const [minAgeDays, setMinAgeDays] = useState('')
  const [maxAgeDays, setMaxAgeDays] = useState('')
  const [ownerFilter, setOwnerFilter] = useState('')
  const [facilityFilter, setFacilityFilter] = useState('')
  const [search, setSearch] = useState('')
  const [sort, setSort] = useState('amount')
  const [descending, setDescending] = useState(true)
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const [bulkType, setBulkType] = useState('')
  const [bulkBusy, setBulkBusy] = useState(false)
  const [carcOptions, setCarcOptions] = useState<CarcOption[]>([])
  const [generatingId, setGeneratingId] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [notice, setNotice] = useState<Notice | null>(null)
  const [queueing, setQueueing] = useState(false)
  const [loading, setLoading] = useState(true)
  const savedViews: SavedView[] = JSON.parse(localStorage.getItem('openclaim.denialViews') || '[]')

  const saveCurrentView = () => {
    const name = window.prompt('Name this saved queue')
    if (!name?.trim()) return
    const next = [...savedViews.filter(v => v.name !== name.trim()), {
      name: name.trim(), status: statusFilter, carc: carcFilter, payer: payerFilter, minAmount, maxAmount, minAgeDays, maxAgeDays, owner: ownerFilter, facility: facilityFilter, search, sort, descending,
    }]
    localStorage.setItem('openclaim.denialViews', JSON.stringify(next))
    setNotice({ error: false, text: `Saved queue “${name.trim()}”.` })
  }

  const loadDenials = (filters: QueueFilters = {}, { append = false }: { append?: boolean } = {}) => {
    const params = new URLSearchParams({ limit: '50' })
    if (filters.status) params.set('status', filters.status)
    if (filters.carc_code) params.set('carc_code', filters.carc_code)
    if (filters.payer_name) params.set('payer_name', filters.payer_name)
    if (filters.min_amount) params.set('min_amount', filters.min_amount)
    if (filters.max_amount) params.set('max_amount', filters.max_amount)
    if (filters.min_age_days) params.set('min_age_days', filters.min_age_days)
    if (filters.max_age_days) params.set('max_age_days', filters.max_age_days)
    if (filters.owner) params.set('owner', filters.owner)
    if (filters.facility_type_code) params.set('facility_type_code', filters.facility_type_code)
    if (filters.cursor) params.set('cursor', filters.cursor)
    if (filters.q) params.set('q', filters.q)
    if (filters.sort) params.set('sort', filters.sort)
    if (filters.descending) params.set('descending', 'true')

    fetch(`${API_BASE}/denials?${params}`)
      .then(r => r.json())
      .then((data: unknown) => { const page = data as { items?: Denial[]; next_cursor?: string }; const items = Array.isArray(data) ? data as Denial[] : (page.items || []); setDenials(current => append ? [...current, ...items] : items); setNextCursor(page.next_cursor || null); setSelected(new Set()); setLoading(false) })
      .catch(err => { console.error(err); setLoading(false) })
  }

  // Re-query whenever a filter changes. Previously the selects only set
  // state and nothing reloaded until the Filter button was pressed, so
  // choosing a filter appeared to do nothing.
  useEffect(() => {
    setLoading(true)
    const t = setTimeout(
      () => loadDenials({ status: statusFilter, carc_code: carcFilter, payer_name: payerFilter, min_amount: minAmount, max_amount: maxAmount, min_age_days: minAgeDays, max_age_days: maxAgeDays, owner: ownerFilter, facility_type_code: facilityFilter, q: search, sort, descending }),
      search ? 300 : 0,
    )
    return () => clearTimeout(t)
  }, [statusFilter, carcFilter, payerFilter, minAmount, maxAmount, minAgeDays, maxAgeDays, ownerFilter, facilityFilter, search, sort, descending])

  // The CARC list is built from the codes actually present in the data.
  // It used to be hard-coded to five codes that mostly never occur.
  //
  // It also has to follow the status filter: the counts are a promise about
  // how many rows picking that code will show, so they have to be counted
  // over the same denials the table is listing. Counting denials already sent
  // to Appeals or the Worklist made every count read high.
  useEffect(() => {
    const params = new URLSearchParams()
    if (statusFilter) params.set('status', statusFilter)
    fetch(`${API_BASE}/denials/carc-options?${params}`)
      .then(r => r.json())
      .then((data: unknown) => setCarcOptions(Array.isArray(data) ? data as CarcOption[] : []))
      .catch(err => console.error('CARC options failed:', err))
  }, [statusFilter])

  const handleGenerateAnalysis = async (denialId: string) => {
    // Track the specific row: a single `generating` flag disabled every AI
    // button on the page while one request was in flight.
    setGeneratingId(denialId)
    setError(null)
    try {
      const resp = await fetch(`${API_BASE}/analyses/generate`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ denial_id: denialId }),
      })
      const result = await resp.json()
      if (resp.ok) {
        loadDenials({ status: statusFilter, carc_code: carcFilter })
        if (selectedDenial?.id === denialId) {
          try {
            const detailResponse = await fetch(`${API_BASE}/denials/${denialId}`)
            const detail = await detailResponse.json()
            if (!detailResponse.ok) throw new Error(detail?.detail || `Could not refresh denial details (HTTP ${detailResponse.status})`)
            setSelectedDenial(detail)
          } catch (err) {
            console.error('Denial detail refresh failed:', err)
            setError(err instanceof Error ? `Analysis generated, but ${err.message}` : 'Analysis generated, but denial details could not be refreshed')
          }
        }
      } else {
        // A failure used to be swallowed entirely, so the button looked inert.
        setError(result?.detail || `Analysis failed (HTTP ${resp.status})`)
      }
    } catch (err) {
      console.error('Analysis generation failed:', err)
      setError(err instanceof Error ? err.message : 'Analysis request failed')
    }
    setGeneratingId(null)
  }

  // Map the AI's required_action onto the resolution types the queue accepts,
  // so the recommendation becomes a unit of work rather than advice. This is a
  // fallback: the server sends `recommended_resolution`, which also applies the
  // PR rule (a patient-responsibility balance is billed, never written off).
  const RESOLUTION_FOR: Record<string, string> = {
    appeal: 'appeal_letter',
    coding_correction: 'corrected_claim',
    clinical_documentation: 'clinical_docs',
    bill_patient: 'bill_patient',
    no_action_required: 'write_off',
  }

  // Which tab a queued item lands on. Mirrors APPEAL_RESOLUTION_TYPES in
  // crates/domain/src/lib.rs — only a letter to the payer is an appeal.
  const APPEAL_TYPES = ['appeal_letter']
  const WORKLIST_TYPES = ['corrected_claim', 'clinical_docs', 'payer_contact', 'bill_patient', 'write_off']
  const destinationFor = (t?: string) => (t && APPEAL_TYPES.includes(t) ? 'Appeals' : 'Worklist')
  const LABELS: Record<string, string> = {
    appeal_letter: '⚖️ Appeal letter',
    corrected_claim: '✏️ Corrected claim',
    clinical_docs: '📄 Clinical documentation',
    payer_contact: '📞 Payer contact',
    bill_patient: '🧾 Bill patient',
    write_off: '🗑️ Write-off',
  }

  const handleQueueAppeal = async (denial: Denial, resolutionType: string) => {
    setQueueing(true)
    setNotice(null)
    try {
      const resp = await fetch(`${API_BASE}/appeals`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          denial_id: denial.id,
          ai_analysis_id: denial.ai_analysis_id || null,
          resolution_type: resolutionType,
        }),
      })
      const data = await resp.json()
      setNotice(resp.ok
        ? { error: false, text: `Queued as ${resolutionType.replace(/_/g, ' ')}. Track it on the ${destinationFor(resolutionType)} page.` }
        : { error: true, text: data?.detail || `Could not queue (HTTP ${resp.status})` })
      if (resp.ok) {
        loadDenials({ status: statusFilter, carc_code: carcFilter })
        setSelectedDenial(prev => (prev ? { ...prev, appeal_id: data.id, appeal_status: data.outcome_status } : prev))
      }
    } catch (err) {
      setNotice({ error: true, text: err instanceof Error ? err.message : 'Could not queue denial' })
    }
    setQueueing(false)
  }

  const toggle = (id: string) => setSelected(prev => {
    const next = new Set(prev)
    next.has(id) ? next.delete(id) : next.add(id)
    return next
  })

  const handleBulkQueue = async () => {
    if (!bulkType || selected.size === 0) return
    if (!confirm(`Queue ${selected.size} denial(s) as ${bulkType.replace(/_/g, ' ')}?`)) return
    setBulkBusy(true)
    setNotice(null)
    try {
      const resp = await fetch(`${API_BASE}/appeals/bulk`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ denial_ids: [...selected], resolution_type: bulkType }),
      })
      const data = await resp.json()
      if (!resp.ok) throw new Error(data.detail || 'Bulk queue failed')
      // Partial success is the normal case, so report both halves rather than
      // a bare "done" that hides what did not happen.
      const skipped = data.not_queued || []
      setNotice({
        error: false,
        text: `${data.queued} queued to the ${destinationFor(bulkType)} tab`
          + (data.skipped ? `; ${data.skipped} skipped (${[...new Set(skipped.map((s: { reason?: string }) => s.reason))].join(', ')})` : ''),
      })
      setSelected(new Set())
      setBulkType('')
      loadDenials({ status: statusFilter, carc_code: carcFilter, q: search })
    } catch (err) {
      setNotice({ error: true, text: err instanceof Error ? err.message : 'Bulk queue failed' })
    }
    setBulkBusy(false)
  }

  const handleSelectDenial = async (denial: Denial) => {
    const resp = await fetch(`${API_BASE}/denials/${denial.id}`)
    const data = await resp.json()
    setSelectedDenial(data)
    setShowDetail(true)
  }

  const carcChoices = carcFilter && !carcOptions.some(o => o.carc_code === carcFilter)
    ? [...carcOptions, { carc_code: carcFilter, description: 'No matches under this status filter', denial_count: 0 }]
    : carcOptions

  const recommended = selectedDenial?.recommended_resolution
    || (selectedDenial?.required_action ? RESOLUTION_FOR[selectedDenial.required_action] : null)

  return (
    <div className="page-body">
      <div className="filters-bar">
        <select className="form-select" value={statusFilter} onChange={e => setStatusFilter(e.target.value)}>
          <option value="">Active Only</option>
          <option value="open">Open</option>
          <option value="analyzed">Analyzed</option>
          <option value="in_progress">In Progress (worklist)</option>
          <option value="in_appeal">In Appeal</option>
          <option value="appealed">Appealed/Resolved</option>
        </select>
        <select className="form-select" value={carcFilter} onChange={e => setCarcFilter(e.target.value)}>
          <option value="">All CARC Codes ({carcChoices.length})</option>
          {carcChoices.map(o => (
            <option key={o.carc_code} value={o.carc_code}>
              {o.carc_code} — {o.description} ({o.denial_count})
            </option>
          ))}
        </select>
        <input className="form-input" style={{ maxWidth: 260 }}
               placeholder="Search claim, patient, CPT, CARC…"
               value={search} onChange={e => setSearch(e.target.value)} />
        <input className="form-input" style={{ maxWidth: 180 }} placeholder="Payer" value={payerFilter} onChange={e => setPayerFilter(e.target.value)} />
        <input className="form-input" style={{ maxWidth: 110 }} type="number" min="0" placeholder="Min $" value={minAmount} onChange={e => setMinAmount(e.target.value)} />
        <input className="form-input" style={{ maxWidth: 110 }} type="number" min="0" placeholder="Max $" value={maxAmount} onChange={e => setMaxAmount(e.target.value)} />
        <input className="form-input" style={{ maxWidth: 110 }} type="number" min="0" placeholder="Min age" value={minAgeDays} onChange={e => setMinAgeDays(e.target.value)} />
        <input className="form-input" style={{ maxWidth: 110 }} type="number" min="0" placeholder="Max age" value={maxAgeDays} onChange={e => setMaxAgeDays(e.target.value)} />
        <select className="form-select" value={ownerFilter} onChange={e => setOwnerFilter(e.target.value)}><option value="">Any owner</option><option value="unassigned">Unassigned</option></select>
        <input className="form-input" style={{ maxWidth: 110 }} placeholder="Facility" value={facilityFilter} onChange={e => setFacilityFilter(e.target.value)} />
        <select className="form-select" value={sort} onChange={e => setSort(e.target.value)} aria-label="Sort denials">
          <option value="amount">Amount</option><option value="deadline">Appeal deadline</option><option value="created">Created</option>
        </select>
        <button className="btn" onClick={() => setDescending(value => !value)} title="Toggle sort direction">
          {descending ? '↓ Descending' : '↑ Ascending'}
        </button>
        <button className="btn" onClick={() => { setStatusFilter(''); setCarcFilter(''); setPayerFilter(''); setMinAmount(''); setMaxAmount(''); setMinAgeDays(''); setMaxAgeDays(''); setOwnerFilter(''); setFacilityFilter(''); setSearch('') }}>Clear</button>
        <button className="btn" onClick={saveCurrentView}>Save view</button>
        {savedViews.length > 0 && <select className="form-select" defaultValue="" onChange={e => {
          const view = savedViews.find(v => v.name === e.target.value)
          if (view) { setStatusFilter(view.status || ''); setCarcFilter(view.carc || ''); setPayerFilter(view.payer || ''); setMinAmount(view.minAmount || ''); setMaxAmount(view.maxAmount || ''); setMinAgeDays(view.minAgeDays || ''); setMaxAgeDays(view.maxAgeDays || ''); setOwnerFilter(view.owner || ''); setFacilityFilter(view.facility || ''); setSearch(view.search || ''); setSort(view.sort || 'amount'); setDescending(view.descending !== false) }
        }}><option value="">Saved views…</option>{savedViews.map(view => <option key={view.name} value={view.name}>{view.name}</option>)}</select>}
      </div>

      {notice && (
        <div className="card" style={{ marginBottom: 12, borderLeft: `4px solid ${notice.error ? 'var(--danger)' : 'var(--success)'}` }}>
          <div className="card-body" style={{ color: notice.error ? 'var(--danger)' : 'var(--success-text)' }}>{notice.text}</div>
        </div>
      )}

      {error && (
        <div className="card" style={{ marginBottom: 12, borderLeft: '4px solid var(--danger)' }}>
          <div className="card-body" style={{ color: 'var(--danger)' }}>
            <strong>AI analysis failed:</strong> {error}
          </div>
        </div>
      )}

      {/* Denials arrive in clusters that share one reason code, and the
          decision for the cluster is usually one decision. */}
      {selected.size > 0 && (
        <div className="card" style={{ marginBottom: 12 }}>
          <div className="card-body" style={{ display: 'flex', alignItems: 'center', gap: 12, flexWrap: 'wrap' }}>
            <strong>{selected.size} selected</strong>
            <select className="form-select" style={{ maxWidth: 240 }}
                    value={bulkType} onChange={e => setBulkType(e.target.value)}>
              <option value="">Queue all as…</option>
              {[...APPEAL_TYPES, ...WORKLIST_TYPES].map(t => (
                <option key={t} value={t}>{LABELS[t]}</option>
              ))}
            </select>
            <button className="btn btn-primary" disabled={!bulkType || bulkBusy} onClick={handleBulkQueue}>
              {bulkBusy ? 'Queueing…' : `Queue ${selected.size}`}
            </button>
            <button className="btn" onClick={() => setSelected(new Set())}>Clear selection</button>
            {bulkType && (
              <span style={{ color: 'var(--gray-500)', fontSize: '0.85rem' }}>
                → {destinationFor(bulkType)} tab
              </span>
            )}
          </div>
        </div>
      )}

      <div className="card">
        <div className="table-container">
          <table>
            <thead>
              <tr>
                <th style={{ width: 32 }}>
                  <input type="checkbox"
                         title="Select everything shown"
                         checked={denials.length > 0 && selected.size === denials.length}
                         onChange={e => setSelected(e.target.checked ? new Set(denials.map(d => d.id)) : new Set())} />
                </th>
                <th>Claim #</th>
                <th>Patient</th>
                <th>Payer</th>
                <th>Facility</th>
                <th>CPT</th>
                <th>CARC</th>
                <th>Amount</th>
                <th>Status</th>
                <th>Deadline</th>
                <th>Actions</th>
              </tr>
            </thead>
            <tbody>
              {denials.length === 0 ? (
                <tr><td colSpan={11} style={{ textAlign: 'center', padding: 20 }}>No denials found</td></tr>
              ) : (
                denials.map(d => (
                  <tr key={d.id} style={{ cursor: 'pointer' }} onClick={() => handleSelectDenial(d)}>
                    <td onClick={e => e.stopPropagation()}>
                      <input type="checkbox" checked={selected.has(d.id)} onChange={() => toggle(d.id)} />
                    </td>
                    <td>{d.claim_number}</td>
                    <td>{d.patient_name || '—'}</td>
                    <td>{d.payer_name}</td>
                    <td>{d.facility_type_code || '—'}</td>
                    <td>{d.cpt_code || '—'}</td>
                    <td>{d.carc_code || '—'}</td>
                    <td>{formatCurrency(d.charge_amount)}</td>
                    <td><span className={`badge badge-${(d.status || 'open').replace(/ /g, '-')}`}>{d.status || 'open'}</span></td>
                    <td>{formatDate(d.appeal_deadline)}</td>
                    <td>
                      <button
                        className="btn btn-sm btn-primary"
                        onClick={e => { e.stopPropagation(); handleGenerateAnalysis(d.id) }}
                        disabled={generatingId === d.id}
                      >
                        {generatingId === d.id ? '⏳' : '🤖 AI'}
                      </button>
                    </td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>
      </div>
      {nextCursor && <div style={{ marginTop: 12, textAlign: 'center' }}><button className="btn" onClick={() => loadDenials({ status: statusFilter, carc_code: carcFilter, payer_name: payerFilter, min_amount: minAmount, max_amount: maxAmount, min_age_days: minAgeDays, max_age_days: maxAgeDays, owner: ownerFilter, facility_type_code: facilityFilter, q: search, sort, descending, cursor: nextCursor }, { append: true })}>Load more</button></div>}

      {/* Detail Modal */}
      {showDetail && selectedDenial && (
        <div className="modal-overlay" onClick={() => setShowDetail(false)}>
          <div className="modal" onClick={e => e.stopPropagation()}>
            <div className="modal-header">
              <h3>Denial Details — {selectedDenial.claim_number}</h3>
              <button className="btn" onClick={() => setShowDetail(false)}>✕</button>
            </div>
            <div className="modal-body">
              {/* Patient & Claim Info */}
              <div className="detail-grid">
                <div className="detail-item">
                  <div className="detail-label">Patient</div>
                  <div className="detail-value">{selectedDenial.patient_name || '—'}</div>
                </div>
                <div className="detail-item">
                  <div className="detail-label">Payer</div>
                  <div className="detail-value">{selectedDenial.payer_name}</div>
                </div>
                <div className="detail-item">
                  <div className="detail-label">CPT Code</div>
                  <div className="detail-value">{selectedDenial.cpt_code || '—'}</div>
                </div>
                <div className="detail-item">
                  <div className="detail-label">Charge</div>
                  <div className="detail-value">{formatCurrency(selectedDenial.charge_amount)}</div>
                </div>
                <div className="detail-item">
                  <div className="detail-label">CARC</div>
                  <div className="detail-value">{selectedDenial.carc_code} — {selectedDenial.carc_description || '—'}</div>
                </div>
                <div className="detail-item">
                  <div className="detail-label">Diagnosis</div>
                  <div className="detail-value">{selectedDenial.icd_10_codes?.join(', ') || '—'}</div>
                </div>
              </div>

              {/* AI Analysis */}
              {selectedDenial.explanation && (
                <div style={{ marginBottom: 20 }}>
                  <h4 style={{ marginBottom: 8 }}>🤖 AI Analysis</h4>
                  <p style={{ color: 'var(--gray-500)', fontSize: '0.8rem', margin: '0 0 10px' }}>
                    AI-generated and <strong>advisory only</strong> — the AI can be wrong.
                    Verify against the claim and payer rules before acting.
                  </p>
                  <div className="card">
                    <div className="card-body">
                      <p style={{ marginBottom: 12 }}><strong>Explanation:</strong> {selectedDenial.explanation}</p>
                      <p style={{ marginBottom: 12 }}><strong>Category:</strong> {selectedDenial.denial_category}</p>
                      {selectedDenial.required_action && (
                        <p style={{ marginBottom: 12 }}>
                          <strong>Required action:</strong>{' '}
                          <span className="badge">{selectedDenial.required_action.replace(/_/g, ' ')}</span>
                        </p>
                      )}
                      {selectedDenial.confidence_score && (
                        <p style={{ marginBottom: 12 }}><strong>Confidence:</strong> {Math.round(selectedDenial.confidence_score * 100)}%</p>
                      )}
                      {Array.isArray(selectedDenial.steps) && selectedDenial.steps.length > 0 && (
                        <div>
                          <strong>Resolution steps:</strong>
                          <ol style={{ marginTop: 6, paddingLeft: 20 }}>
                            {selectedDenial.steps.map((s, i) => (
                              <li key={i} style={{ marginBottom: 4 }}>
                                {typeof s === 'string' ? s : s.action}
                              </li>
                            ))}
                          </ol>
                        </div>
                      )}
                      {Array.isArray(selectedDenial.citations) && selectedDenial.citations.length > 0 && (
                        <div style={{ marginTop: 12 }}>
                          <strong>References used:</strong>
                          <ul style={{ marginTop: 6, paddingLeft: 20 }}>
                            {(selectedDenial.citations as Citation[]).map(citation => (
                              <li key={citation.evidence_id} style={{ marginBottom: 4 }}>
                                {citation.document_title} ({citation.source_type}, chunk {citation.chunk_index})
                              </li>
                            ))}
                          </ul>
                        </div>
                      )}
                    </div>
                  </div>
                </div>
              )}

              {/* Queue this denial as work. Appeals and everything else are
                  offered separately, because they go to different tabs. */}
              {selectedDenial.ai_analysis_id && (
                <div style={{ marginBottom: 20 }}>
                  <h4 style={{ marginBottom: 8 }}>📋 Queue this denial as work</h4>
                  {selectedDenial.appeal_id ? (
                    <p style={{ color: 'var(--gray-500)' }}>
                      Already queued as{' '}
                      <strong>{(selectedDenial.appeal_resolution_type || 'work').replace(/_/g, ' ')}</strong>
                      {' '}— status{' '}
                      <span className="badge">{selectedDenial.appeal_status || 'queued'}</span>.
                      Track it on the {destinationFor(selectedDenial.appeal_resolution_type)} page.
                    </p>
                  ) : (
                    <div>
                      {recommended && (
                        <p style={{ color: 'var(--gray-500)', marginBottom: 10 }}>
                          Recommended: <strong>{LABELS[recommended] || recommended}</strong>
                          {' '}— it will go to the {destinationFor(recommended)} tab.
                        </p>
                      )}
                      {/* The offered action differs from what the analysis
                          literally said. Say so rather than quietly overruling
                          it — the biller should know the two disagree. */}
                      {selectedDenial.recommendation_note && (
                        <div className="callout callout-warning" style={{ marginBottom: 10, fontSize: '0.85rem' }}>
                          {selectedDenial.recommendation_note}
                          {selectedDenial.required_action && (
                            <div style={{ marginTop: 6, opacity: 0.85 }}>
                              The analysis said: <em>{selectedDenial.required_action.replace(/_/g, ' ')}</em>.
                            </div>
                          )}
                        </div>
                      )}
                      {selectedDenial.cagc === 'PR' && (
                        <p style={{ color: 'var(--warning-text)', marginBottom: 10, fontSize: '0.9rem' }}>
                          🧾 This is a <strong>PR (Patient Responsibility)</strong> adjustment —
                          {' '}{selectedDenial.carc_description || 'the payer assigned this balance to the patient'}.
                          {' '}The balance is collectible and belongs on the patient's statement, not written off.
                        </p>
                      )}

                      <div style={{ marginBottom: 12 }}>
                        <div style={{ fontSize: '0.8rem', textTransform: 'uppercase', letterSpacing: 1, color: 'var(--gray-500)', marginBottom: 6 }}>
                          Appeal the payer's decision → Appeals tab
                        </div>
                        <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
                          {APPEAL_TYPES.map(t => (
                            <button key={t} className={`btn btn-sm ${t === recommended ? 'btn-primary' : ''}`}
                                    disabled={queueing}
                                    onClick={() => handleQueueAppeal(selectedDenial, t)}>
                              {queueing ? 'Queueing…' : LABELS[t]}{t === recommended ? ' (recommended)' : ''}
                            </button>
                          ))}
                        </div>
                      </div>

                      <div>
                        <div style={{ fontSize: '0.8rem', textTransform: 'uppercase', letterSpacing: 1, color: 'var(--gray-500)', marginBottom: 6 }}>
                          Work the denial without appealing → Worklist tab
                        </div>
                        <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
                          {WORKLIST_TYPES.map(t => (
                            <button key={t} className={`btn btn-sm ${t === recommended ? 'btn-primary' : ''}`}
                                    disabled={queueing}
                                    onClick={() => handleQueueAppeal(selectedDenial, t)}>
                              {queueing ? 'Queueing…' : LABELS[t]}{t === recommended ? ' (recommended)' : ''}
                            </button>
                          ))}
                        </div>
                      </div>
                    </div>
                  )}
                </div>
              )}

              {/* Appeal Letter */}
              {selectedDenial.draft_appeal_letter && (
                <div>
                  <h4 style={{ marginBottom: 8 }}>📝 Draft Appeal Letter</h4>
                  <div className="appeal-letter">
                    {selectedDenial.draft_appeal_letter}
                  </div>
                </div>
              )}
            </div>
            <div className="modal-footer">
              <button className="btn" onClick={() => setShowDetail(false)}>Close</button>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
