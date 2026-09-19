# ADR-0003: Align RBAC roles with plan §2.2

## Status

Accepted

## Context

The application used five legacy role names that did not match the seven-role
plan model. This made role provisioning and audit evidence ambiguous, and the
organization membership role had no database constraint.

## Decision

Use the plan roles: `system_admin`, `security_admin`,
`revenue_cycle_manager`, `billing_specialist`, `coding_specialist`, `auditor`,
and `read_only`. Migration 032 normalizes existing `admin` users to
`system_admin`, and both `billing_manager` and `rcm_director` to
`revenue_cycle_manager`. New users and memberships accept only the canonical
set. System/security administrators manage configuration; revenue-cycle
managers manage operational policy; specialists write operational work;
auditors and read-only users are read-only.

## Consequences

Existing JWTs with legacy role claims must be reissued after migration. The
role names are now consistent across the database, API, RBAC middleware, and
UI. Fine-grained organization-defined roles remain future work.
