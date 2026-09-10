# Initial threat model

## Protected assets

PHI, claim/remittance files, knowledge documents, credentials, audit records,
and recommendation provenance.

## Controls

- RBAC, revocable sessions, rate limits, and audited access protect API data.
- Upload limits and validation treat EDI/documents as untrusted input.
- Scoped service keys, internal credentials, and non-root containers reduce
  service-compromise impact.
- AI remains advisory; retrieved text is evidence, never executable instruction.
- Backups, retention policies, and restore verification mitigate data loss.

## Logging boundary

Logs may contain operation names, opaque IDs, status codes, durations, and
bounded sanitized transport diagnostics. They must not contain raw EDI,
uploaded document text, patient/claim fields, request/response bodies,
authorization headers, passwords, API keys, JWTs, or provider prompts and
responses. Production log access is privileged and logs follow the same
retention and incident-handling controls as other security records.
