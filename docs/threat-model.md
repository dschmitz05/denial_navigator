# Threat model

## Protected assets

PHI, claim/remittance files, knowledge documents, credentials, audit records,
and recommendation provenance.

## Trust boundaries

The browser, reverse proxy, gateway, database, object store, EDI sources, and
model/embedding services are separate trust boundaries. Only the reverse proxy
may be internet facing. Claim/remittance content, patient identifiers,
attachments, and knowledge documents are PHI; credentials, audit records,
recommendation provenance, backups, and encryption keys are also protected.

## Threats and controls

- RBAC, revocable sessions, rate limits, and audited access protect API data.
- Upload limits and validation treat EDI/documents as untrusted input.
- Scoped service keys, internal credentials, and non-root containers reduce
  service-compromise impact.
- AI remains advisory; retrieved text is evidence, never executable instruction.
- Backups, retention policies, and restore verification mitigate data loss.

| Threat | Required control |
|---|---|
| IDOR/cross-tenant access | Resolve active organization membership for every request; scope every query and object key by organization; test cross-org negative cases. |
| SQL injection, XSS, CSRF | Bound parameters, input validation, output escaping, CSP, and same-origin authenticated API. |
| SSRF/malicious uploads | Allowlisted service endpoints, no caller-supplied fetch URLs, byte/segment limits, and untrusted-input parsing. |
| Prompt injection/fabricated evidence | Retrieved text is evidence, never instructions; evidence IDs are allowlisted; AI remains advisory and cannot mutate claims. |
| Malicious administrator | Least-privilege roles, MFA, audited configuration, financial dual control, and periodic access review. |
| Supply chain | Locked dependencies, SBOM, vulnerability/secret scans, reviewed image provenance, and critical-patch process. |
| Object store/export/backup exposure | Private encrypted storage, scoped keys, audited downloads, encrypted tested backups, retention and legal-hold controls. |
| Excessive retention/log disclosure | Minimize PHI, redact logs, avoid raw prompts by default, and never place PHI in audit metadata. |
| Ransomware/availability | Segmentation, immutable/offline backups, restore drills, rate limits, alerting, RPO/RTO, and incident response. |

## Logging boundary

Logs may contain operation names, opaque IDs, status codes, durations, and
bounded sanitized transport diagnostics. They must not contain raw EDI,
uploaded document text, patient/claim fields, request/response bodies,
authorization headers, passwords, API keys, JWTs, or provider prompts and
responses. Production log access is privileged and logs follow the same
retention and incident-handling controls as other security records.
