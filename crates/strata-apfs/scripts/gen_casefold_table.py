#!/usr/bin/env python3
"""Regenerate src/casefold_table.rs from a Unicode CaseFolding.txt.

Usage: python3 scripts/gen_casefold_table.py CaseFolding-17.0.0.txt > src/casefold_table.rs
(download from https://www.unicode.org/Public/<version>/ucd/CaseFolding.txt).
Keeps status C and F rows (full folding; no S, no T), sorted by code point.
"""
import re
import sys

src = open(sys.argv[1], encoding="utf-8").read()
m = re.search(r"CaseFolding-(\d+)\.(\d+)\.(\d+)\.txt", src)
if not m:
    sys.exit("cannot find the version line in the input")
major, minor, patch = m.groups()
rows = []
for line in src.splitlines():
    line = line.split("#", 1)[0].strip()
    if not line:
        continue
    code, status, mapping = [f.strip() for f in line.split(";")[:3]]
    if status not in ("C", "F"):
        continue
    rows.append((int(code, 16), [int(x, 16) for x in mapping.split()]))
rows.sort()
out = sys.stdout
out.write(f"//! Generated from Unicode {major}.{minor}.{patch} CaseFolding.txt (status C and F; no S, no T).\n")
out.write("//! Regenerate with crates/strata-apfs/scripts/gen_casefold_table.py. Do not edit.\n\n")
out.write("#[allow(dead_code)] // read by the unit tests and the oracle notes\n")
out.write(f"pub(crate) const UNICODE_VERSION: (u8, u8, u8) = ({major}, {minor}, {patch});\n\n")
out.write("/// (code point, full folding), sorted by code point.\n")
out.write("pub(crate) static FOLD: &[(char, &str)] = &[\n")
for c, m in rows:
    s = "".join(f"\\u{{{x:X}}}" for x in m)
    out.write(f"    ('\\u{{{c:X}}}', \"{s}\"),\n")
out.write("];\n")
