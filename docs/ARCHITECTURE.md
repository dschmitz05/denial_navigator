# Denial Navigator — Architecture Documentation

## System Overview

Denial Navigator is a self-hosted, decoupled healthcare denial management system that processes EDI 835 Electronic Remittance Advice (ERA) files, analyzes claim denials using a local LLM with RAG-powered policy retrieval, and provides a human-in-the-loop UI for billing teams to resolve and appeal denied claims.

## Architecture Diagram

```
┌─────────────────────────────────────────────────────────────────────┐
│                        DENIAL NAVIGATOR                             │
│                                                                     │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐             │
│  │ EDI Parser  │    │  RAG Engine │    │  LLM Service│             │
│  │  Port 8001  │    │  Port 8002  │    │  Port 8003  │             │
│  └──────┬──────┘    └──────┬──────┘    └──────┬──────┘             │
│         │                  │                  │                      │
│         ▼                  ▼                  ▼                      │
│  ┌─────────────────────────────────────────────────────┐             │
│  │              API Gateway (Port 8000)                │             │
│  │  Claims │ Denials │ Appeals │ Analyses │ Knowledge  │             │
│  └─────────────────────────┬───────────────────────────┘             │
│                            │                                         │
│                            ▼                                          │
│  ┌─────────────────────────────────────────────────────┐             │
│  │         PostgreSQL + pgvector (Port 5432)           │             │
│  │  claims │ denials │ ai_analyses │ appeals_queue     │             │
│  │  carc_codes │ rarc_codes │ knowledge_chunks         │             │
│  └─────────────────────────────────────────────────────┘             │
│                                                                     │
│  ┌──────────────┐  ┌──────────────────────────────────┐            │
│  │ llama.cpp    │  │       Frontend (Port 3080)       │            │
│  │   Port 8080  │  │   React + Vite + Nginx           │            │
│  │  qwen2.5:7b  │  │   Denial Triage │ Appeals │ KB   │            │
│  │ nomic-embed  │  └──────────────────────────────────┘            │
│  └──────────────┘                                                  │
│                                                                     │
│  ┌──────────────┐                                                  │
│  │   Dropzone   │                                                  │
│  │  (Volume)    │ ← .835 files land here → auto-parsed            │
│  └──────────────┘                                                  │
└─────────────────────────────────────────────────────────────────────┘
```

## Component Details

### 1. EDI Parser Service (`ediparser/`)

**Purpose:** Parse ANSI X12 835 ERA files into structured JSON data.

**Key Features:**
- File system watcher monitors dropzone directory for new `.835` or `.txt` files
- Parses X12 segments: ISA, GS, ST, BPR, N1, CLP, SVC, CAS, MIA, MOA, LQ, SE, GE, IEA
- Extracts payment info (BPR), claim details (CLP), service lines (SVC), and adjustments (CAS)
- Maps CARC/RARC codes to their definitions
- Derives denial records from claim adjustment data
- Stores results via API Gateway

**X12 Segment Mapping:**

| Segment | Purpose | Fields Extracted |
|---------|---------|-----------------|
| BPR | Payment summary | Method, amount, claim count, payer control number |
| CLP | Claim header | Claim ID, status, charges, paid, patient info, DOB, diagnosis codes |
| SVC | Service line | CPT/HCPCS, modifiers, charge, service date |
| CAS | Adjustments | Group code (PR/CO/OA), CARC, RARC, amount, reason |

### 2. RAG Engine (`rag-engine/`)

**Purpose:** Provide semantic search over payer policies and medical guidelines.

**Key Features:**
- Text chunking (512 tokens, 10% overlap)
- Embedding generation via llama.cpp (`nomic-embed-text` → 768-dim vectors)
- pgvector cosine similarity search
- Prompt construction for denial analysis with retrieved policy context
- Knowledge document lifecycle management

**Vector Store:**
- Uses pgvector with IVFFlat indexing (`lists = 100`)
- Cosine similarity metric: `<=>` operator
- Metadata filtering by payer, date, source type

### 3. LLM Service (`llm-service/`)

**Purpose:** Self-hosted LLM integration for denial reasoning and appeal generation.

