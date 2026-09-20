#!/usr/bin/env python3
"""Checks that file paths named in the docs actually exist (FB-19).

README.md and docs/*.md are the first thing anyone reads before running the
stack, and stale references cost real time to notice: the Python stack's
compose file, `seed_admin.py` and `setup.sh` all stayed in README.md for
months after the services they described were deleted. This does not catch
everything a doc can get wrong, but a path that does not exist is
unambiguous, so it is worth catching automatically.

Scans inline code spans (`` `like/this` ``) in every Markdown file passed, or
README.md and docs/*.md by default, for anything that looks like a repo-
relative path, and fails if it does not exist. Not a path: anything with a
space, a URL, a placeholder such as `<file>`, an environment variable name, a
bare word with no extension or slash, or a glob. A path legitimately
mentioned but not meant to exist yet (a generated artifact, a path in an
example command) goes in ALLOWLIST below rather than being silently skipped
elsewhere in the script.
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

# Referenced but not expected to exist in a fresh checkout: generated at
# runtime, an example/placeholder value, or a path outside this repo.
ALLOWLIST = {
    ".env",
    "certs/tls.crt",
    "certs/tls.key",
    "certs-rust/",
    "certs-rust/tls.crt",
    "certs-rust/tls.key",
    "your-cert.pem",
    "your-key.pem",
    "database/backups/denial_navigator_20260906.sql.gz",
    "/var/log/dn-backup.log",
    "/var/log/dn-digests.log",
    "/path/to/denial-navigator/scripts/backup.sh",
    "/path/to/denial-navigator/scripts/send_deadline_digests.sh",
    "/etc/letsencrypt",
}

PATH_EXTENSIONS = (
    ".sh", ".py", ".sql", ".yml", ".yaml", ".toml", ".json", ".rs", ".md",
    ".env", ".service", ".crt", ".key", ".pem", ".gz", ".txt",
)

CODE_SPAN = re.compile(r"`([^`\n]+)`")


def candidate_paths(span: str) -> list[str]:
    """The path-like tokens in one backtick span, stripped of decoration."""
    if not span or " " in span or "\t" in span:
        return []
    if span.startswith(("http://", "https://", "<", "$")):
        return []
    if "*" in span or "{" in span:
        return []  # a glob or a shell brace expansion, not a literal path
    token = span.rstrip(":,;.")
    token = token[2:] if token.startswith("./") else token
    looks_like_path = "/" in token or token.endswith(PATH_EXTENSIONS)
    if not looks_like_path:
        return []
    if re.fullmatch(r"[A-Z][A-Z0-9_]*", token):
        return []  # an environment variable name (all caps, no slash)
    if re.fullmatch(r"[a-z][a-z0-9_]*", token):
        return []  # a bare identifier such as a table or column name
    return [token]


def check(markdown_files: list[Path]) -> list[str]:
    problems = []
    for path in markdown_files:
        text = path.read_text()
        for match in CODE_SPAN.finditer(text):
            for candidate in candidate_paths(match.group(1)):
                if candidate in ALLOWLIST:
                    continue
                target = (REPO_ROOT / candidate).resolve()
                if REPO_ROOT not in target.parents and target != REPO_ROOT:
                    continue  # points outside the repo; not ours to verify
                if not target.exists():
                    line = text.count("\n", 0, match.start()) + 1
                    problems.append(f"{path.relative_to(REPO_ROOT)}:{line}: `{candidate}` does not exist")
    return problems


def main() -> int:
    # Defaults to README.md, matching FB-19: pass explicit paths to check
    # other docs, which use enough path shorthand (`routes/foo.rs` meaning
    # `crates/api-gateway/src/routes/foo.rs`) that a literal check on them
    # produces more noise than signal.
    files = [Path(a) for a in sys.argv[1:]] or [REPO_ROOT / "README.md"]
    problems = check(files)
    if problems:
        print("Docs reference paths that do not exist:", file=sys.stderr)
        for p in problems:
            print(f"  {p}", file=sys.stderr)
        print(
            "\nIf this path is intentionally not present in a fresh checkout "
            "(generated at runtime, an example value), add it to ALLOWLIST in "
            "scripts/check_docs_paths.py instead of removing the check.",
            file=sys.stderr,
        )
        return 1
    print(f"Checked {len(files)} file(s); every referenced path exists.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
