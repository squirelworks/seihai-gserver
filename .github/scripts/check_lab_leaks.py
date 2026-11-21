#!/usr/bin/env python3
"""Fail if tracked text files name the lab, show a home path, or quote raw
binary addresses.

Project rule: lab captures, lab machine and VM names, personal home paths and
raw binary internals stay out of the repos; derived tables and write-ups only.
Write "lab host A/B" or "the lab Windows 10 VM", and describe what a routine
does instead of where it lives.

The names themselves are not in this file, so the check cannot leak them. They
come from the LAB_LEAK_DENYLIST environment variable (CI passes the repository
secret of that name): one Python regular expression per line, matched without
regard to case; blank lines and lines starting with # are ignored. The rules
kept here are generic:

* home paths under /home/, /Users/ or X:\\Users\\ (CI runner accounts and
  placeholders in angle brackets are fine),
* image-relative code addresses: 0x4xxxxx-0x6xxxxx (exe image) and
  0x10xxxxxx (DLL image base), unless the value is on ALLOWED_HEX.

When LAB_LEAK_REQUIRE_DENYLIST is "true" (CI sets it except on pull requests
from forks, which get no secrets), a missing denylist is an error, so the
name check cannot silently switch off.

With --pdf it also reads the text of every tracked PDF (needs the pypdf
package) and applies the denylist to it.

Pure Python (stdlib only, plus pypdf for --pdf). Run from the repo root:
    LAB_LEAK_DENYLIST="$(cat my-denylist.txt)" python3 .github/scripts/check_lab_leaks.py
The same script is shared across the Seihai suite repos; keep the copies equal.
"""
from __future__ import annotations

import os
import re
import subprocess
import sys

SELF = ".github/scripts/check_lab_leaks.py"

HOME_RE = re.compile(
    r"/home/(?!runner\b|user\b|ubuntu\b|<)[a-z_][a-z0-9_.-]*"
    r"|/Users/(?!Shared\b|runner\b|<)[A-Za-z][A-Za-z0-9_.-]*"
    r"|\b[A-Za-z]:\\+Users\\+(?!Public\b|runneradmin\b|<)[A-Za-z][A-Za-z0-9_.-]*"
)
ADDRESS_RE = re.compile(
    r"(?<![0-9A-Za-z_])0[xX](?:00)?[4-6][0-9a-fA-F]{5}(?![0-9A-Za-z_])"
    r"|(?<![0-9A-Za-z_])0[xX]10[0-9a-fA-F]{6}(?![0-9A-Za-z_])"
)

# Values that look like addresses but are constants. Compared by value. They
# are written with digit separators so the suite's other address checks, which
# look for a plain 0x literal, do not flag this list.
ALLOWED_HEX: dict[int, str] = {
    0x40_0000: "default PE image base / mask bit",
    0x1000_0000: "default DLL image base / mask bit",
    0x1032_5476: "MD5 initial state word D",
    0x106a_a070: "SHA-256 round constant",
    0x40_4060: "colour value (RGB), seihai-g3d example background",
    0x5a_78b4: "colour value (RGB), seihai-g3d sky",
}

# Files whose hex literals are packed data, not addresses. The address rule is
# skipped there; the other rules still apply.
ADDRESS_EXEMPT_PATHS: dict[str, str] = {
    "crates/seihai-core/src/baddies.rs": "packed baddy pose tables",
}


def load_denylist() -> list[re.Pattern[str]]:
    raw = os.environ.get("LAB_LEAK_DENYLIST", "")
    pats = []
    for line in raw.splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            pats.append(re.compile(line, re.I))
    return pats


def line_errors(line: str, deny: list[re.Pattern[str]],
                check_addresses: bool = True, generic: bool = True) -> list[str]:
    errs: list[str] = []
    for rx in deny:
        for m in rx.finditer(line):
            errs.append(f"denylisted lab detail: {m.group(0)}")
    if generic:
        for m in HOME_RE.finditer(line):
            errs.append(f"home path: {m.group(0)}")
    if check_addresses:
        for m in ADDRESS_RE.finditer(line):
            if int(m.group(0)[2:], 16) not in ALLOWED_HEX:
                errs.append(f"binary address: {m.group(0)}")
    return errs


def tracked_paths() -> list[str]:
    out = subprocess.check_output(["git", "ls-files", "-z"])
    return [p.decode("utf-8", "surrogateescape") for p in out.split(b"\0") if p]


def pdf_leaks(path: str, deny: list[re.Pattern[str]]) -> list[str]:
    from pypdf import PdfReader  # only needed with --pdf

    found: list[str] = []
    for page_no, page in enumerate(PdfReader(path).pages, 1):
        for line in (page.extract_text() or "").splitlines():
            # Names only: the address and path rules are for source text.
            for err in line_errors(line, deny, check_addresses=False, generic=False):
                found.append(f"{path}: page {page_no}: {err}")
    return found


def main() -> int:
    scan_pdfs = "--pdf" in sys.argv[1:]
    deny = load_denylist()
    if not deny:
        if os.environ.get("LAB_LEAK_REQUIRE_DENYLIST", "").lower() == "true":
            print("LAB_LEAK_DENYLIST is empty: add the repository secret "
                  "LAB_LEAK_DENYLIST (one regex per line).", file=sys.stderr)
            return 1
        print("note: LAB_LEAK_DENYLIST not set; checking only home paths "
              "and addresses.")
    leaks: list[str] = []
    for path in tracked_paths():
        if path == SELF:
            continue
        for rx in deny:
            if rx.search(path):
                leaks.append(f"{path}: file name has a denylisted lab detail")
        if path.lower().endswith(".pdf"):
            if scan_pdfs and deny:
                leaks.extend(pdf_leaks(path, deny))
            continue
        try:
            data = open(path, "rb").read()
        except OSError:
            continue
        if b"\0" in data:
            continue  # binary
        text = data.decode("utf-8", "replace")
        check_addresses = path not in ADDRESS_EXEMPT_PATHS
        for n, line in enumerate(text.splitlines(), 1):
            for err in line_errors(line, deny, check_addresses):
                leaks.append(f"{path}:{n}: {err}")
    if leaks:
        print("Lab-detail leak check failed:", file=sys.stderr)
        for leak in leaks:
            print("  " + leak, file=sys.stderr)
        print(f"{len(leaks)} hit(s). Use neutral wording (lab host A/B, "
              "the lab Windows 10 VM, <home>/...) and describe routines "
              "instead of quoting addresses.", file=sys.stderr)
        return 1
    print("Lab-detail leak check passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
