import React, { useState, useEffect } from 'react'
import { useSearchParams } from 'react-router-dom'
import { useAuth } from '../contexts/AuthContext'
import LcdImportPanel from '../components/LcdImportPanel'

const API_BASE = '/api/v1'
const EXPIRING_SOON_DAYS = 30

// The source types a curator can file a document under. Shared by the paste
// form and the upload panel so the two cannot drift apart.
const SOURCE_TYPES: [string, string][] = [
  ['cms_lcd', 'CMS LCD'],
  ['payer_policy', 'Payer Policy'],
  ['fee_schedule', 'Fee Schedule'],
  ['contract', 'Contract'],
  ['prior_auth_policy', 'Prior Auth Policy'],
  ['medical_necessity_criteria', 'Medical Necessity Criteria'],
]

const EMPTY_UPLOAD_META: UploadMeta = {
  source_type: 'payer_policy', payer_name: '', effective_date: '',
  expiration_date: '', jurisdiction: '', version_label: '',
}

type KnowledgeDocument = Record<string, any> & {
  id: string; title: string; source_type: string; status: string; created_at: string; chunk_count?: number
  expiration_date?: string | null; superseded_by?: string | null; superseded_by_title?: string | null
}
type NewDocument = { title: string; source_type: string; payer_name: string; effective_date: string; expiration_date: string; jurisdiction: string; version_label: string; content: string }
type UploadMeta = { source_type: string; payer_name: string; effective_date: string; expiration_date: string; jurisdiction: string; version_label: string }
type Notice = { error: boolean; text: string }
type SearchResult = Record<string, any>
type ViewingContent = { content?: string; chunk_count?: number; error?: string }

// What the server managed to pull out of the file, phrased per format.
function extractedSummary(extracted: Record<string, any> | undefined): string {
  if (!extracted) return ''
  if (extracted.pages) return ` (${extracted.pages} PDF pages, ${extracted.chars} chars extracted)`
  if (extracted.format === 'csv') return ` (${extracted.rows} rows, ${extracted.columns} columns)`
  if (extracted.format === 'spreadsheet') return ` (${extracted.rows} rows across ${extracted.sheets} sheets)`
  return ''
}

// A document whose upload staged sections that are not all embedded yet -
// a run the browser abandoned partway. The server keeps the progress and the
// staged sections, so indexing picks up where it stopped.
function resumable(d: KnowledgeDocument): boolean {
  const progress = d.metadata?.index_progress
  if (d.status !== 'pending' || !progress) return false
  return (progress.sections_total ?? 0) > (progress.sections_done ?? 0)
}

// Client-side, so a document loaded under one filter still shows its own
// correct badge if the org's clock and the filter's cutoff briefly disagree.
function expiryStatus(d: KnowledgeDocument): 'expired' | 'expiring_soon' | null {
  if (!d.expiration_date) return null
  const days = (new Date(d.expiration_date).getTime() - Date.now()) / 86400000
  if (days < 0) return 'expired'
  if (days <= EXPIRING_SOON_DAYS) return 'expiring_soon'
  return null
}

