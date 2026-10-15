#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 JustMyth / Tan Ming Jing
"""Third-party licence inventory and notices from `cargo metadata`.

usage: cargo metadata --format-version 1 --locked > meta.json
       python3 scripts/third-party-licenses.py meta.json > THIRD_PARTY_LICENSES.md
       python3 scripts/third-party-licenses.py meta.json --notices --out THIRD_PARTY_NOTICES.txt

The inventory lists every crate MythBrowse can link on any supported platform with its declared
SPDX licence. `--notices` writes the licence / copyright files each crate ships (LICENSE*,
COPYING*, NOTICE*, COPYRIGHT*) as found in the crate sources (needs the crates downloaded, e.g.
after a build or `cargo fetch`), including those of libraries a crate bundles in subdirectories
(for example the libraries inside SpiderMonkey's `mozjs_sys`). A text that several crates ship
word for word is printed once and referenced by its tag ([T1], [T2], ...).

For a crate that ships no licence file, its entry lists the copyright lines found at the top of
its source files and its Cargo.toml authors, and the appendix prints the standard text of each
licence such a crate names (LICENSES/third-party/*.txt: the SPDX License List texts,
github.com/spdx/license-list-data). For MIT- or BSD-only crates among them, the licence file of
the upstream repository is printed as well (LICENSES/third-party/upstream/, copied from the
repository named in the crate's Cargo.toml).
"""
import hashlib
import json
import os
import re
import sys
from collections import Counter

m = json.load(open(sys.argv[1], encoding="utf-8-sig"))
notices = "--notices" in sys.argv
if "--out" in sys.argv:
    sys.stdout = open(sys.argv[sys.argv.index("--out") + 1], "w", encoding="utf-8", newline="\n")
ws = set(m["workspace_members"])
nodes = {n["id"]: n for n in m["resolve"]["nodes"]}
seen, stack = set(), list(ws)
while stack:
    i = stack.pop()
    if i in seen:
        continue
    seen.add(i)
    stack += [d["pkg"] for d in nodes[i]["deps"]]
pk = {p["id"]: p for p in m["packages"]}
deps = sorted((pk[i] for i in seen if i not in ws), key=lambda p: (p["name"], p["version"]))