**Key Features:**
- llama.cpp integration via OpenAI-compatible API (`qwen2.5:7b` or `qwen2.5:14b`)
- Structured JSON output for analysis results
- Automatic parsing and validation of LLM responses
- Storage of raw prompts and responses for audit
- Token counting for cost tracking

**Prompt Architecture:**

```
System: Expert RCM Denial Analyst persona
User: Structured denial context + retrieved policies
Output: JSON with explanation, category, action plan, steps, appeal draft
```

### 4. API Gateway (`api-gateway/`)

**Purpose:** Central REST API orchestrating all microservices.

**Endpoints:**

| Prefix | Route | Description |
|--------|-------|-------------|
| `/api/v1/claims` | GET, POST, PATCH | Claim CRUD + dashboard stats |
| `/api/v1/denials` | GET, PATCH | Denial list with filters + detail |
| `/api/v1/appeals` | GET, POST, PATCH | Appeals queue management |
| `/api/v1/analyses` | GET, POST, store, generate | AI analysis + generation |
| `/api/v1/knowledge` | GET, POST, embed, search | Knowledge base + vector search |
| `/api/v1/ingestion` | POST, GET | File upload + ingestion log |
| `/api/v1/feedback` | GET, POST, analytics | Human feedback loop |

### 5. Frontend (`frontend/`)

**Purpose:** React-based billing dashboard for human-in-the-loop operations.

**Pages:**
- **Dashboard:** Stats, priority denials, CARC code aggregation
- **Claims:** Claim list with status filtering
- **Denials:** Denial triage with AI analysis generation, detail modal with appeal preview
- **Appeals:** Queue management, status updates, letter preview
- **Knowledge Base:** Document management, semantic search
- **Settings:** Service status, model management, security notes

## Database Schema

### Core Tables

| Table | Purpose | Key Indexes |
|-------|---------|-------------|
| `claims` | Claim records | status, payer_id, created_at |
| `denials` | Individual denials | claim_id, status, carc_code, appeal_deadline |
| `ai_analyses` | LLM outputs | denial_id, claim_id, denial_category |
| `appeals_queue` | Operational queue | denial_id, outcome_status, assigned_user_id |
| `feedback_loop` | Success/failure logging | ai_analysis_id, accepted, was_paid |

### Reference Tables

| Table | Purpose |
|-------|---------|
| `carc_codes` | WPC Claim Adjustment Reason Codes |
| `rarc_codes` | WPC Remittance Advice Remark Codes |
| `knowledge_documents` | Payer policies, CMS LCDs, fee schedules |
| `knowledge_chunks` | Vector embeddings for policy text |

### Compliance Tables

| Table | Purpose |
|-------|---------|
| `audit_log` | HIPAA-compliant access logging |
| `users` | RBAC user management |
| `ingestion_log` | File processing audit trail |

## Security & Compliance

### PHI Protection
- All processing occurs within the Docker network
- No external API calls for PHI data
- llama.cpp models run entirely within the container

### Audit Logging
- Every access event logged to `audit_log` table
- Timestamps, user IDs, IP addresses tracked
- All data modifications recorded with before/after state

### RBAC Framework
- `billing_specialist`: Queue operations, view claims
- `billing_manager`: Policy management, bulk operations
- `rcm_director`: Full access, reporting
- `admin`: System configuration

### Production Checklist
- [ ] Change default database password in `.env`
- [ ] Configure JWT authentication
- [ ] Enable HTTPS/TLS termination
- [ ] Set up automated backups for PostgreSQL
- [ ] Configure rate limiting on API
- [ ] Review and adjust CORS origins
- [ ] Set up log aggregation (ELK, Grafana Loki)

## Deployment

### Local Development
```bash
./scripts/setup.sh
```

### Production
```bash
# Build with production settings
docker-compose -f docker-compose.yml up -d

# Verify
curl http://localhost:8000/health
curl http://localhost:3080
```

### Monitoring
- Docker Compose healthchecks for all services
- PostgreSQL connection pooling (min 2, max 20)
- llama.cpp model availability checks

## Extensibility

