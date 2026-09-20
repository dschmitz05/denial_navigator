set dotenv-load := true

default:
    @just --list
dev:
    docker compose -f docker-compose.rust.yml up --build
api:
    cargo run -p api-gateway
web:
    npm --prefix apps/web run dev
test-rust:
    cargo test --workspace --locked
test-web:
    npm --prefix apps/web run smoke
test: test-rust test-web
generate-api:
    python3 crates/api-gateway/openapi/generate.py
fmt:
    cargo fmt --all
fmt-check:
    cargo fmt --all -- --check
lint-rust:
    cargo clippy --workspace --all-targets --locked
object-storage:
    docker compose -f docker-compose.rust.yml --profile object-storage up --build

# Run the watched-import worker with its required dependencies.
worker:
    docker compose -f docker-compose.rust.yml up --build ediparser

# Migrations run automatically when the gateway starts; this command performs
# that startup path without starting the browser.
migrate:
    docker compose -f docker-compose.rust.yml up --build api

seed-test-knowledge:
    ./scripts/seed_test_knowledge.sh

synthetic-data:
    python3 scripts/generate_synthetic_835.py

synthetic-e2e:
    ./scripts/test_synthetic_e2e.sh

security-check:
    cargo audit --ignore RUSTSEC-2023-0071
    npm --prefix apps/web audit --audit-level=high

# Deliberately requires an exact confirmation before deleting local Compose
# volumes. Never run this against a production project or a PHI-containing DB.
db-reset confirm:
    test "{{confirm}}" = "RESET-LOCAL-DB"
    docker compose -f docker-compose.rust.yml down -v
