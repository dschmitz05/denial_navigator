#!/usr/bin/env bash
set -euo pipefail

# Create a signed SHA-256 manifest for already-built, immutable release files.
# This deliberately does not build, tag, or upload anything.
#
# Usage:
#   ./scripts/sign_release_checksums.sh dist/v1.0.0
#
# The caller must have a signing-capable GPG secret key available. The script
# refuses empty directories and excludes any previous manifest/signature so it
# is safe to re-run after replacing an artifact.

if [[ $# -ne 1 ]]; then
  echo "Usage: $0 <release-directory>" >&2
  exit 2
fi

release_dir="$1"
if [[ ! -d "$release_dir" ]]; then
  echo "Release directory does not exist: $release_dir" >&2
  exit 2
fi

manifest="$release_dir/SHA256SUMS"
signature="$manifest.asc"
mapfile -d '' artifacts < <(
  find "$release_dir" -maxdepth 1 -type f \
    ! -name 'SHA256SUMS' ! -name 'SHA256SUMS.asc' -print0 | sort -z
)

if [[ ${#artifacts[@]} -eq 0 ]]; then
  echo "No release artifacts found in: $release_dir" >&2
  exit 2
fi

(
  cd "$release_dir"
  for artifact in "${artifacts[@]}"; do
    sha256sum "$(basename "$artifact")"
  done
) >"$manifest"

gpg --batch --armor --detach-sign --output "$signature" "$manifest"
gpg --verify "$signature" "$manifest"

echo "Created and verified: $manifest"
echo "Created and verified: $signature"
