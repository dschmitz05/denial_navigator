# Denial Navigator — API Reference

## Base URL
```
http://localhost:8000/api/v1
```

Interactive Swagger docs: `http://localhost:8000/docs`

---

## Health

### GET /health
```json
{
  "status": "healthy",
  "version": "1.0.0",
  "services": {
    "ediparser": "http://localhost:8001",
    "rag_engine": "http://localhost:8002",
    "llm_service": "http://localhost:8003",
    "database": "postgresql://denial_nav@postgres:5432/denial_navigator"
  }
}
```

---

## Claims

### GET /claims
List claims with optional filtering.

**Query Parameters:**
- `status` — Filter by claim status (ingested, parsed, denied, partially_paid, resolved)
- `limit` — Max results (default: 50, max: 500)
- `offset` — Pagination offset

**Response:** `Claim[]`

### GET /claims/:claim_id
Get a single claim with denials and analyses.

**Response:**
```json
{
  "id": "uuid",
  "claim_number": "CLM-2024-001",
  "patient_name": "John Doe",
  "payer_name": "BlueCross BlueShield",
  "total_charge": 2500.00,
  "denials": [...],
  "analyses": [...]
}
```

### POST /claims
Create a new claim.

**Body:**
```json
{
  "claim_number": "CLM-2024-999",
  "patient_id": "PAT-999",
  "payer_name": "UnitedHealthcare",
  "total_charge": 1500.00,
  "icd_10_codes": ["J18.9", "R05.2"]
}
```

### PATCH /claims/:claim_id
Update a claim.

**Body:**
```json
{
  "status": "resolved",
  "total_paid": 2000.00,
  "total_adjustment": 500.00
}
```

### GET /claims/dashboard/stats
Get dashboard statistics.

**Response:**
```json
{
  "total_claims": 5,
  "denied_claims": 3,
  "pending_denials": 8,
  "pending_appeals": 6,
  "total_charges": 13050.00,
  "total_paid": 2150.00,
  "total_adjustments": 10900.00
}
```

---

## Denials

### GET /denials
List denials with filters.

**Query Parameters:**
- `status` — open, analyzed, in_appeal, appealed, resolved, written_off
- `carc_code` — Filter by CARC code
- `cagc` — Filter by adjustment group code (CO, PR, OA)
- `claim_id` — Filter by claim
- `priority` — Only show denials with appeal deadline ≤ 14 days
- `limit`, `offset` — Pagination

### GET /denials/:denial_id
Get denial with full context (codes, patient, payer, analyses).

### PATCH /denials/:denial_id
Update denial status.

**Body:**
```json
{
  "status": "analyzed",
  "appeal_deadline": "2024-02-15"
}
```

### GET /denials/bulk-carc
Get denial counts aggregated by CARC code for batch processing.

**Response:**
```json
[
  {
    "carc_code": "150",
    "carc_description": "Not covered because prior authorization required and not obtained",
    "cagc": "CO",
    "denial_count": 5,
    "total_denied_amount": 2500.00,
    "avg_denial_amount": 500.00
  }
]
```

---

## Appeals

### GET /appeals
List appeals queue items.

**Query Parameters:**
- `outcome_status` — queued, in_progress, submitted, approved, denied_again, overruled, resolved, cancelled
- `resolution_type` — appeal_letter, corrected_claim, clinical_docs, payer_contact, write_off
- `assigned_user_id` — Filter by user

### POST /appeals
Create appeal queue item.

**Body:**
```json
{
  "denial_id": "uuid",
  "resolution_type": "appeal_letter",
  "assigned_user_id": "uuid",
  "notes": "High priority — appeal deadline in 5 days"
}
```

### PATCH /appeals/:appeal_id
Update appeal status.

**Body:**
```json
{
  "outcome_status": "submitted",
  "submitted_at": "2024-01-15T10:00:00Z",
  "notes": "Appeal submitted to payer"
}
```

### GET /appeals/:appeal_id/letter
Get the appeal letter preview for an appeal.

