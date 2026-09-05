# Denial Navigator

A self-hosted healthcare denial management system that processes EDI 835 Electronic Remittance Advice (ERA) files, analyzes claim denials using a local LLM with RAG-powered policy retrieval, and provides a human-in-the-loop UI for billing teams to resolve and appeal denied claims.

## Architecture

```
[ raw 835 EDI / ERA ]
         │
         ▼
 ┌──────────────┐
 │  EDI Parser  │ (X12 835 -> Canonical JSON)
 └───────┬──────┘
         │
         ▼
 ┌──────────────┐      ┌─────────────────────────┐
 │ Data Store   │ ───► │ Vector DB (Embeddings)  │
 │ (PostgreSQL) │      │ (CMS & Local Policies)  │
 └───────┬──────┘      └────────────┬────────────┘
         │                          │
         └───────────┬──────────────┘
                     │
                     ▼
        ┌─────────────────────────┐
        │  LLM Reasoning Engine   │ (Local/Hosted Model)
        └────────────┬────────────┘
                     │
                     ▼
        ┌─────────────────────────┐
        │ Human-in-the-Loop UI    │ (Appeals / Re-submits)
        └─────────────────────────┘
```

## Components

| Service | Port | Purpose |
|---------|------|---------|
| **PostgreSQL** | 5432 | Relational datastore + pgvector |
| **llama.cpp** | 8080 | Self-hosted LLM + embedding model (OpenAI-compatible API) |
| **EDI Parser** | 8001 | File watcher, X12 835 parsing |
| **RAG Engine** | 8002 | Semantic search over policy documents |
| **LLM Service** | 8003 | LLM reasoning for denial analysis |
| **API Gateway** | 8000 | Main REST API |
| **Frontend** | 3081 | React billing dashboard |

## Quick Start

### Prerequisites

- Docker & Docker Compose
- llama.cpp server running with models loaded (see Setup section)
- At least 8GB RAM, 20GB disk

### 1. Configure Environment

```bash
cp .env.example .env
# Edit .env — set LLAMA_BASE_URL to your llama.cpp server
```

### 2. Load Models on llama.cpp Server

Your llama.cpp server at `10.10.10.98:8080` should have these models loaded:

```bash
# Embedding model (768-dim vectors for pgvector)
# Load via llama.cpp's model loading mechanism

# Reasoning model (for denial analysis)
# Load via llama.cpp's model loading mechanism
```

### 3. Start Services

```bash
docker-compose up -d
```

### 4. Verify

```bash
# Check all services
docker-compose ps

# View API docs
open http://localhost:8000/docs

# View frontend
open http://localhost:3081
```

### 5. Drop an EDI 835 File

Place a `.835` or `.txt` file in the dropzone:

```bash
# Find the dropzone volume
docker volume inspect denial-navigator-dropzone --format '{{ .Mountpoint }}'

# Copy a test file there
cp sample_835.txt /var/lib/docker/volumes/denial-navigator-dropzone/_data/

# Or place directly via the API
curl -X POST http://localhost:8000/api/v1/claims/ingest \
  -F "file=@sample_835.txt"
```

## Database Schema

See `database/init.sql` for the full schema. Key tables:

- **claims** — Primary claim tracking
- **denials** — Individual denial records with CARC/RARC codes
- **ai_analyses** — LLM-generated analysis and action plans
- **appeals_queue** — Operational queue for billing teams
- **feedback_loop** — Success/failure logging for model refinement
- **carc_codes** / **rarc_codes** — WPC-maintained code lookups
- **knowledge_documents** — Payer policies and guidelines

## API Documentation

Interactive Swagger/OpenAPI docs available at:
- `http://localhost:8000/docs` (API Gateway)
- `http://localhost:8001/docs` (EDI Parser)
- `http://localhost:8002/docs` (RAG Engine)
- `http://localhost:8003/docs` (LLM Service)

## Security & Compliance

- All PHI stays within your self-hosted environment
- No data sent to public LLM endpoints
- Audit logging on all data modifications
- Role-based access control (RBAC) framework ready

## License

Proprietary — Internal Use Only
