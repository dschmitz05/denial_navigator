# ADR-0005: Expose /metrics without authentication

## Status

Accepted

## Context

The gateway had no request-level telemetry: no way to see aggregate request
volume, error rate, or latency without reading application logs. Prometheus
is the standard scrape model for this, and a Prometheus scrape target is
conventionally unauthenticated on `/metrics` — the scraper is a fixed
in-cluster job, not a browser client, and most Prometheus deployments don't
carry bearer tokens or session cookies.

This repo's data-handling rule is that no PHI, patient identifiers, claim
IDs, or tenant-identifying values may leave the trust boundary without
access control. A metrics endpoint is a plausible way to violate that rule
if it labels counters by path, claim ID, organization, or user.

## Decision

Add `/metrics` to `PUBLIC_EXACT` in `crates/auth/src/rbac.rs`, so it bypasses
authentication like `/health*`. Back it with `GatewayMetrics`
(`crates/app/src/lib.rs`): three process-lifetime counters — total requests,
5xx count, and cumulative duration — with no per-path, per-tenant, per-user,
or per-claim labels. The Prometheus text output is three fixed metric names
with no label sets at all, so there is no cardinality and no identifying
data to expose regardless of who can reach the endpoint.

Given that constraint, unauthenticated access is a deliberate simplification
rather than a compliance risk: the endpoint cannot leak PHI or tenant data
because it was never given anything to hold beyond three integers.

## Consequences

Prometheus (or any scraper) can reach `/metrics` without credentials, same
network-reachability profile as `/health*`. If more granular metrics are
added later (per-route latency, per-tenant counts), this decision must be
revisited — those would need either labels scrubbed of identifying values or
the endpoint moved behind authentication, since the "no labels, so nothing
to leak" argument stops holding once labels exist. This ADR should be
updated or superseded if `GatewayMetrics` grows beyond aggregate counters.
