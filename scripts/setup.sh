#!/bin/bash
# ============================================================
# Denial Navigator — Setup Script
# ============================================================

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

echo "========================================"
echo "  Denial Navigator — Setup"
echo "========================================"

cd "$PROJECT_DIR"

# ── 1. Check prerequisites ──
echo ""
echo "[1/6] Checking prerequisites..."

if ! command -v docker &> /dev/null; then
    echo "ERROR: Docker is not installed. Please install Docker first."
    exit 1
fi

if ! command -v docker-compose &> /dev/null; then
    echo "ERROR: Docker Compose is not installed. Please install docker-compose first."
    exit 1
fi

echo "  ✓ Docker: $(docker --version)"
echo "  ✓ Docker Compose: $(docker-compose --version)"

# ── 2. Create environment file ──
echo ""
echo "[2/6] Setting up environment..."

if [ ! -f .env ]; then
    cp .env.example .env
    echo "  ✓ Created .env from template"
    echo "  ⚠ Please edit .env with your configuration"
else
    echo "  ✓ .env already exists"
fi

# ── 3. Start services ──
echo ""
echo "[3/6] Starting services..."

docker-compose up -d postgres

# Wait for PostgreSQL
echo "  Waiting for PostgreSQL..."
for i in $(seq 1 30); do
    if docker exec denial-navigator-postgres pg_isready -U denial_nav -d denial_navigator &>/dev/null; then
        echo "  ✓ PostgreSQL is ready"
        break
    fi
    sleep 1
done

# Wait for external llama.cpp server
LLAMA_HOST=${LLAMA_BASE_URL:-http://10.10.10.98:8080}
echo "  Waiting for llama.cpp server at $LLAMA_HOST..."
for i in $(seq 1 30); do
    if curl -sf "${LLAMA_HOST}/health" &>/dev/null; then
        echo "  ✓ llama.cpp server is ready"
        break
    fi
    sleep 1
done

# ── 4. Verify models ──
echo ""
echo "[4/6] Verifying models..."

EMBEDDING_MODEL=${EMBEDDING_MODEL:-nomic-embed-text}
LLM_MODEL=${LLM_MODEL:-qwen2.5:7b}

echo "  Checking embedding model: $EMBEDDING_MODEL"
curl -sf "${LLAMA_HOST}/v1/models" | python3 -m json.tool 2>/dev/null | grep -q "$EMBEDDING_MODEL" && echo "  ✓ Embedding model available" || echo "  ⚠ Embedding model not found — load it on the host"

echo "  Checking reasoning model: $LLM_MODEL"
curl -sf "${LLAMA_HOST}/v1/models" | python3 -m json.tool 2>/dev/null | grep -q "$LLM_MODEL" && echo "  ✓ Reasoning model available" || echo "  ⚠ Reasoning model not found — load it on the host"

# ── 5. Start remaining services ──
echo ""
echo "[5/6] Starting remaining services..."

docker-compose up -d ediparser rag-engine llm-service api frontend

# ── 6. Verify ──
echo ""
echo "[6/6] Verifying services..."

sleep 5
docker-compose ps

echo ""
echo "========================================"
echo "  Setup Complete!"
echo "========================================"
echo ""
echo "  Frontend:    https://localhost:3443   (http://localhost:3081 redirects here)"
echo "  API Docs:    https://localhost:3443/docs"
echo ""
echo "  The certificate is self-signed on a first run, so your browser will"
echo "  warn. Replace it with a trusted one before go-live - see certs/README.md."
echo "  llama.cpp:   ${LLAMA_HOST}"
echo ""
echo "  To view logs:     docker-compose logs -f"
echo "  To stop:          docker-compose down"
echo "  To restart:       docker-compose restart"
echo ""