**Response:**
```json
{
  "claim_number": "CLM-2024-001",
  "patient_name": "John Doe",
  "payer_name": "BlueCross BlueShield",
  "draft_appeal_letter": "...",
  "explanation": "..."
}
```

---

## Analyses

### GET /analyses
List AI analyses.

**Query Parameters:**
- `denial_id` — Filter by denial
- `claim_id` — Filter by claim
- `limit` — Max results

### POST /analyses/store
Store an AI analysis result (called by LLM service).

**Body:**
```json
{
  "denial_id": "uuid",
  "claim_id": "uuid",
  "model_name": "qwen2.5:7b",
  "raw_prompt": "...",
  "raw_response": "...",
  "parsed_result": {
    "explanation": "...",
    "denial_category": "administrative",
    "root_cause_summary": "...",
    "action_plan": {"type": "corrected_claim", "requires": ["prior_auth"]},
    "steps": [...],
    "needs_appeal": false,
    "confidence_score": 0.85
  },
  "prompt_tokens": 450,
  "completion_tokens": 320,
  "total_tokens": 770
}
```

### POST /analyses/generate
Generate a complete AI analysis (RAG retrieval + LLM call).

**Body:**
```json
{
  "denial_id": "uuid",
  "temperature": 0.3
}
```

---

## Knowledge Base

### GET /knowledge/documents
List knowledge documents.

**Query Parameters:**
- `source_type` — cms_lcd, payer_policy, fee_schedule, contract, prior_auth_policy, medical_necessity_criteria
- `status` — pending, processing, indexed, error, archived

### POST /knowledge/documents
Create a knowledge document.

**Body:**
```json
{
  "title": "BlueCross Prior Authorization Requirements 2024",
  "source_type": "payer_policy",
  "effective_date": "2024-01-01"
}
```

### POST /knowledge/embed
Store document chunks with embeddings.

**Body:**
```json
{
  "document_id": "uuid",
  "chunks": [
    {
      "chunk_index": 0,
      "content": "Prior authorization is required for...",
      "embedding": [0.1, 0.2, ...],
      "token_count": 45
    }
  ],
  "embeddings": [[0.1, 0.2, ...]]
}
```

### POST /knowledge/search
Search the knowledge base.

**Body:**
```json
{
  "query": "BlueCross prior authorization requirements 2024",
  "top_k": 5,
  "filters": {"payer": "BlueCross BlueShield"}
}
```

---

## Ingestion

### POST /ingestion/upload
Upload an EDI 835 file for parsing.

**Content-Type:** `multipart/form-data`

**Body:** `file` (UploadFile)

**Response:**
```json
{
  "file_name": "remittance_20240101.835",
  "file_hash": "sha256...",
  "claims_parsed": 3,
  "denials_parsed": 8,
  "status": "completed"
}
```

### POST /ingestion/store
Store parsed ingestion results.

**Body:**
```json
{
  "file_name": "remittance_20240101.835",
  "file_hash": "sha256...",
  "file_size": 4096,
  "claims": [...],
  "denials": [...]
}
```

### GET /ingestion/log
List ingestion history.

---

## Feedback Loop

### GET /feedback
List feedback records.

**Query Parameters:**
- `ai_analysis_id` — Filter by analysis
- `accepted` — Filter by acceptance (true/false)

### POST /feedback
Submit feedback on an AI analysis.

**Body:**
```json
{
  "ai_analysis_id": "uuid",
  "user_id": "uuid",
  "rating": 4,
  "accepted": true,
  "user_edits": {"steps": ["Modified step 3"]},
  "action_taken": "corrected_and_resubmitted",
  "was_paid_on_resubmit": true,
  "feedback_text": "Analysis was mostly correct, needed adjustment on step 2"
}
```

### GET /feedback/analytics
Get feedback analytics.

**Response:**
```json
{
  "total_feedback": 15,
  "accepted_count": 12,
  "success_count": 10,
  "avg_rating": 4.2,
  "corrected_count": 3
}
```