### Adding New CARC/RARC Codes
Edit `database/seed/carc_codes.sql` and `database/seed/rarc_codes.sql`, then:
```bash
docker-compose exec postgres psql -U denial_nav -d denial_navigator -f /docker-entrypoint-initdb.d/200-seed/carc_codes.sql
```

### Adding New Payer Policies
1. Add document via Knowledge Base UI or API
2. Upload PDF/text content
3. System auto-chunks and generates embeddings
4. Searchable via RAG engine

### Adding New LLM Models
Load the model on your llama.cpp server, then update `.env`:
```bash
# Update .env: LLM_MODEL=your-model
docker-compose restart llm-service api
```

## Troubleshooting

### Common Issues

| Issue | Solution |
|-------|----------|
| PostgreSQL won't start | Check `pgdata` volume permissions |
| llama.cpp models not loading | Check GPU memory, verify model is loaded on host |
| EDI parsing fails | Verify file is valid X12 format, check dropzone permissions |
| Frontend can't reach API | Check CORS settings, verify API is running |
| Vector search returns nothing | Ensure embeddings exist, check pgvector extension |

### Useful Commands
```bash
# Check all service status
docker-compose ps

# View service logs
docker-compose logs -f <service-name>

# Restart a specific service
docker-compose restart <service-name>

# Access PostgreSQL shell
docker-compose exec postgres psql -U denial_nav -d denial_navigator

# Check vector store size
docker-compose exec postgres psql -c "SELECT COUNT(*) FROM knowledge_chunks;"

# Check loaded models on llama.cpp
curl http://10.10.10.98:8080/v1/models
```

## File Structure

```
denial-navigator/
├── docker-compose.yml          # Service orchestration
├── .env.example                # Environment template
├── README.md                   # Getting started
├── docs/
│   └── ARCHITECTURE.md         # This file
├── database/
│   ├── init.sql                # Schema + views + triggers
│   ├── seed/
│   │   ├── carc_codes.sql      # CARC code reference
│   │   ├── rarc_codes.sql      # RARC code reference
│   │   └── sample_data.sql     # Demo data
├── ediparser/                  # EDI 835 parsing service
│   ├── main.py                 # FastAPI entry point
│   ├── parser/
│   │   ├── x12_parser.py       # Core X12 segment parser
│   │   ├── schema.py           # Pydantic data models
│   │   └── __init__.py
│   ├── watch/
│   │   └── watcher.py          # File system watcher
├── rag-engine/                 # RAG/vector search service
│   ├── main.py                 # FastAPI entry point
│   ├── embedding.py            # llama.cpp embedding client
│   ├── vector_store.py         # pgvector operations
│   └── prompts/
│       └── denial_analysis.py  # Prompt templates
├── llm-service/                # LLM reasoning service
│   └── main.py                 # FastAPI + llama.cpp integration
├── api-gateway/                # Main API orchestration
│   ├── main.py                 # FastAPI entry point
│   ├── services/
│   │   ├── db.py               # PostgreSQL connection pool
│   │   └── __init__.py         # Service HTTP clients
│   └── routes/
│       ├── claims.py           # Claim endpoints
│       ├── denials.py          # Denial endpoints
│       ├── appeals.py          # Appeals endpoints
│       ├── analyses.py         # AI analysis endpoints
│       ├── knowledge.py        # Knowledge base endpoints
│       ├── ingestion.py        # File upload endpoints
│       └── feedback.py         # Feedback loop endpoints
├── frontend/                   # React billing dashboard
│   ├── src/
│   │   ├── main.jsx
│   │   ├── App.jsx
│   │   ├── components/
│   │   │   └── Layout.jsx
│   │   ├── pages/
│   │   │   ├── Dashboard.jsx
│   │   │   ├── Claims.jsx
│   │   │   ├── Denials.jsx
│   │   │   ├── Appeals.jsx
│   │   │   ├── KnowledgeBase.jsx
│   │   │   └── Settings.jsx
│   │   └── styles/
│   │       └── main.css
│   └── Dockerfile
└── scripts/
    ├── setup.sh                # Initial setup
    ├── upload_sample.sh        # Upload test file
    └── teardown.sh             # Stop services
```
