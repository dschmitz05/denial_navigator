# Deployment security checklist

Use only synthetic data in non-production environments. Before a production
deployment, complete and retain evidence for each item below.

- [ ] Replace every value from `.env.example`, including the five scoped
  service/internal keys and `TOTP_FERNET_KEY`.
- [ ] Use a trusted TLS certificate and set `TLS_MODE=provided`; do not expose
  the self-signed development endpoint to staff.
- [ ] Set a unique PostgreSQL password and encrypt database, backup, and object
  storage volumes at rest.
- [ ] Restrict published ports to the reverse proxy and private administration
  network; do not publish PostgreSQL or service containers.
- [ ] Configure approved model and embedding providers and document the PHI
  disclosure level permitted for each one.
- [ ] Enable tested backups, retain them according to policy, and perform a
  restore drill before go-live.
- [ ] Provision named user accounts, require TOTP where policy requires it,
  and remove the development administrator password.
- [ ] Review audit retention, logging sink access, operating-system patching,
  and container-image update procedures.
- [ ] Validate authorization with least-privilege accounts and review the
  threat model after any new integration or data flow.
- [ ] Document BAA/data-processing approval for every provider that can
  receive PHI; confirm all model and embedding access remains in the approved boundary.
- [ ] Segment browser/proxy, gateway, database/object store, and model-service
  networks with deny-by-default firewall or security-group rules.
- [ ] Configure HIDS/EDR, security-log review, DLP/export controls, and an
  incident-response owner with tested containment and breach-notification procedures.
- [ ] Define and test RPO/RTO, immutable/offsite backups, legal-hold, and key
  rotation/recovery procedures.
- [ ] Require MFA for PHI access and enforce an idle/session-timeout policy.
