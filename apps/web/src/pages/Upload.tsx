import React, { useState } from 'react'
import ProviderAdjustmentsSummary from '../components/ProviderAdjustments'

const API_BASE = '/api/v1'

type UploadResult = {
  file: string
  status: 'success' | 'error'
  error?: string
  claims_stored?: number
  denials_stored?: number
}

type IngestionHistoryRow = {
  id: string
  file_name: string
  file_size_bytes: number
  claims_count?: number
  denials_count?: number
  status?: string
  created_at?: string
}

export default function Upload() {
  const [files, setFiles] = useState<File[]>([])
  const [uploading, setUploading] = useState(false)
  const [results, setResults] = useState<UploadResult[]>([])
  const [error, setError] = useState('')
  const [dragActive, setDragActive] = useState(false)

  const handleFiles = (newFiles: FileList | File[]) => {
    setFiles(prev => [...prev, ...Array.from(newFiles)].slice(0, 10)) // max 10 files
  }

  const handleDrag = (e: React.DragEvent<HTMLDivElement>) => {
    e.preventDefault()
    e.stopPropagation()
    if (e.type === 'dragenter' || e.type === 'dragover') {
      setDragActive(true)
    } else if (e.type === 'dragleave') {
      setDragActive(false)
    }
  }

  const handleDrop = (e: React.DragEvent<HTMLDivElement>) => {
    e.preventDefault()
    e.stopPropagation()
    setDragActive(false)
    if (e.dataTransfer.files && e.dataTransfer.files.length > 0) {
      handleFiles(e.dataTransfer.files)
    }
  }

  const handleFileInput = (e: React.ChangeEvent<HTMLInputElement>) => {
    if (e.target.files && e.target.files.length > 0) {
      handleFiles(e.target.files)
    }
  }

  const removeFile = (index: number) => {
    setFiles(prev => prev.filter((_, i) => i !== index))
  }

  const uploadFiles = async () => {
    if (files.length === 0) return
    setUploading(true)
    setError('')
    setResults([])

    for (const file of files) {
      const formData = new FormData()
      formData.append('file', file)

      try {
        const resp = await fetch(`${API_BASE}/ingestion/ingest`, {
          method: 'POST',
          headers: {
            Authorization: `Bearer ${localStorage.getItem('auth_token')}`,
          },
          body: formData,
        })
        const data = await resp.json() as Record<string, unknown>
        if (!resp.ok) {
          setResults(prev => [...prev, { file: file.name, status: 'error', error: typeof data.detail === 'string' ? data.detail : 'Upload failed' }])
        } else {
          setResults(prev => [...prev, { file: file.name, status: 'success', claims_stored: typeof data.claims_stored === 'number' ? data.claims_stored : undefined, denials_stored: typeof data.denials_stored === 'number' ? data.denials_stored : undefined }])
        }
      } catch (err) {
        setResults(prev => [...prev, { file: file.name, status: 'error', error: err instanceof Error ? err.message : 'Upload failed' }])
      }
    }

    setUploading(false)
    setFiles([])
  }

  const canUpload = files.length > 0 && !uploading

  return (
    <div className="page-body">
      <h1 className="page-title">Upload</h1>
      <div className="card">
        <div className="card-header">
          <h3>📁 Upload EDI 835 / 837 Files</h3>
          <p style={{ fontSize: '0.85rem', color: 'var(--gray-500)', marginTop: 4 }}>
            Upload EDI 835 ERA or EDI 837 claim files for parsing and analysis. Supports .txt, .835, .837 formats.
          </p>
        </div>
        <div className="card-body">
          {/* Upload Zone */}
          <div
            onDragEnter={handleDrag}
            onDragOver={handleDrag}
            onDragLeave={handleDrag}
            onDrop={handleDrop}
            style={{
              border: `2px dashed ${dragActive ? 'var(--primary)' : 'var(--gray-300)'}`,
              borderRadius: 12,
              padding: '40px 20px',
              textAlign: 'center',
              cursor: 'pointer',
              background: dragActive ? 'var(--info-light)' : 'var(--surface-alt)',
              marginBottom: 24,
              transition: 'all 0.2s',
            }}
            onClick={() => document.getElementById('file-input')?.click()}
          >
            <div style={{ fontSize: '2rem', marginBottom: 12 }}>📤</div>
            <p style={{ fontWeight: 600, marginBottom: 4 }}>
              Drag & drop files here, or click to browse
            </p>
            <p style={{ fontSize: '0.85rem', color: 'var(--gray-500)' }}>
              Supports .txt, .835, .837 files (max 10 at once)
            </p>
            <input
              id="file-input"
              type="file"
              multiple
              accept=".txt,.835,.837"
              style={{ display: 'none' }}
              onChange={handleFileInput}
            />
          </div>

          {/* File List */}
          {files.length > 0 && (
            <div style={{ marginBottom: 20 }}>
              <h4 style={{ fontSize: '0.9rem', marginBottom: 8 }}>
                Selected Files ({files.length})
              </h4>
              {files.map((file, i) => (
                <div key={i} style={{
                  display: 'flex',
                  alignItems: 'center',
                  gap: 12,
                  padding: '8px 12px',
                  background: 'var(--gray-100)',
                  borderRadius: 6,
                  marginBottom: 6,
                  fontSize: '0.85rem',
                }}>
                  <span>📄</span>
                  <span style={{ flex: 1 }}>{file.name}</span>
                  <span style={{ color: 'var(--gray-500)', fontSize: '0.8rem' }}>
                    {(file.size / 1024).toFixed(1)} KB
                  </span>
                  <button
                    className="btn btn-sm"
                    onClick={() => removeFile(i)}
                    style={{ padding: '2px 8px' }}
                  >
                    ✕
                  </button>
                </div>
              ))}
              <button
                className="btn"
                onClick={() => setFiles([])}
                style={{ fontSize: '0.8rem', marginTop: 8 }}
              >
                Clear all
              </button>
            </div>
          )}

          {/* Upload Button */}
          {canUpload && (
            <button
              className="btn btn-primary"
              onClick={uploadFiles}
              style={{ width: '100%', padding: '12px', fontSize: '1rem' }}
              disabled={uploading}
            >
              {uploading ? '⏳ Uploading...' : `📤 Upload ${files.length} File${files.length > 1 ? 's' : ''}`}
            </button>
          )}

          {/* Upload Results */}
          {results.length > 0 && (
            <div style={{ marginTop: 32 }}>
              <h4 style={{ fontSize: '0.9rem', marginBottom: 12 }}>Upload Results</h4>
              {results.map((r, i) => (
                // The pale green/red backgrounds were fixed values while the
                // filename inherited --text, which is near-white in dark mode.
                // The callout classes state both halves, so they follow the theme.
                <div key={i}
                     className={`callout ${r.status === 'success' ? 'callout-success' : 'callout-danger'}`}
                     style={{ display: 'flex', alignItems: 'center', gap: 12, marginBottom: 8 }}>
                  <span style={{ fontSize: '1.2rem' }}>
                    {r.status === 'success' ? '✅' : '❌'}
                  </span>
                  <div style={{ flex: 1 }}>
                    <div style={{ fontWeight: 600, fontSize: '0.9rem' }}>{r.file}</div>
                    {r.status === 'success' && (
                      <div style={{ fontSize: '0.8rem', opacity: 0.85 }}>
                        {r.claims_stored} claims, {r.denials_stored} denials stored
                      </div>
                    )}
                    {r.status === 'error' && (
                      <div style={{ fontSize: '0.8rem', opacity: 0.85 }}>{r.error}</div>
                    )}
                  </div>
                </div>
              ))}
            </div>
          )}

          {/* Error */}
          {error && (
            <div className="card" style={{
              background: 'var(--danger-light)', color: 'var(--danger-text)',
              border: '1px solid var(--danger)',
              marginTop: 20,
              fontSize: '0.85rem',
            }}>
              {error}
            </div>
          )}
        </div>
      </div>

      {/* Ingestion History */}
      <IngestionHistory />
      <ProviderAdjustmentsSummary />
    </div>
  )
}