export default function KnowledgeBase() {
  const { can } = useAuth()
  // Policy documents steer every future AI analysis, so curating them is a
  // manager responsibility. Everyone can still read and search them.
  const mayCurate = can.manageKnowledge()
  const [documents, setDocuments] = useState<KnowledgeDocument[]>([])
  const [sourceFilter, setSourceFilter] = useState('')
  // Deep-linked from the Dashboard's "Policies Expiring Soon" / "Expired
  // Policies Still Active" cards (?expiry=expiring_soon|expired_active).
  const [searchParams, setSearchParams] = useSearchParams()
  const [expiryFilter, setExpiryFilter] = useState(searchParams.get('expiry') || '')
  const [loading, setLoading] = useState(true)
  const [newDoc, setNewDoc] = useState<NewDocument>({ title: '', source_type: 'payer_policy', payer_name: '', effective_date: '', expiration_date: '', jurisdiction: '', version_label: '', content: '' })
  const [searchFilters, setSearchFilters] = useState({ payer: '', jurisdiction: '', effective_on: '' })
  const [indexing, setIndexing] = useState(false)
  const [busyId, setBusyId] = useState<string | null>(null)
  const [notice, setNotice] = useState<Notice | null>(null)
  const [showForm, setShowForm] = useState(false)
  // Upload metadata is deliberately its own state, not borrowed from the
  // paste form: it used to read `newDoc`, whose fields live inside the
  // collapsed "+ Add Document" panel, so every upload silently filed itself
  // as the default Payer Policy with no way to say otherwise.
  const [showUpload, setShowUpload] = useState(false)
  const [uploadMeta, setUploadMeta] = useState<UploadMeta>(EMPTY_UPLOAD_META)
  const [indexProgress, setIndexProgress] = useState<{ done: number; total: number } | null>(null)
  const [showLcdImport, setShowLcdImport] = useState(false)
  const [searchQuery, setSearchQuery] = useState('')
  const [searchResults, setSearchResults] = useState<SearchResult[]>([])
  const [searching, setSearching] = useState(false)
  const [viewingDoc, setViewingDoc] = useState<KnowledgeDocument | null>(null)
  const [viewingContent, setViewingContent] = useState<ViewingContent | null>(null)
  const [viewingLoading, setViewingLoading] = useState(false)
  const [supersedeFor, setSupersedeFor] = useState<KnowledgeDocument | null>(null)
  const [supersedeTarget, setSupersedeTarget] = useState('')

  const loadDocuments = () => {
    const params = new URLSearchParams({ limit: '50' })
    if (sourceFilter) params.set('source_type', sourceFilter)
    if (expiryFilter) {
      params.set('expiry', expiryFilter)
      params.set('expiring_within_days', String(EXPIRING_SOON_DAYS))
    }

    fetch(`${API_BASE}/knowledge/documents?${params}`)
      .then(r => r.json())
      .then((data: unknown) => { setDocuments(Array.isArray(data) ? data as KnowledgeDocument[] : []); setLoading(false) })
      .catch(err => { console.error(err); setLoading(false) })
  }

  // Reload when a filter changes. This page has no Filter button at all, so
  // without this the selects were entirely inert.
  useEffect(() => {
    setLoading(true)
    loadDocuments()
  }, [sourceFilter, expiryFilter])

  const handleCreateDoc = async (e: React.FormEvent<HTMLFormElement>) => {
    e.preventDefault()
    setIndexing(true)
    setNotice(null)
    try {
      const resp = await fetch(`${API_BASE}/knowledge/documents`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(newDoc),
      })
      const data = await resp.json()
      if (!resp.ok) {
        setNotice({ error: true, text: data?.detail || `Create failed (HTTP ${resp.status})` })
      } else if (data.index_error) {
        setNotice({ error: true, text: `Saved, but indexing failed: ${data.index_error}` })
      } else {
        setNotice({ error: false, text: `Indexed ${data.chunks_indexed ?? 0} chunks.` })
        setShowForm(false)
        setNewDoc({ title: '', source_type: 'payer_policy', payer_name: '', effective_date: '', expiration_date: '', jurisdiction: '', version_label: '', content: '' })
      }
      loadDocuments()
    } catch (err) {
      console.error('Create failed:', err)
      setNotice({ error: true, text: err instanceof Error ? err.message : 'Create failed' })
    }
    setIndexing(false)
  }

  const handleUpload = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0]
    if (!file) return
    setIndexing(true)
    setNotice(null)
    const form = new FormData()
    form.append('file', file)
    const params = new URLSearchParams({ source_type: uploadMeta.source_type })
    if (uploadMeta.payer_name.trim()) params.set('payer_name', uploadMeta.payer_name.trim())
    if (uploadMeta.effective_date) params.set('effective_date', uploadMeta.effective_date)
    if (uploadMeta.expiration_date) params.set('expiration_date', uploadMeta.expiration_date)
    if (uploadMeta.jurisdiction.trim()) params.set('jurisdiction', uploadMeta.jurisdiction.trim())
    if (uploadMeta.version_label.trim()) params.set('version_label', uploadMeta.version_label.trim())
    try {
      const resp = await fetch(
        `${API_BASE}/knowledge/documents/upload?${params}`,
        { method: 'POST', body: form }
      )
      const data = await resp.json()
      if (!resp.ok) {
        setNotice({ error: true, text: data?.detail || `Upload failed (HTTP ${resp.status})` })
        loadDocuments()
        setIndexing(false)
        setIndexProgress(null)
        e.target.value = ''
        return
      }
      // The upload only staged the parsed sections; embedding runs in batches
      // driven from here, so a file of any size indexes in steps that each fit
      // inside the request timeout.
      loadDocuments()
      const chunks = await runIndexBatches(data.id, data.sections_total ?? 0)
      // Name the type it was filed under, so a wrong one is caught now
      // rather than the next time a denial analysis cites it oddly.
      const label = SOURCE_TYPES.find(([v]) => v === uploadMeta.source_type)?.[1]
        ?? uploadMeta.source_type
      setNotice({ error: false, text:
        `Indexed ${chunks} chunks from ${file.name}`
        + ` as ${label}${extractedSummary(data.extracted)}.` })
      setShowUpload(false)
      loadDocuments()
    } catch (err) {
      setNotice({ error: true, text: err instanceof Error ? err.message : 'Upload failed' })
    }
    setIndexing(false)
    setIndexProgress(null)
    e.target.value = ''
  }

  // Drives POST /knowledge/documents/{id}/index-batch until it reports done,
  // the same batch-per-request loop the LCD bulk import uses. Returns the
  // chunk count; throws with the server's message if a batch fails, leaving
  // the document `error` and its remaining sections staged.
  const runIndexBatches = async (
    documentId: string, sectionsTotal: number, sectionsDone = 0,
  ): Promise<number> => {
    let chunks = 0
    setIndexProgress({ done: sectionsDone, total: sectionsTotal })
    for (;;) {
      const resp = await fetch(
        `${API_BASE}/knowledge/documents/${documentId}/index-batch?limit=40`,
        { method: 'POST' }
      )
      const data = await resp.json()
      if (!resp.ok) throw new Error(data?.detail || `Indexing failed (HTTP ${resp.status})`)
      chunks = data.chunks ?? chunks
      setIndexProgress({ done: data.sections_done ?? 0, total: data.sections_total ?? sectionsTotal })
      if (data.done) break
    }
    setIndexProgress(null)
    return chunks
  }

  const handleResumeIndexing = async (doc: KnowledgeDocument) => {
    setIndexing(true)
    setNotice(null)
    try {
      const progress = doc.metadata?.index_progress
      const chunks = await runIndexBatches(
        doc.id, progress?.sections_total ?? 0, progress?.sections_done ?? 0,
      )
      setNotice({ error: false, text: `Finished indexing ${doc.title} — ${chunks} chunks.` })
    } catch (err) {
      setNotice({ error: true, text: err instanceof Error ? err.message : 'Indexing failed' })
    }
    setIndexing(false)
    setIndexProgress(null)
    loadDocuments()
  }

  const handleRetire = async (doc: KnowledgeDocument, purge: boolean) => {
    const what = purge
      ? `Permanently delete "${doc.title}"? This cannot be undone.`
      : `Archive "${doc.title}"? Its ${doc.chunk_count ?? 0} indexed chunks are removed so it stops feeding denial analysis. The record is kept.`
    if (!window.confirm(what)) return

    setBusyId(doc.id)
    setNotice(null)
    try {
      const resp = await fetch(
        `${API_BASE}/knowledge/documents/${doc.id}${purge ? '?purge=true' : ''}`,
        { method: 'DELETE' }
      )
      const data = await resp.json()
      setNotice(resp.ok
        ? { error: false, text: `${data.status === 'purged' ? 'Deleted' : 'Archived'} "${data.title}" — ${data.chunks_removed} chunks removed from the index.` }
        : { error: true, text: data?.detail || `Failed (HTTP ${resp.status})` })
      loadDocuments()
    } catch (err) {
      setNotice({ error: true, text: err instanceof Error ? err.message : 'Document update failed' })
    }
    setBusyId(null)
  }

  const handleSupersede = async (doc: KnowledgeDocument, newDocumentId: string) => {
    setBusyId(doc.id)
    setNotice(null)
    try {
      const resp = await fetch(`${API_BASE}/knowledge/documents/${doc.id}/supersede`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ new_document_id: newDocumentId }),
      })
      const data = await resp.json()
      setNotice(resp.ok
        ? { error: false, text: `"${doc.title}" now points to its replacement and expires ${data.expiration_date ? new Date(data.expiration_date).toLocaleDateString() : 'today'}.` }
        : { error: true, text: data?.detail || `Failed (HTTP ${resp.status})` })
      if (resp.ok) setSupersedeFor(null)
      loadDocuments()
    } catch (err) {
      setNotice({ error: true, text: err instanceof Error ? err.message : 'Supersede failed' })
    }
    setBusyId(null)
  }

  const handleSearch = async () => {
    if (!searchQuery.trim()) return
    setSearching(true)
    try {
      const resp = await fetch(`${API_BASE}/knowledge/search`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ query: searchQuery, top_k: 10, filters: Object.fromEntries(Object.entries(searchFilters).filter(([, value]) => value)) }),
      })
      const data = await resp.json()
      setSearchResults(Array.isArray(data.results) ? data.results as SearchResult[] : [])
    } catch (err) {
      console.error('Search failed:', err)
    }
    setSearching(false)
  }

  const handleViewDoc = async (doc: KnowledgeDocument) => {
    setViewingDoc(doc)
    setViewingLoading(true)
    setViewingContent(null)
    try {
      const resp = await fetch(`${API_BASE}/knowledge/documents/${doc.id}`)
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`)
      const data = await resp.json()
      setViewingContent(data as ViewingContent)
    } catch (err) {
      setViewingContent({ error: err instanceof Error ? err.message : 'Could not load document' })
    }
    setViewingLoading(false)
  }

  return (
    <div className="page-body">
      <h1 className="page-title">Knowledge Base</h1>
      <div className="filters-bar denials-filters">
        <select className="form-select" value={sourceFilter} onChange={e => setSourceFilter(e.target.value)}>
          <option value="">All Types</option>
          <option value="cms_lcd">CMS LCD</option>
          <option value="payer_policy">Payer Policy</option>
          <option value="fee_schedule">Fee Schedule</option>
          <option value="contract">Contract</option>
          <option value="prior_auth_policy">Prior Auth Policy</option>
          <option value="medical_necessity_criteria">Medical Necessity</option>
        </select>
        <select
          className="form-select"
          value={expiryFilter}
          onChange={e => {
            setExpiryFilter(e.target.value)
            setSearchParams(e.target.value ? { expiry: e.target.value } : {})
          }}
        >
          <option value="">Any expiration</option>
          <option value="expiring_soon">Expiring in {EXPIRING_SOON_DAYS} days</option>
          <option value="expired_active">Expired but still active</option>
        </select>
        {mayCurate && (
          <>
            <button className="btn btn-primary" onClick={() => setShowForm(!showForm)}>+ Add Document</button>
            <button className="btn" onClick={() => setShowUpload(!showUpload)}>
              {showUpload ? '✕ Cancel' : '⬆ Upload PDF / CSV / Excel / .txt / .md'}
            </button>
            <button className="btn" onClick={() => setShowLcdImport(!showLcdImport)}>
              {showLcdImport ? '✕ Cancel' : '⬆ Bulk import LCDs (CSV/MDB)'}
            </button>
          </>
        )}
      </div>

      {mayCurate && showUpload && (
        <div className="card" style={{ marginBottom: 20 }}>
          <div className="card-header"><h3>Upload a document</h3></div>
          <div className="card-body">
            <div className="form-group">
              <label>Source Type</label>
              <select
                className="form-select"
                value={uploadMeta.source_type}
                onChange={e => setUploadMeta({ ...uploadMeta, source_type: e.target.value })}
              >
                {SOURCE_TYPES.map(([value, label]) => (
                  <option key={value} value={value}>{label}</option>
                ))}
              </select>
              <div className="form-hint">
                How this file is filed and cited. A fee schedule filed as a
                payer policy still indexes, but reads as the wrong kind of
                source in an appeal.
              </div>
            </div>
            <div className="form-group">
              <label>Payer</label>
              <input
                className="form-input"
                value={uploadMeta.payer_name}
                onChange={e => setUploadMeta({ ...uploadMeta, payer_name: e.target.value })}
                placeholder="Leave blank if it applies to every payer (e.g. a CMS LCD)"
              />
              <div className="form-hint">
                Analysis for a denial only cites documents for that claim's
                payer, plus anything left blank here.
              </div>
            </div>
            <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(170px, 1fr))', gap: 12 }}>
              <div className="form-group"><label>Effective date</label><input className="form-input" type="date" value={uploadMeta.effective_date} onChange={e => setUploadMeta({ ...uploadMeta, effective_date: e.target.value })} /></div>
              <div className="form-group"><label>Expiration date</label><input className="form-input" type="date" value={uploadMeta.expiration_date} onChange={e => setUploadMeta({ ...uploadMeta, expiration_date: e.target.value })} /></div>
              <div className="form-group"><label>Jurisdiction</label><input className="form-input" value={uploadMeta.jurisdiction} onChange={e => setUploadMeta({ ...uploadMeta, jurisdiction: e.target.value })} placeholder="e.g., Noridian JF" /></div>
              <div className="form-group"><label>Version</label><input className="form-input" value={uploadMeta.version_label} onChange={e => setUploadMeta({ ...uploadMeta, version_label: e.target.value })} placeholder="e.g., 2026.1" /></div>
            </div>
            <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
              <label className="btn btn-primary" style={{ cursor: indexing ? 'default' : 'pointer' }}>
                {indexing ? 'Indexing…' : 'Choose file & index'}
                <input type="file"
                       accept=".pdf,.csv,.xlsx,.xls,.xlsb,.ods,.txt,.md,application/pdf,text/csv,application/vnd.openxmlformats-officedocument.spreadsheetml.sheet,application/vnd.ms-excel,text/plain,text/markdown"
                       style={{ display: 'none' }} onChange={handleUpload} disabled={indexing} />
              </label>
              <button type="button" className="btn" onClick={() => setUploadMeta(EMPTY_UPLOAD_META)} disabled={indexing}>Reset</button>
              <span className="form-hint" style={{ margin: 0 }}>
                The title comes from the filename. Large spreadsheets embed a
                few thousand rows and can take several minutes — leave this tab
                open until the progress bar completes.
              </span>
            </div>
          </div>
        </div>
      )}

      {/* Shown for an upload's own indexing run and for a resumed one alike. */}
      {indexProgress && (
        <div className="card" style={{ marginBottom: 20 }}>
          <div className="card-body">
            <p style={{ fontSize: '0.9rem', marginTop: 0 }}>
              Indexing {indexProgress.done} / {indexProgress.total} sections — leave this tab open.
            </p>
            <div style={{ height: 8, background: 'var(--border)', borderRadius: 4, overflow: 'hidden' }}>
              <div style={{
                height: '100%',
                width: `${indexProgress.total ? Math.round((indexProgress.done / indexProgress.total) * 100) : 0}%`,
                background: 'var(--primary)',
                transition: 'width 0.3s ease',
              }} />
            </div>
          </div>
        </div>
      )}

      {mayCurate && showLcdImport && (
        <LcdImportPanel onDone={loadDocuments} />
      )}

      {notice && (
        <div className="card" style={{ marginBottom: 12, borderLeft: `4px solid ${notice.error ? 'var(--danger)' : 'var(--success)'}` }}>
          <div className="card-body" style={{ color: notice.error ? 'var(--danger)' : 'var(--success-text)' }}>{notice.text}</div>
        </div>
      )}

      {/* New Document Form */}
      {showForm && (
        <div className="card" style={{ marginBottom: 20 }}>
          <div className="card-header"><h3>New Knowledge Document</h3></div>
          <div className="card-body">
            <form onSubmit={handleCreateDoc}>
              <div className="form-group">
                <label>Document Title</label>
                <input className="form-input" value={newDoc.title} onChange={e => setNewDoc({ ...newDoc, title: e.target.value })} placeholder="e.g., BlueCross Prior Auth Requirements 2024" />
              </div>
              <div className="form-group">
                <label>Document Text</label>
                <textarea
                  className="form-input"
                  rows={10}
                  value={newDoc.content}
                  onChange={e => setNewDoc({ ...newDoc, content: e.target.value })}
                  placeholder="Paste the policy text here. It is chunked, embedded and stored in pgvector so denial analysis can cite it."
                />
              </div>
              <div className="form-group">
                <label>Payer</label>
                <input
                  className="form-input"
                  value={newDoc.payer_name}
                  onChange={e => setNewDoc({ ...newDoc, payer_name: e.target.value })}
                  placeholder="Leave blank if it applies to every payer (e.g. a CMS LCD)"
                />
                <div className="form-hint">
                  Analysis for a denial only cites documents for that claim's payer,
                  plus anything left blank here.
                </div>
              </div>
              <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(170px, 1fr))', gap: 12 }}>
                <div className="form-group"><label>Effective date</label><input className="form-input" type="date" value={newDoc.effective_date} onChange={e => setNewDoc({ ...newDoc, effective_date: e.target.value })} /></div>
                <div className="form-group"><label>Expiration date</label><input className="form-input" type="date" value={newDoc.expiration_date} onChange={e => setNewDoc({ ...newDoc, expiration_date: e.target.value })} /></div>
                <div className="form-group"><label>Jurisdiction</label><input className="form-input" value={newDoc.jurisdiction} onChange={e => setNewDoc({ ...newDoc, jurisdiction: e.target.value })} placeholder="e.g., Noridian JF" /></div>
                <div className="form-group"><label>Version</label><input className="form-input" value={newDoc.version_label} onChange={e => setNewDoc({ ...newDoc, version_label: e.target.value })} placeholder="e.g., 2026.1" /></div>
              </div>
              <div className="form-group">
                <label>Source Type</label>
                <select className="form-select" value={newDoc.source_type} onChange={e => setNewDoc({ ...newDoc, source_type: e.target.value })}>
                  {SOURCE_TYPES.map(([value, label]) => (
                    <option key={value} value={value}>{label}</option>
                  ))}
                </select>
              </div>
              <div style={{ display: 'flex', gap: 8 }}>
                <button type="submit" className="btn btn-primary" disabled={indexing}>
                  {indexing ? 'Indexing…' : 'Create & Index'}
                </button>
                <button type="button" className="btn" onClick={() => setShowForm(false)}>Cancel</button>
              </div>
            </form>
          </div>
        </div>
      )}

      {/* Search */}
      <div className="card" style={{ marginBottom: 20 }}>
        <div className="card-body">
          <h4 style={{ marginBottom: 12 }}>🔍 Search Knowledge Base</h4>
          <div style={{ display: 'flex', gap: 8 }}>
            <input className="form-input" value={searchQuery} onChange={e => setSearchQuery(e.target.value)} placeholder="Search by CPT, diagnosis, payer policy..." />
            <button className="btn btn-primary" onClick={handleSearch} disabled={searching}>
              {searching ? 'Searching...' : 'Search'}
            </button>
          </div>
          <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(170px, 1fr))', gap: 8, marginTop: 8 }}>
            <input className="form-input" value={searchFilters.payer} onChange={e => setSearchFilters({ ...searchFilters, payer: e.target.value })} placeholder="Filter payer" />
            <input className="form-input" value={searchFilters.jurisdiction} onChange={e => setSearchFilters({ ...searchFilters, jurisdiction: e.target.value })} placeholder="Filter jurisdiction" />
            <input className="form-input" type="date" value={searchFilters.effective_on} onChange={e => setSearchFilters({ ...searchFilters, effective_on: e.target.value })} title="Effective on" />
          </div>
          {searchResults.length > 0 && (
            <div style={{ marginTop: 16 }}>
              <h4>Results ({searchResults.length})</h4>
              {searchResults.map((r, i) => (
                <div key={i} style={{ padding: 12, background: 'var(--gray-50)', borderRadius: 8, marginBottom: 8 }}>
                  <p style={{ fontSize: 0.85, color: 'var(--gray-500)' }}>{r.document_title || 'Knowledge document'} · {r.metadata?.page ? `Page ${r.metadata.page} · ` : ''}Chunk {r.chunk_index} · {r.token_count} tokens · Semantic: {Math.round(r.similarity_score * 100)}% · Keyword: {Math.round((r.keyword_score || 0) * 100)}%</p>
                  <p style={{ marginTop: 4 }}>{r.content?.substring(0, 300)}...</p>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>

      {/* Document List */}
      <div className="card">
        <div className="table-container">
          <table>
            <thead>
              <tr>
                <th>Title</th>
                <th>Type</th>
                <th>Status</th>
                <th>Chunks</th>
                <th>Expiration</th>
                <th>Created</th>
                <th>Actions</th>
              </tr>
            </thead>
            <tbody>
              {documents.length === 0 ? (
                <tr><td colSpan={7} style={{ textAlign: 'center', padding: 20 }}>No knowledge documents</td></tr>
              ) : (
                documents.map(d => {
                  const expiration = expiryStatus(d)
                  return (
                    <React.Fragment key={d.id}>
                      <tr style={d.status === 'archived' ? { opacity: 0.55 } : undefined}>
                        <td>{d.title}</td>
                        <td>{d.source_type.replace(/_/g, ' ')}</td>
                        <td><span className={`badge badge-${d.status}`}>{d.status}</span></td>
                        <td>{d.chunk_count ?? 0}</td>
                        <td>
                          {d.expiration_date ? new Date(d.expiration_date).toLocaleDateString() : '—'}
                          {expiration && d.status !== 'archived' && (
                            <span className="badge" style={{
                              marginLeft: 6,
                              background: expiration === 'expired' ? 'var(--danger)' : 'var(--warning)',
                              color: '#fff',
                            }}>
                              {expiration === 'expired' ? 'expired' : `≤${EXPIRING_SOON_DAYS}d`}
                            </span>
                          )}
                          {d.superseded_by && (
                            <div style={{ fontSize: '0.75rem', color: 'var(--text-muted)' }}>
                              superseded by {d.superseded_by_title || d.superseded_by}
                            </div>
                          )}
                        </td>
                        <td>{new Date(d.created_at).toLocaleDateString()}</td>
                        <td style={{ whiteSpace: 'nowrap' }}>
                          <button className="btn btn-sm"
                                  title="View document content"
                                  onClick={() => handleViewDoc(d)}>
                            👁 View
                          </button>
                          {mayCurate && resumable(d) && (
                            <button className="btn btn-sm" disabled={indexing || busyId === d.id}
                                    title="Finish indexing the sections this upload has not embedded yet"
                                    onClick={() => handleResumeIndexing(d)}
                                    style={{ marginLeft: 6 }}>
                              ▶ Resume indexing
                            </button>
                          )}
                          {mayCurate && d.status !== 'archived' && (
                            <button className="btn btn-sm" disabled={busyId === d.id}
                                    title="Remove from the index but keep the record"
                                    onClick={() => handleRetire(d, false)}
                                    style={{ marginLeft: 6 }}>
                              {busyId === d.id ? '…' : '📦 Archive'}
                            </button>
                          )}
                          {mayCurate && !d.superseded_by && (
                            <button className="btn btn-sm" disabled={busyId === d.id}
                                    title="Link the document that replaced this one, and expire this one"
                                    onClick={() => { setSupersedeFor(supersedeFor?.id === d.id ? null : d); setSupersedeTarget('') }}
                                    style={{ marginLeft: 6 }}>
                              ↪ Supersede
                            </button>
                          )}
                          {mayCurate && (
                            <button className="btn btn-sm" disabled={busyId === d.id}
                                    style={{ marginLeft: 6, color: 'var(--danger)' }}
                                    title="Permanently delete this record"
                                    onClick={() => handleRetire(d, true)}>
                              {busyId === d.id ? '…' : '🗑 Delete'}
                            </button>
                          )}
                        </td>
                      </tr>
                      {supersedeFor?.id === d.id && (
                        <tr>
                          <td colSpan={7} style={{ background: 'var(--gray-50)' }}>
                            <div style={{ display: 'flex', gap: 8, alignItems: 'center', padding: '8px 0' }}>
                              <span>Superseded by:</span>
                              <select className="form-select" style={{ maxWidth: 320 }}
                                      value={supersedeTarget} onChange={e => setSupersedeTarget(e.target.value)}>
                                <option value="">Choose the replacement document…</option>
                                {documents.filter(o => o.id !== d.id).map(o => (
                                  <option key={o.id} value={o.id}>{o.title}</option>
                                ))}
                              </select>
                              <button className="btn btn-sm btn-primary" disabled={!supersedeTarget || busyId === d.id}
                                      onClick={() => handleSupersede(d, supersedeTarget)}>
                                {busyId === d.id ? '…' : 'Confirm'}
                              </button>
                              <button className="btn btn-sm" onClick={() => setSupersedeFor(null)}>Cancel</button>
                            </div>
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

      {/* View Document Modal */}
      {viewingDoc && (
        <div style={{
          position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.5)',
          display: 'flex', alignItems: 'center', justifyContent: 'center',
          zIndex: 1000, padding: 20,
        }} onClick={() => { setViewingDoc(null); setViewingContent(null); }}>
          <div style={{
            background: 'var(--surface)', color: 'var(--text)', borderRadius: 12, maxWidth: 800, width: '100%',
            maxHeight: '85vh', display: 'flex', flexDirection: 'column',
          }} onClick={e => e.stopPropagation()}>
            <div style={{
              padding: '16px 24px', borderBottom: '1px solid var(--border)',
              display: 'flex', justifyContent: 'space-between', alignItems: 'center',
            }}>
              <div>
                <h3 style={{ margin: 0 }}>{viewingDoc.title}</h3>
                <p style={{ margin: '4px 0 0', fontSize: '0.85rem', color: 'var(--gray-500)' }}>
                  {viewingDoc.source_type.replace(/_/g, ' ')} · {viewingContent?.chunk_count ?? '?'} chunks · {new Date(viewingDoc.created_at).toLocaleDateString()}
                </p>
              </div>
              <button className="btn" onClick={() => { setViewingDoc(null); setViewingContent(null); }} style={{ padding: '4px 12px' }}>✕</button>
            </div>
            <div style={{ padding: 24, overflowY: 'auto', flex: 1 }}>
              {viewingLoading ? (
                <p style={{ color: 'var(--gray-500)', textAlign: 'center' }}>Loading…</p>
              ) : viewingContent?.error ? (
                <p style={{ color: 'var(--danger)' }}>Failed to load: {viewingContent.error}</p>
              ) : (
                <pre style={{
                  whiteSpace: 'pre-wrap', wordBreak: 'break-word',
                  fontFamily: 'inherit', fontSize: '0.9rem', lineHeight: 1.7,
                  color: 'var(--text)', margin: 0,
                }}>
                  {viewingContent?.content || '(empty)'}
                </pre>
              )}
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
