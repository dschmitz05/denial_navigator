import React, { useState, useEffect } from 'react'

const API_BASE = '/api/v1'

function formatCurrency(value) {
  return new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD' }).format(value)
}

function formatDate(dateStr) {
  if (!dateStr) return '—'
  return new Date(dateStr).toLocaleDateString('en-US')
}

function denialProgress(claim) {
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

export default function Claims() {
  const [claims, setClaims] = useState([])
  const [statusFilter, setStatusFilter] = useState('')
  const [search, setSearch] = useState('')
  const [loading, setLoading] = useState(true)

  const loadClaims = () => {
    const params = new URLSearchParams({ limit: 50 })
    if (statusFilter) params.set('status', statusFilter)
    if (search.trim()) params.set('q', search.trim())

    fetch(`${API_BASE}/claims?${params}`)
      .then(r => r.json())
      .then(data => { setClaims(data); setLoading(false) })
      .catch(err => { console.error(err); setLoading(false) })
  }

  // Debounced so a search is one request, not one per keystroke.
  useEffect(() => {
    const t = setTimeout(loadClaims, search ? 300 : 0)
    return () => clearTimeout(t)
  }, [statusFilter, search])

  return (
    <div className="page-body">
      <div className="filters-bar">
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
                <tr><td colSpan="9" style={{ textAlign: 'center', padding: 20 }}>No claims found</td></tr>
              ) : (
                claims.map(c => (
                  <tr key={c.id}>
                    <td>{c.claim_number}</td>
                    <td>{c.patient_name || c.patient_id}</td>
                    <td>{c.payer_name}</td>
                    <td>{formatCurrency(c.total_charge)}</td>
                    <td>{formatCurrency(c.total_paid)}</td>
                    <td>{formatCurrency(c.total_adjustment)}</td>
                    <td><span className={`badge badge-${c.status.replace(/_/g, '-')}`}>{c.status.replace(/_/g, ' ')}</span></td>
                    <td>{denialProgress(c)}</td>
                    <td>{formatDate(c.created_at)}</td>
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