function IngestionHistory() {
  const [history, setHistory] = useState<IngestionHistoryRow[]>([])
  const [loading, setLoading] = useState(true)

  React.useEffect(() => {
    fetch('/api/v1/ingestion/history?limit=50')
      .then(r => r.json())
      .then((data: unknown) => { setHistory(Array.isArray(data) ? data as IngestionHistoryRow[] : []); setLoading(false) })
      .catch(() => setLoading(false))
  }, [])

  if (loading) return null

  return (
    <div className="card" style={{ marginTop: 24 }}>
      <div className="card-header">
        <h3>📋 Upload History</h3>
      </div>
      <div className="card-body" style={{ padding: 0 }}>
        {history.length === 0 ? (
          <p style={{ textAlign: 'center', padding: 20, color: 'var(--gray-500)' }}>No uploads yet</p>
        ) : (
          <table>
            <thead>
              <tr>
                <th>File Name</th>
                <th>Size</th>
                <th>Claims</th>
                <th>Denials</th>
                <th>Status</th>
                <th>Date</th>
              </tr>
            </thead>
            <tbody>
              {history.map(h => (
                <tr key={h.id}>
                  <td>{h.file_name}</td>
                  <td>{(h.file_size_bytes / 1024).toFixed(1)} KB</td>
                  <td>{h.claims_count || 0}</td>
                  <td>{h.denials_count || 0}</td>
                  <td>
                    <span className={`badge badge-${(h.status || 'pending').replace(/ /g, '-')}`}>
                      {h.status}
                    </span>
                  </td>
                  <td>{h.created_at ? new Date(h.created_at).toLocaleString() : '—'}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  )
}
