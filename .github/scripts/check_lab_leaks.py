#!/usr/bin/env python3
"""Fail if tracked text files name the lab, or quote raw binary addresses.

Project rule: lab captures, lab machine and VM names, personal home paths and
raw binary internals stay out of the repos; derived tables and write-ups only.
Write "lab host A/B", "the lab Windows 10 VM", "<home>/..." and describe what a
routine does instead of where it lives.

Fails on:
* lab machine names (the two lab hosts),
* lab VM clone names and VM folder names,
* personal home paths and the local handoff folder,
* image-relative code addresses: 0x4xxxxx-0x6xxxxx (exe image) and
  0x10xxxxxx (DLL image base), unless the value is on ALLOWED_HEX.

Pure Python (stdlib only), so it runs the same on Linux, macOS and Windows.
Run from the repo root: python3 .github/scripts/check_lab_leaks.py
With --pdf it also reads the text of every tracked PDF (needs the pypdf
package) and applies the name, VM and path rules, so a built manual cannot
carry a lab name.
The same script is shared across the Seihai suite repos; keep the copies equal.
"""
from __future__ import annotations

import re
import subprocess
import sys

SELF = ".github/scripts/check_lab_leaks.py"

# The patterns are spelled with character classes so this file does not match
# itself when another repo vendors it under a different path.
RULES: list[tuple[str, re.Pattern[str]]] = [
    ("lab machine name", re.compile(r"gwe[n]\s*-?\s*te[a]|\bmira[e]\b", re.I)),
    ("lab VM clone name", re.compile(
        r"\bRE La[b]\s+(?:VM\b|v\d)|\bv\d+prob[e]\d*\b"
        r"|\b(?:lion|tiger|sequoia|leopard|win10|debian)-v[m]\b", re.I)),
    ("home path", re.compile(
        r"/home/bria[n]\b|\bbrian-hac[k]\b|/Users/bria[n]\b|[A-Za-z]:\\+Users\\+bria[n]\b"
        r"|~/Claud[e]\b|%USERPROFILE%\\+Claud[e]\b", re.I)),
    ("binary address", re.compile(
        r"(?<![0-9A-Za-z_])0[xX](?:00)?[4-6][0-9a-fA-F]{5}(?![0-9A-Za-z_])"
        r"|(?<![0-9A-Za-z_])0[xX]10[0-9a-fA-F]{6}(?![0-9A-Za-z_])")),
]

# Values that look like addresses but are constants. Compared by value.
ALLOWED_HEX: dict[int, str] = {
    0x400000: "default PE image base / mask bit",
    0x10000000: "default DLL image base / mask bit",
    0x10325476: "MD5 initial state word D",
    0x106aa070: "SHA-256 round constant",
    0x404060: "colour value (RGB), seihai-g3d example background",
    0x5a78b4: "colour value (RGB), seihai-g3d sky",
}

# Files whose hex literals are packed data, not addresses. The address rule is
# skipped there; the name and path rules still apply.
ADDRESS_EXEMPT_PATHS: dict[str, str] = {
    "crates/seihai-core/src/baddies.rs": "packed baddy pose tables",
}

HEX_NUM = re.compile(r"0[xX]([0-9a-fA-F]+)")


def line_errors(line: str, check_addresses: bool = True) -> list[str]:
    errs: list[str] = []
    for label, rx in RULES:
        if label == "binary address" and not check_addresses:
            continue
        for m in rx.finditer(line):
            if label == "binary address":
                if int(HEX_NUM.match(m.group(0)).group(1), 16) in ALLOWED_HEX:
                    continue
            errs.append(f"{label}: {m.group(0)}")
    return errs


def tracked_paths() -> list[str]:
    out = subprocess.check_output(["git", "ls-files", "-z"])
    return [p.decode("utf-8", "surrogateescape") for p in out.split(b"\0") if p]


def pdf_leaks(path: str) -> list[str]:
    from pypdf import PdfReader  # only needed with --pdf

    found: list[str] = []
    for page_no, page in enumerate(PdfReader(path).pages, 1):
        for line in (page.extract_text() or "").splitlines():
            # Names and paths only: the address rule is for source text.
            for err in line_errors(line, check_addresses=False):
                found.append(f"{path}: page {page_no}: {err}")
    return found


def main() -> int:
    scan_pdfs = "--pdf" in sys.argv[1:]
    leaks: list[str] = []
    for path in tracked_paths():
        if path == SELF:
            continue
        if path.lower().endswith(".pdf"):
            if scan_pdfs:
                leaks.extend(pdf_leaks(path))
            continue
        for label, rx in RULES[:3]:
            if rx.search(path):
                leaks.append(f"{path}: file name has a {label}")
        try:
            data = open(path, "rb").read()
        except OSError:
            continue
        if b"\0" in data:
            continue  # binary
        text = data.decode("utf-8", "replace")
        check_addresses = path not in ADDRESS_EXEMPT_PATHS
        for n, line in enumerate(text.splitlines(), 1):
            for err in line_errors(line, check_addresses):
                leaks.append(f"{path}:{n}: {err}")
    if leaks:
        print("Lab-detail leak check failed:", file=sys.stderr)
        for leak in leaks:
            print("  " + leak, file=sys.stderr)
        print(f"{len(leaks)} hit(s). Use neutral wording (lab host A/B, "
              "the lab Windows 10 VM) and describe routines instead of "
              "quoting addresses.", file=sys.stderr)
        return 1
    print("Lab-detail leak check passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