if notices:
    ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    TEXTS = os.path.join(ROOT, "LICENSES", "third-party")
    PREFIXES = ("LICENSE", "LICENCE", "COPYING", "NOTICE", "COPYRIGHT", "UNLICENSE")
    # Bundled-library licence files are searched below the crate root, skipping directories that
    # hold only tests, tooling or build scripts (not part of the program).
    SKIP_DIRS = {"test", "tests", "testing", "tools", "python", "examples", "example", "benches",
                 "bench", "docs", "doc", "fuzz", "fuzzing", ".github", "devtools", "pgo", "target"}
    # Upstream licence files for crates that ship none (keyed by crate name): MIT- / BSD-only
    # crates, the Chromium licence accesskit's sources refer to, and ICU 77, which SpiderMonkey
    # (`mozjs_sys`) bundles without its LICENSE file.
    UPSTREAM = {
        "alloc-stdlib": [("alloc-no-stdlib.txt", "upstream licence file")],
        "malloc_buf": [("malloc_buf.txt", "upstream licence file")],
        "nom-language": [("nom.txt", "upstream licence file")],
        "simd_helpers": [("simd_helpers.txt", "upstream licence file")],
        "taffy": [("taffy.txt", "upstream licence file")],
        "void": [("void.txt", "upstream licence file")],
        "accesskit": [("accesskit-LICENSE.chromium.txt", "upstream LICENSE.chromium (referenced by the crate sources)")],
        "mozjs_sys": [("icu.txt", "ICU 77 LICENSE (bundled library mozjs/intl/icu, from unicode-org/icu)")],
    }
    for n in ("block", "block2", "dispatch", "objc-sys", "objc2", "objc2-encode") + tuple(
            p["name"] for p in deps if p["name"].startswith("objc2-")):
        UPSTREAM.setdefault(n, [("objc2-0.5.txt", "upstream licence file (objc2 0.5)"),
                                ("objc2.txt", "upstream licence file")])
    # Permissive licence notices kept in the leading comment of a source file.
    HEADER_LICENCE = re.compile(r"Redistributions? (of source code|in binary form)|Permission is hereby granted|"
                                r"Permission to use, copy, modify", re.I)
    SPDX_FILES = {"MIT": "MIT.txt", "Apache-2.0": "Apache-2.0.txt", "LLVM-exception": "LLVM-exception.txt",
                  "BSD-3-Clause": "BSD-3-Clause.txt", "Zlib": "Zlib.txt", "BSL-1.0": "BSL-1.0.txt",
                  "CC0-1.0": "CC0-1.0.txt", "MPL-2.0": "../MPL-2.0.txt"}
    COPYRIGHT = re.compile(r"(?i)(copyright\s*(\(c\)|©|\d{4})|©\s*\d{4})")

    def licence_files(d):
        top, nested = [], []
        if not os.path.isdir(d):
            return top, nested
        for root, dirs, files in os.walk(d):
            dirs[:] = sorted(x for x in dirs if x.lower() not in SKIP_DIRS and not x.endswith(".dist-info"))
            for f in sorted(files):
                if f.upper().startswith(PREFIXES) and os.path.isfile(os.path.join(root, f)):
                    (top if root == d else nested).append(os.path.join(root, f))
        return top, nested

    def copyright_lines(d, limit=60):
        found = []
        for root, dirs, files in os.walk(d):
            dirs[:] = sorted(x for x in dirs if x.lower() not in SKIP_DIRS)
            for f in sorted(files):
                if not f.endswith((".rs", ".c", ".h", ".cc", ".cpp", ".py", ".toml", ".md", ".txt")):
                    continue
                try:
                    with open(os.path.join(root, f), encoding="utf-8", errors="replace") as fh:
                        for k, line in enumerate(fh):
                            if k > 40:
                                break
                            if COPYRIGHT.search(line):
                                s = re.sub(r"^[\s/*#!;-]+", "", line).strip()[:160]
                                if s and s not in found:
                                    found.append(s)
                except OSError:
                    continue
                if len(found) >= limit:
                    return found
        return found

    def ids(expr):
        out = []
        for t in re.split(r"[\s()/]+", expr or ""):
            if t and t not in ("OR", "AND", "WITH") and t not in out:
                out.append(t)
        return out

    def header_notices(d, limit=400):
        """Leading comment blocks of source files that carry a permissive licence notice."""
        blocks = []
        for root, dirs, files in os.walk(d):
            dirs[:] = sorted(x for x in dirs if x.lower() not in SKIP_DIRS and not x.endswith(".dist-info"))
            for f in sorted(files):
                if not f.endswith((".rs", ".c", ".h", ".cc", ".cpp", ".hpp", ".inc", ".S", ".asm")):
                    continue
                try:
                    with open(os.path.join(root, f), encoding="utf-8", errors="replace") as fh:
                        head = []
                        for k, line in enumerate(fh):
                            st = line.strip()
                            if k > 150 or (st and not st.startswith(("/*", "*", "//", "*/", ";"))):
                                break
                            head.append(line.rstrip())
                except OSError:
                    continue
                text = "\n".join(head).strip()
                if HEADER_LICENCE.search(text):
                    blocks.append((os.path.relpath(os.path.join(root, f), d).replace(os.sep, "/"), text))
                    if len(blocks) >= limit:
                        return blocks
        return blocks

    def emit_text(label, body, owner):
        h = hashlib.sha256("\n".join(l.rstrip() for l in body.splitlines()).encode()).hexdigest()
        if h in texts:
            tag, first = texts[h]
            print(f"\n--- {label} --- identical to text {tag} ({first}), printed above.")
            return
        tag = f"[T{len(texts) + 1}]"
        texts[h] = (tag, f"{owner}, {label}")
        print(f"\n--- {label} --- text {tag}")
        print(body)

    texts = {}  # sha -> (tag, first owner)
    no_file = []
    mpl = []

    def emit(label, path, owner):
        emit_text(label, open(path, encoding="utf-8", errors="replace").read().rstrip(), owner)

    print("THIRD-PARTY SOFTWARE NOTICES AND LICENCES FOR MYTHBROWSE")
    print("=" * 60)
    print("Generated by scripts/third-party-licenses.py from Cargo.lock. For each crate: name,")
    print("version, declared licence, and the licence / notice files the crate ships, including")
    print("those of libraries it bundles. A text several crates ship word for word is printed once")
    print("and referenced by its tag. Crates that ship no licence file are listed with their")
    print("copyright lines; the standard texts of their licences are in the appendix at the end.\n")
    for p in deps:
        d = os.path.dirname(p["manifest_path"])
        top, nested = licence_files(d)
        owner = f"{p['name']} {p['version']}"
        lic = p["license"] or "see crate"
        print("-" * 78)
        print(f"{owner}  ({lic})")
        if p.get("repository"):
            print(p["repository"])
        if "MPL-2.0" in ids(lic):
            mpl.append(p)
        if not top:
            no_file.append(p)
            print("(This crate ships no licence file of its own; it is distributed under the licence")
            print(" named above, whose standard text is in the appendix.)")
            lines = copyright_lines(d)
            if lines:
                print("Copyright lines in the crate sources:")
                for s in lines:
                    print(f"  {s}")
            if p.get("authors"):
                print("Authors (Cargo.toml): " + ", ".join(re.sub(r"\s*<[^>]*>", "", a) for a in p["authors"]))
            for f, what in UPSTREAM.get(p["name"], []):
                fp = os.path.join(TEXTS, "upstream", f)
                if os.path.isfile(fp):
                    emit(f"{what} ({p.get('repository') or 'repository'})", fp, owner)
            for rel, body in header_notices(d):
                emit_text(f"{rel} (licence notice in the source file)", body, owner)
        for fp in top:
            emit(os.path.basename(fp), fp, owner)
        for fp in nested:
            emit(os.path.relpath(fp, d).replace(os.sep, "/") + " (bundled library)", fp, owner)
        print()
    print("-" * 78)
    print(f"{len(deps)} crates; {len(no_file)} ship no licence file of their own; "
          f"{len(texts)} distinct licence / notice texts.\n")

    print("=" * 78)
    print("APPENDIX A. STANDARD LICENCE TEXTS FOR CRATES THAT SHIP NO LICENCE FILE")
    print("=" * 78)
    print("Each text applies to the crates listed under it, with the copyright holders given in")
    print("each crate's entry above (or, if none is given, the crate's authors). Where a crate")
    print("offers a choice of licences, every offered text is listed.\n")
    by_id = {}
    for p in no_file:
        for t in ids(p["license"]):
            by_id.setdefault(t, []).append(f"{p['name']} {p['version']}")
    missing_text = []
    for t in sorted(by_id):
        f = SPDX_FILES.get(t)
        fp = os.path.normpath(os.path.join(TEXTS, f)) if f else None
        print("-" * 78)
        print(f"{t}: " + ", ".join(by_id[t]))
        if fp and os.path.isfile(fp):
            print()
            print(open(fp, encoding="utf-8").read().rstrip())
        elif all(any(SPDX_FILES.get(o) for o in ids(p["license"]) if o != t)
                 for p in no_file if t in ids(p["license"])) and " OR " in " ".join(
                     p["license"] for p in no_file if t in ids(p["license"])):
            print("(an alternative these crates offer; MythBrowse uses them under another licence")
            print(" they offer, whose text is in this appendix)")
        else:
            missing_text.append(t)
            print("(standard text not bundled)")
        print()

    print("=" * 78)
    print("APPENDIX B. SOURCE CODE OF MPL-2.0 COMPONENTS")
    print("=" * 78)
    print("The following components are covered by the Mozilla Public License 2.0 (text in")
    print("LICENSE). Their Source Code Form is available from crates.io at")
    print("https://crates.io/crates/<name>/<version> and from the repository listed; the ones")
    print("MythBrowse modifies are in third_party/ of the MythBrowse source")
    print("(https://github.com/devtmj1123/MythBrowse), with every change listed there.\n")
    for p in mpl:
        where = "third_party/ (MythBrowse source)" if p["source"] is None else \
            f"https://crates.io/crates/{p['name']}/{p['version']}"
        print(f"  {p['name']} {p['version']}: {where}" + (f"  {p['repository']}" if p.get("repository") else ""))
    if missing_text:
        print(f"\nNOTE: no standard text bundled for: {', '.join(missing_text)}", file=sys.stderr)
    sys.exit(0)

