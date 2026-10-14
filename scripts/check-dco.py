#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 JustMyth / Tan Ming Jing
"""Developer Certificate of Origin check: every non-merge commit in the range must carry a
`Signed-off-by:` line naming its author (same email, case-insensitive).

usage: check-dco.py <rev-range>     e.g. origin/main..HEAD, or HEAD for the whole history
"""
import re
import subprocess
import sys

rng = sys.argv[1] if len(sys.argv) > 1 else "HEAD"
out = subprocess.run(
    ["git", "log", "--no-merges", "--format=%H%x1f%an%x1f%ae%x1f%B%x1e", rng],
    capture_output=True, text=True, check=True).stdout
bad = 0
n = 0
for rec in out.split("\x1e"):
    rec = rec.strip("\n")
    if not rec:
        continue
    sha, name, email, body = rec.split("\x1f", 3)
    n += 1
    signoffs = re.findall(r"^Signed-off-by:\s*(.+?)\s*<([^>]+)>\s*$", body, flags=re.M)
    if not any(e.lower() == email.lower() for _, e in signoffs):
        bad += 1
        found = ", ".join(f"{a} <{b}>" for a, b in signoffs) or "none"
        print(f"::error::commit {sha[:12]} by {name} <{email}> has no matching Signed-off-by "
              f"line (found: {found}). Use `git commit -s`; see CONTRIBUTING.md.")
print(f"DCO: {n} commit(s) checked, {bad} without a matching sign-off")
sys.exit(1 if bad else 0)
