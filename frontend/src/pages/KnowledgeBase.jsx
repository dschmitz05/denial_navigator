import React, { useState, useEffect } from 'react'

const API_BASE = '/api/v1'

export default function KnowledgeBase() {
  const [documents, setDocuments] = useState([])
  const [sourceFilter, setSourceFilter] = useState('')
  const [loading, setLoading] = useState(true)
  const [newDoc, setNewDoc] = useState({ title: '', source_type: 'payer_policy', content: '' })
  const [indexing, setIndexing] = useState(false)
  const [busyId, setBusyId] = useState(null)
  const [notice, setNotice] = useState(null)
  const [showForm, setShowForm] = useState(false)
  const [searchQuery, setSearchQuery] = useState('')
  const [searchResults, setSearchResults] = useState([])
  const [searching, setSearching] = useState(false)

  const loadDocuments = () => {
    const params = new URLSearchParams({ limit: 50 })
    if (sourceFilter) params.set('source_type', sourceFilter)

    fetch(`${API_BASE}/knowledge/documents?${params}`)
      .then(r => r.json())
      .then(data => { setDocuments(data); setLoading(false) })
      .catch(err => { console.error(err); setLoading(false) })
  }

  // Reload when the type filter changes. This page has no Filter button at
  // all, so without this the select was entirely inert.
  useEffect(() => {
    setLoading(true)
    loadDocuments()
  }, [sourceFilter])

  const handleCreateDoc = async (e) => {
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
        setNewDoc({ title: '', source_type: 'payer_policy', content: '' })
      }
      loadDocuments()
    } catch (err) {
      console.error('Create failed:', err)
      setNotice({ error: true, text: err.message })
    }
    setIndexing(false)
  }

  const handleUpload = async (e) => {
    const file = e.target.files?.[0]
    if (!file) return
    setIndexing(true)
    setNotice(null)
    const form = new FormData()
    form.append('file', file)
    try {
      const resp = await fetch(
        `${API_BASE}/knowledge/documents/upload?source_type=${encodeURIComponent(newDoc.source_type)}`,
        { method: 'POST', body: form }
      )
      const data = await resp.json()
      setNotice(resp.ok
        ? { error: false, text: `Indexed ${data.chunks_indexed ?? 0} chunks from ${file.name}`
            + (data.extracted?.pages ? ` (${data.extracted.pages} PDF pages, ${data.extracted.chars} chars extracted).` : '.') }
        : { error: true, text: data?.detail || `Upload failed (HTTP ${resp.status})` })
      loadDocuments()
    } catch (err) {
      setNotice({ error: true, text: err.message })
    }
    setIndexing(false)
    e.target.value = ''
  }

  const handleRetire = async (doc, purge) => {
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
      setNotice({ error: true, text: err.message })
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
        body: JSON.stringify({ query: searchQuery, top_k: 10 }),
      })
      const data = await resp.json()
      setSearchResults(data.results || [])
    } catch (err) {
      console.error('Search failed:', err)
    }
    setSearching(false)
  }

  return (
    <div className="page-body">
      <div className="filters-bar">
        <select className="form-select" value={sourceFilter} onChange={e => setSourceFilter(e.target.value)}>
          <option value="">All Types</option>
          <option value="cms_lcd">CMS LCD</option>
          <option value="payer_policy">Payer Policy</option>
          <option value="fee_schedule">Fee Schedule</option>
          <option value="contract">Contract</option>
          <option value="prior_auth_policy">Prior Auth Policy</option>
          <option value="medical_necessity_criteria">Medical Necessity</option>
        </select>
        <button className="btn btn-primary" onClick={() => setShowForm(!showForm)}>+ Add Document</button>
        <label className="btn" style={{ cursor: 'pointer' }}>
          {indexing ? 'Indexing…' : '⬆ Upload PDF / .txt / .md'}
          <input type="file" accept=".pdf,.txt,.md,application/pdf,text/plain,text/markdown"
                 style={{ display: 'none' }} onChange={handleUpload} disabled={indexing} />
        </label>
      </div>

      {notice && (
        <div className="card" style={{ marginBottom: 12, borderLeft: `4px solid ${notice.error ? '#dc2626' : '#16a34a'}` }}>
          <div className="card-body" style={{ color: notice.error ? '#dc2626' : '#166534' }}>{notice.text}</div>
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
                <label>Source Type</label>
                <select className="form-select" value={newDoc.source_type} onChange={e => setNewDoc({ ...newDoc, source_type: e.target.value })}>
                  <option value="cms_lcd">CMS LCD</option>
                  <option value="payer_policy">Payer Policy</option>
                  <option value="fee_schedule">Fee Schedule</option>
                  <option value="contract">Contract</option>
                  <option value="prior_auth_policy">Prior Auth Policy</option>
                  <option value="medical_necessity_criteria">Medical Necessity Criteria</option>
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
          {searchResults.length > 0 && (
            <div style={{ marginTop: 16 }}>
              <h4>Results ({searchResults.length})</h4>
              {searchResults.map((r, i) => (
                <div key={i} style={{ padding: 12, background: '#f9fafb', borderRadius: 8, marginBottom: 8 }}>
                  <p style={{ fontSize: 0.85, color: '#6b7280' }}>Chunk {r.chunk_index} · {r.token_count} tokens · Score: {Math.round(r.similarity_score * 100)}%</p>
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
                <th>Created</th>
                <th>Actions</th>
              </tr>
            </thead>
            <tbody>
              {documents.length === 0 ? (
                <tr><td colSpan="6" style={{ textAlign: 'center', padding: 20 }}>No knowledge documents</td></tr>
              ) : (
                documents.map(d => (
                  <tr key={d.id} style={d.status === 'archived' ? { opacity: 0.55 } : undefined}>
                    <td>{d.title}</td>
                    <td>{d.source_type.replace(/_/g, ' ')}</td>
                    <td><span className={`badge badge-${d.status}`}>{d.status}</span></td>
                    <td>{d.chunk_count ?? 0}</td>
                    <td>{new Date(d.created_at).toLocaleDateString()}</td>
                    <td style={{ whiteSpace: 'nowrap' }}>
                      {d.status !== 'archived' && (
                        <button className="btn btn-sm" disabled={busyId === d.id}
                                title="Remove from the index but keep the record"
                                onClick={() => handleRetire(d, false)}>
                          {busyId === d.id ? '…' : '📦 Archive'}
                        </button>
                      )}
                      <button className="btn btn-sm" disabled={busyId === d.id}
                              style={{ marginLeft: d.status !== 'archived' ? 6 : 0, color: '#dc2626' }}
                              title="Permanently delete this record"
                              onClick={() => handleRetire(d, true)}>
                        {busyId === d.id ? '…' : '🗑 Delete'}
                      </button>
                    </td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>
      </div>
    </div>
  )
}