rows = [(p["name"], p["version"], p["license"] or "see crate", p.get("repository") or "",
         "vendored in third_party/" if p["source"] is None else "crates.io") for p in deps]
print("# Third-party licences\n")
print("Generated by `scripts/third-party-licenses.py` from `Cargo.lock`. Covers every Rust crate")
print("MythBrowse can link on any supported platform, with the licence each crate declares (SPDX).")
print("The licence texts and copyright notices are in `THIRD_PARTY_NOTICES.txt`, which Windows")
print("packages ship next to `mythbrowse.exe`.\n")
print("Also used outside Cargo: ANGLE (BSD-3-Clause, via `mozangle`), SQLite (public domain, via")
print("`libsqlite3-sys`), SpiderMonkey (MPL-2.0, via `mozjs_sys`), the Mozilla Public Suffix List")
print("(MPL-2.0, `crates/passwords/data/`). See also `NOTICE`.\n")
print("## Summary\n")
print("| Licence | Crates |\n|---|---|")
for lic, c in Counter(r[2] for r in rows).most_common():
    print(f"| {lic} | {c} |")
print(f"\n{len(rows)} crates.\n\n## Crates\n")
print("| Crate | Version | Licence | Source | Repository |\n|---|---|---|---|---|")
for n, v, lic, repo, src in rows:
    print(f"| {n} | {v} | {lic} | {src} | {repo} |")
