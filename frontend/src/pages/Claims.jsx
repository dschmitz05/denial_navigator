import React, { useState, useEffect } from 'react'

const API_BASE = '/api/v1'

function formatCurrency(value) {
  return new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD' }).format(value)
}

function formatDate(dateStr) {
  if (!dateStr) return '—'
  return new Date(dateStr).toLocaleDateString('en-US')
}

export default function Claims() {
  const [claims, setClaims] = useState([])
  const [statusFilter, setStatusFilter] = useState('')
  const [loading, setLoading] = useState(true)

  const loadClaims = () => {
    const params = new URLSearchParams({ limit: 50 })
    if (statusFilter) params.set('status', statusFilter)

    fetch(`${API_BASE}/claims?${params}`)
      .then(r => r.json())
      .then(data => { setClaims(data); setLoading(false) })
      .catch(err => { console.error(err); setLoading(false) })
  }

  useEffect(() => { loadClaims() }, [statusFilter])

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
        </select>
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
                <th>Date</th>
              </tr>
            </thead>
            <tbody>
              {claims.length === 0 ? (
                <tr><td colSpan="8" style={{ textAlign: 'center', padding: 20 }}>No claims found</td></tr>
              ) : (
                claims.map(c => (
                  <tr key={c.id}>
                    <td>{c.claim_number}</td>
                    <td>{c.patient_name || c.patient_id}</td>
                    <td>{c.payer_name}</td>
                    <td>{formatCurrency(c.total_charge)}</td>
                    <td>{formatCurrency(c.total_paid)}</td>
                    <td>{formatCurrency(c.total_adjustment)}</td>
                    <td><span className={`badge badge-${c.status.replace(/ /g, '-')}`}>{c.status}</span></td>
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
