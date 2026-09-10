# OpenClaim Navigator architecture

OpenClaim Navigator is a self-hostable claims-denial workbench. The API owns
authentication, authorization, audit, workflow changes, and canonical
PostgreSQL writes. X12 and document uploads are untrusted input and must never
be logged as raw payloads. AI is optional, advisory, and evidence-backed.

The existing React UI remains the visual baseline during the modular-monolith
migration described in [ADR-0001](adr/0001-modular-monolith-migration.md).
