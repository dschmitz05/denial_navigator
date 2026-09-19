# Dependency and license policy

OpenClaim Navigator is MIT licensed. Dependencies and bundled reference data
must be compatible with that license and their source/license metadata must be
retained. Do not add copyleft or source-available dependencies to the core
distribution without an ADR and maintainer approval.

Before merging a dependency change:

1. Prefer a maintained, actively released package with a documented license.
2. Record any non-obvious runtime, native-library, model, or reference-data
   license obligation in the pull request.
3. Do not vendor payer manuals, production EDI, PHI, credentials, or material
   whose redistribution is not explicitly allowed.
4. Run the Rust and frontend CI checks. Release builds should additionally
   generate an SBOM and perform container/dependency scans in the deployment
   environment.

## Automated scanning (CI)

Every push and pull request runs, in addition to the SBOM/Trivy container scan
already in `supply-chain`:

- **`cargo audit`** (the `dependencies` job) against `Cargo.lock`. A new
  advisory with a fix available must be resolved by upgrading before merge,
  not by adding it to the ignore list below.
- **`npm audit --audit-level=high`** (`apps/web`, same job).
- **`gitleaks`** (the `secrets` job) over only the commits the push or pull
  request introduces — not the whole repository history — using
  `.gitleaks.toml` at the repo root, which extends gitleaks' default rules.

### Accepted exceptions

**`RUSTSEC-2023-0071`** (`rsa` crate, "Marvin Attack" timing side-channel; no
fixed upgrade exists) — ignored in the `cargo audit` CI step.

`rsa` reaches `Cargo.lock` only through `sqlx-mysql`'s optional RSA
authentication plugin. This workspace's `sqlx` dependency requests the
`postgres` feature only (see the root `Cargo.toml`); `sqlx-mysql` is resolved
into the lockfile (a Cargo quirk — `sqlx-macros-core` unifies backend features
across the workspace graph even when unused) but is never compiled into any
binary, confirmed by the absence of `sqlx-mysql`/`rsa` build artifacts under
`target/`. Re-check this exception whenever `sqlx` is upgraded, in case a
future release changes that resolution behavior or ships a fix.

**`.gitleaks.toml`'s test-password allowlist** — the end-to-end scripts under
`scripts/test_*.sh` each create a throwaway local account against a running
dev stack, using a fixed, obviously-synthetic password
(`Payer-Test-2026`, `Deadline-Test-2026`, and so on) so a leftover account from
a failed run is recognizable; the script's cleanup trap deletes the account
whether the run passes or fails. The allowlist is scoped to that path and
value shape only, and a canary secret in an unrelated file is still caught
with it active (verified when the rule was added).
