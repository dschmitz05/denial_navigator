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
