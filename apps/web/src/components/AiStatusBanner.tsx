import { useEffect, useState } from 'react'

type AiStatus = {
  degraded: boolean
  recent_all_degraded: boolean
  analyses: number
  fallback_share: number
  reasons: { llm_error: number; retrieval_error: number; no_evidence: number }
}

/** What each `fallback_reason` means for the person reading the analysis. */
export const FALLBACK_LABEL: Record<string, string> = {
  llm_error: 'Rules-based: AI unavailable',
  retrieval_error: 'AI without policy evidence: search failed',
  no_evidence: 'AI without policy evidence: nothing relevant found',
}

/** Shown only while a large share of recent analyses fell back, so staff know
 *  recommendations are rules-based or unsupported before relying on them. */
export default function AiStatusBanner() {
  const [status, setStatus] = useState<AiStatus | null>(null)

  useEffect(() => {
    fetch('/api/v1/analyses/status')
      .then(r => (r.ok ? r.json() : null))
      .then(setStatus)
      .catch(() => setStatus(null))
  }, [])

  if (!status?.degraded) return null
  const { llm_error, retrieval_error, no_evidence } = status.reasons
  return (
    <div className="card" style={{ marginBottom: 12, borderLeft: '4px solid var(--warning)' }} role="status">
      <div className="card-body">
        <strong>AI analysis is degraded.</strong>{' '}
        {status.recent_all_degraded && 'The most recent analyses all fell back. '}
        Over the last 24 hours, {Math.round(status.fallback_share * 100)}% of {status.analyses} analyses fell back:
        {' '}{llm_error} used deterministic rules because the AI was unavailable,
        {' '}{retrieval_error + no_evidence} ran without policy evidence.
        Treat recommendations with extra care; an administrator can check Settings → Service Status.
      </div>
    </div>
  )
}
