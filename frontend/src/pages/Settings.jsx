import React from 'react'

export default function Settings() {
  return (
    <div className="page-body">
      <div className="card">
        <div className="card-header"><h3>⚙️ Settings</h3></div>
        <div className="card-body">
          <h4 style={{ marginBottom: 16 }}>Service Status</h4>
          <div className="stats-grid">
            <div className="stat-card success">
              <div className="stat-label">API Gateway</div>
              <div className="stat-value" style={{ fontSize: '1rem' }}>✅ Running</div>
            </div>
            <div className="stat-card success">
              <div className="stat-label">PostgreSQL + pgvector</div>
              <div className="stat-value" style={{ fontSize: '1rem' }}>✅ Running</div>
            </div>
            <div className="stat-card warning">
              <div className="stat-label">llama.cpp Server</div>
              <div className="stat-value" style={{ fontSize: '1rem' }}>⚠️ Check Status</div>
            </div>
            <div className="stat-card success">
              <div className="stat-label">EDI Parser</div>
              <div className="stat-value" style={{ fontSize: '1rem' }}>✅ Running</div>
            </div>
            <div className="stat-card success">
              <div className="stat-label">RAG Engine</div>
              <div className="stat-value" style={{ fontSize: '1rem' }}>✅ Running</div>
            </div>
            <div className="stat-card success">
              <div className="stat-label">LLM Service</div>
              <div className="stat-value" style={{ fontSize: '1rem' }}>✅ Running</div>
            </div>
          </div>

          <h4 style={{ marginTop: 24, marginBottom: 16 }}>Quick Links</h4>
          <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
            <a href="/docs" className="btn" target="_blank">📖 API Documentation (Swagger)</a>
            <a href="http://10.10.10.98:8080" className="btn" target="_blank">🤖 llama.cpp Server (10.10.10.98:8080)</a>
          </div>

          <h4 style={{ marginTop: 24, marginBottom: 16 }}>llama.cpp Model Management</h4>
          <p style={{ color: '#6b7280', marginBottom: 16 }}>
            Models are loaded on the remote llama.cpp server at `10.10.10.98:8080`.
            Check loaded models via the OpenAI-compatible API:
          </p>
          <div style={{ background: '#1f2937', color: '#e5e7eb', padding: 16, borderRadius: 8, fontFamily: 'monospace', fontSize: '0.85rem' }}>
            <p># List loaded models</p>
            <p>curl http://10.10.10.98:8080/v1/models</p>
            <br/>
            <p># Check health</p>
            <p>curl http://10.10.10.98:8080/health</p>
          </div>

          <h4 style={{ marginTop: 24, marginBottom: 16 }}>Security Notes</h4>
          <div className="card" style={{ background: '#fffbeb', border: '1px solid #fcd34d' }}>
            <div className="card-body" style={{ fontSize: '0.85rem' }}>
              <p>• All PHI/PII data stays within your self-hosted environment</p>
              <p>• No patient data is sent to public LLM endpoints</p>
              <p>• Audit logging captures all data access and modifications</p>
              <p>• Change default passwords before production deployment</p>
              <p>• Enable JWT authentication for production use</p>
            </div>
          </div>
        </div>
      </div>
    </div>
  )
}
