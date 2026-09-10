# v1.0 release readiness

This project validates releases only with synthetic data. Required release
evidence: `cargo test --workspace --locked`, frontend smoke/build, the X12
fuzzer target, a backup restore drill, container/SBOM scan results, and a
synthetic 835/837 end-to-end import.

## Final release artifacts

Create a release commit only after `git status --short` is empty, then create
and sign the annotated `v1.0.0` tag with the release maintainer's GPG key.
Build the immutable distribution artifacts into one directory and sign their
checksum manifest; the manifest is intentionally generated after the artifacts
exist so it can never claim to cover a mutable image tag or working tree.

```bash
./scripts/sign_release_checksums.sh dist/v1.0.0
gpg --verify dist/v1.0.0/SHA256SUMS.asc dist/v1.0.0/SHA256SUMS
sha256sum --check dist/v1.0.0/SHA256SUMS
```

Publish every artifact in that directory alongside `SHA256SUMS` and
`SHA256SUMS.asc`. Do not publish a release or create the final tag from a
dirty worktree.

## Supported X12 scope

835 remittance parsing supports ISA/GS/ST envelopes, BPR, TRN, CLP, CAS,
SVC, LQ, PLB, SE/GE/IEA and normalizes claim/service adjustments, CARC/RARC,
and patient-responsibility amounts. 837 parsing identifies institutional and
professional transactions and correlates only exact claim numbers.

## Known limitations

- Scanned PDFs require OCR before knowledge ingestion.
- S3 event notifications are not included yet; the SFTP and S3-compatible
  polling importers and a Helm chart are available.
- OIDC/Keycloak and multi-organization tenant isolation remain deployment
  roadmap work; local JWT/TOTP roles protect the current single deployment.
- AI recommendations are advisory and may fall back to deterministic rules.
