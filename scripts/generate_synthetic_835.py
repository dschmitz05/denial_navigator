#!/usr/bin/env python3
"""Emit a deterministic, PHI-free 835 fixture for parser and UI testing."""
from pathlib import Path

source = Path(__file__).with_name("sample_835.txt")
target = Path(__file__).with_name("generated_synthetic_835.txt")
text = source.read_text(encoding="utf-8")
if "ISA" not in text or "CLP" not in text:
    raise SystemExit("sample fixture is not an 835")
target.write_text(text, encoding="utf-8")
print(target)
