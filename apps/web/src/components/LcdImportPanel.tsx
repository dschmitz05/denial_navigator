import { useState } from 'react'

const API_BASE = '/api/v1'

type BatchResult = { title: string; status: string; error?: string }
type Stage = 'idle' | 'staging' | 'running' | 'done' | 'error'

// CMS's bulk LCD export is one CSV with a row per policy - the single-file
// upload elsewhere on this page treats whatever it's given as ONE document
// to chunk and embed, so uploading that export as-is would mix every LCD
// nationwide into one incoherent document. This stages it server-side
// (POST /knowledge/lcd-import) and then drives the batch-processing endpoint
// (POST /knowledge/lcd-import/{job_id}/batch) in a loop, the same
// call-repeatedly-until-done shape as the existing "Re-index" action.
export default function LcdImportPanel({ onDone }: { onDone: () => void }) {
  const [file, setFile] = useState<File | null>(null)
  const [status, setStatus] = useState('A')
  const [keyword, setKeyword] = useState('')
  const [limit, setLimit] = useState('')
  const [stage, setStage] = useState<Stage>('idle')
  const [progress, setProgress] = useState({ processed: 0, total: 0 })
  const [errors, setErrors] = useState<BatchResult[]>([])
  const [message, setMessage] = useState('')

  const runBatches = async (jobId: string, total: number) => {
    let processed = 0
    while (processed < total) {
      const resp = await fetch(`${API_BASE}/knowledge/lcd-import/${jobId}/batch?limit=3`, { method: 'POST' })
      const data = await resp.json()
      if (!resp.ok) throw new Error(data.detail || `Batch failed (HTTP ${resp.status})`)
      processed = data.processed
      setProgress({ processed: data.processed, total: data.total })
      const batchErrors: BatchResult[] = (data.results || []).filter((r: BatchResult) => r.status === 'error')
      if (batchErrors.length) setErrors(prev => [...prev, ...batchErrors])
      if (data.done) break
    }
    setStage('done')
    setMessage(`Indexed ${processed} LCD${processed === 1 ? '' : 's'}.`)
    onDone()
  }

  const start = async () => {
    if (!file) return
    setStage('staging')
    setMessage('')
    setErrors([])
    try {
      const params = new URLSearchParams({ status })
      if (keyword.trim()) params.set('keyword', keyword.trim())
      if (limit.trim()) params.set('limit', limit.trim())
      const form = new FormData()
      form.append('file', file)
      const resp = await fetch(`${API_BASE}/knowledge/lcd-import?${params}`, { method: 'POST', body: form })
      const data = await resp.json()
      if (!resp.ok) throw new Error(data.detail || `Staging failed (HTTP ${resp.status})`)
      setProgress({ processed: 0, total: data.total })
      setMessage(`${data.total} LCD(s) matched (${data.skipped} skipped by filter). Indexing…`)
      setStage('running')
      await runBatches(data.job_id, data.total)
    } catch (err) {
      setMessage(err instanceof Error ? err.message : 'Import failed')
      setStage('error')
    }
  }

  const reset = () => {
    setStage('idle')
    setFile(null)
    setMessage('')
    setErrors([])
    setProgress({ processed: 0, total: 0 })
  }

  return (
    <div style={{
      padding: 20, background: 'var(--gray-50)', borderRadius: 8, marginBottom: 24, border: '1px solid var(--border)',
    }}>
      <h4 style={{ marginTop: 0, fontSize: '0.95rem', marginBottom: 4 }}>Bulk import LCDs from a CMS CSV export</h4>
      <p style={{ fontSize: '0.8rem', color: 'var(--gray-500)', marginTop: 0, marginBottom: 16 }}>
        CMS's bulk LCD export has one row per policy. This splits it into one document per LCD and indexes each
        separately, instead of treating the whole file as a single document.
      </p>

      {(stage === 'idle' || stage === 'error') && (
        <>
          <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(160px, 1fr))', gap: 12, marginBottom: 12 }}>
            <div>
              <label style={{ fontSize: '0.8rem', fontWeight: 600, display: 'block', marginBottom: 4 }}>CSV file *</label>
              <input type="file" accept=".csv,text/csv" className="form-input" style={{ width: '100%' }}
                     onChange={e => setFile(e.target.files?.[0] || null)} />
            </div>
            <div>
              <label style={{ fontSize: '0.8rem', fontWeight: 600, display: 'block', marginBottom: 4 }}>Status</label>
              <input className="form-input" value={status} onChange={e => setStatus(e.target.value)}
                     placeholder="A" style={{ width: '100%' }} />
            </div>
            <div>
              <label style={{ fontSize: '0.8rem', fontWeight: 600, display: 'block', marginBottom: 4 }}>Keyword filter</label>
              <input className="form-input" value={keyword} onChange={e => setKeyword(e.target.value)}
                     placeholder="injection, wheelchair" style={{ width: '100%' }} />
            </div>
            <div>
              <label style={{ fontSize: '0.8rem', fontWeight: 600, display: 'block', marginBottom: 4 }}>Limit (optional)</label>
              <input className="form-input" value={limit} onChange={e => setLimit(e.target.value)}
                     placeholder="e.g. 20 to test first" style={{ width: '100%' }} />
            </div>
          </div>
          {message && <p style={{ color: 'var(--danger-text)', fontSize: '0.85rem' }}>{message}</p>}
          <button className="btn btn-primary" onClick={start} disabled={!file}>Start import</button>
        </>
      )}

      {(stage === 'staging' || stage === 'running' || stage === 'done') && (
        <div>
          <p style={{ fontSize: '0.9rem' }}>
            {message || `Indexing ${progress.processed} / ${progress.total}…`}
          </p>
          {stage !== 'done' && (
            <div style={{ height: 8, background: 'var(--border)', borderRadius: 4, overflow: 'hidden', marginBottom: 12 }}>
              <div style={{
                height: '100%',
                width: `${progress.total ? Math.round((progress.processed / progress.total) * 100) : 0}%`,
                background: 'var(--primary)',
                transition: 'width 0.3s ease',
              }} />
            </div>
          )}
          {errors.length > 0 && (
            <details style={{ fontSize: '0.8rem', marginTop: 8 }}>
              <summary>{errors.length} failed to index</summary>
              <ul>{errors.map((e, i) => <li key={i}>{e.title}: {e.error}</li>)}</ul>
            </details>
          )}
          {stage === 'done' && (
            <button className="btn" onClick={reset} style={{ marginTop: 12 }}>Import another file</button>
          )}
        </div>
      )}
    </div>
  )
}
