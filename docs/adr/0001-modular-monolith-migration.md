# ADR-0001: Modular-monolith migration without replacing the current UI

## Status

Accepted

## Context

The repository has a working Rust service workspace and React UI, while the
project blueprint targets a modular monolith plus worker.

## Decision

Preserve the React UI and visual design. Consolidate domain, persistence, EDI,
storage, and jobs into Rust crates incrementally; existing services remain
compatibility boundaries until a tested migration replaces each one.

## Consequences

The transition temporarily retains extra processes. New core behavior stays
cloud-neutral and requires tests and a safe migration path.
