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
