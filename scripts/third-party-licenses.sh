#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Generate the third-party licence file shipped with every Notideck package.
#   scripts/third-party-licenses.sh <out_file> [--target <triple>]
#     --target <triple>  only crates that are built for that target (cargo metadata --filter-platform)
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT=""
TARGET=""
while [ $# -gt 0 ]; do
  case "$1" in
    --target) [ $# -ge 2 ] || { echo "--target needs a triple" >&2; exit 2; }; TARGET="$2"; shift ;;
    -h|--help) sed -n '2,5p' "$0"; exit 0 ;;
    -*) echo "unknown option: $1" >&2; exit 2 ;;
    *) [ -z "$OUT" ] || { echo "only one output file allowed" >&2; exit 2; }; OUT="$1" ;;
  esac
  shift
done
[ -n "$OUT" ] || { echo "usage: $0 <out_file> [--target <triple>]" >&2; exit 2; }

META_ARGS=(--format-version 1 --locked)
[ -z "$TARGET" ] || META_ARGS+=(--filter-platform "$TARGET")
VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p}' "$REPO/host/Cargo.toml" | head -1)"
WV2_VER="$(awk '/^name = "webview2-com-sys"/{getline; gsub(/version = |"/,""); print; exit}' "$REPO/host/Cargo.lock")"

cd "$REPO/host"
META="$(mktemp)"
trap 'rm -f "$META"' EXIT
cargo metadata "${META_ARGS[@]}" > "$META"

# Make sure every registry/git package is on disk (fresh machine).
if python3 -I - "$META" <<'PY'
import json, os, sys
d = json.load(open(sys.argv[1]))
sys.exit(1 if any(p.get("source") and not os.path.isdir(os.path.dirname(p["manifest_path"]))
                  for p in d["packages"]) else 0)
PY
then :; else
  echo "Fetching missing crate sources (cargo fetch --locked)..." >&2
  cargo fetch --locked ${TARGET:+--target "$TARGET"}
  cargo metadata "${META_ARGS[@]}" > "$META"
fi

REGISTRY="${CARGO_HOME:-$HOME/.cargo}/registry/src"
mkdir -p "$(dirname "$OUT")"
WV2_LICENSE="$REPO/packaging/windows/WebView2-LICENSE.txt" python3 -I - "$META" "$OUT" "${TARGET:-all targets}" "$VERSION" "$WV2_VER" "$REGISTRY" <<'PY'
import json, os, re, sys, glob, collections

meta_path, out_path, target, version, wv2_ver, registry = sys.argv[1:7]
d = json.load(open(meta_path))
by_id = {p["id"]: p for p in d["packages"]}
pkgs = [by_id[n["id"]] for n in d["resolve"]["nodes"] if by_id[n["id"]].get("source")]
pkgs.sort(key=lambda p: (p["name"], p["version"]))

NAME_RE = re.compile(r"^(licen[sc]e|copying|notice|unlicense|copyright)([^a-z0-9].*)?$", re.I)

def read(path):
    with open(path, "rb") as f:
        t = f.read().decode("utf-8", errors="replace")
    return "\n".join(l.rstrip() for l in t.replace("\r\n", "\n").replace("\r", "\n").split("\n")).strip("\n")

def licence_files(pkg):
    base = os.path.dirname(pkg["manifest_path"])
    found = []
    for name in sorted(os.listdir(base)):
        p = os.path.join(base, name)
        if os.path.isfile(p) and NAME_RE.match(name):
            found.append((name, p))
        elif os.path.isdir(p) and name.lower() in ("licenses", "licences"):
            for root, _, files in os.walk(p):
                for f in sorted(files):
                    fp = os.path.join(root, f)
                    found.append((os.path.relpath(fp, base), fp))
    if not found:
        # nothing at the top level: look a little deeper (vendored upstream sources)
        for root, dirs, files in os.walk(base):
            dirs[:] = sorted(x for x in dirs if x.lower() not in ("tests", "test", "examples", "benches", "target"))
            if os.path.relpath(root, base).count(os.sep) >= 2:
                dirs[:] = []
            for f in sorted(files):
                if root != base and NAME_RE.match(f):
                    fp = os.path.join(root, f)
                    found.append((os.path.relpath(fp, base), fp))
    lf = pkg.get("license_file")
    if lf:
        fp = os.path.join(base, lf)
        if os.path.isfile(fp) and fp not in [x[1] for x in found]:
            found.append((lf, fp))
    return found

# distinct texts, numbered in order of first use
texts = collections.OrderedDict()   # text -> {"id", "label", "users"}
def ref(text, label, user):
    e = texts.get(text)
    if e is None:
        e = texts[text] = {"id": len(texts) + 1, "label": label, "users": set()}
    e["users"].add(user)
    return e["id"]

# canonical MIT / Apache-2.0 from the tree: the most common LICENSE-APACHE / LICENSE-MIT text
apache_c = collections.Counter(); mit_c = collections.Counter()
per_pkg = {}
for p in pkgs:
    files = [(n, read(fp)) for n, fp in licence_files(p)]
    per_pkg[p["id"]] = files
    for n, t in files:
        b = os.path.basename(n).lower()
        if b.startswith("license-apache") or b.startswith("licence-apache"):
            if "Apache License" in t and "Version 2.0" in t:
                apache_c[t] += 1
        elif b.startswith("license-mit") or b.startswith("licence-mit"):
            if "Permission is hereby granted, free of charge" in t:
                mit_c[t] += 1
if not apache_c or not mit_c:
    sys.exit("error: no LICENSE-APACHE / LICENSE-MIT found in the crate tree to use as canonical text")
APACHE = apache_c.most_common(1)[0][0]
mit_full = mit_c.most_common(1)[0][0]
MIT = "\n".join(l for l in mit_full.split("\n") if not re.match(r"^\s*copyright\b", l, re.I)).strip("\n")
MIT = re.sub(r"\n{3,}", "\n\n", MIT)

BSD2 = ""
for p in pkgs:
    for n, t in per_pkg[p["id"]]:
        if "Redistributions of source code must retain" in t and "Neither the name" not in t and "endorse" not in t:
            BSD2 = "\n".join(l for l in t.split("\n") if not re.match(r"^\s*copyright\b", l, re.I)).strip("\n")
            break
    if BSD2:
        break

apache_id = ref(APACHE, "Apache License 2.0 (standard text)", "Android app")

def spdx_ids(expr):
    return [t for t in re.split(r"[\s()/]+", expr or "") if t and t not in ("OR", "AND", "WITH")]

blocks = []
nofile = []
for p in pkgs:
    lines = ["%s %s" % (p["name"], p["version"]),
             "  Licence:    %s" % (p.get("license") or "(not declared)"),
             "  Repository: %s" % (p.get("repository") or p.get("homepage") or "(none)")]
    files = per_pkg[p["id"]]
    if files:
        refs = []
        for n, t in files:
            refs.append("%s -> [%d]" % (n, ref(t, "%s from %s %s" % (os.path.basename(n), p["name"], p["version"]), p["name"] + " " + p["version"])))
        lines.append("  Files:      " + "; ".join(refs))
    else:
        ids = spdx_ids(p.get("license"))
        authors = ", ".join(p.get("authors") or []) or "(see repository)"
        if "MIT" in ids:
            n = ref(MIT, "MIT licence (standard text, no copyright line)", p["name"] + " " + p["version"])
            lines.append("  Files:      none shipped in the package; standard MIT text -> [%d]" % n)
            lines.append("  Copyright:  %s" % authors)
        elif "Apache-2.0" in ids:
            n = ref(APACHE, "", p["name"] + " " + p["version"])
            lines.append("  Files:      none shipped in the package; standard Apache-2.0 text -> [%d]" % n)
            lines.append("  Copyright:  %s" % authors)
        elif "BSD-2-Clause" in ids and BSD2:
            n = ref(BSD2, "BSD 2-Clause licence (standard text, no copyright line)", p["name"] + " " + p["version"])
            lines.append("  Files:      none shipped in the package; standard BSD-2-Clause text -> [%d]" % n)
            lines.append("  Copyright:  %s" % authors)
        else:
            lines.append("  Files:      none shipped in the package and no canonical text available")
        nofile.append("%s %s (%s)" % (p["name"], p["version"], p.get("license")))
    blocks.append("\n".join(lines))

# WebView2Loader.dll
wv2_dir = None
for c in glob.glob(os.path.join(registry, "*", "webview2-com-sys-%s" % wv2_ver)):
    wv2_dir = c; break
wv2_files = []
if wv2_dir:
    wv2_files = licence_files({"manifest_path": os.path.join(wv2_dir, "Cargo.toml")})
    for sub in ("x64", "x86", "arm64"):
        sd = os.path.join(wv2_dir, sub)
        if os.path.isdir(sd):
            wv2_files += [(os.path.join(sub, n), os.path.join(sd, n)) for n in sorted(os.listdir(sd)) if NAME_RE.match(n)]

H = "=" * 78
o = []
o.append("Notideck %s - third-party licences" % version)
o.append(H)
o.append("")
o.append("This file lists the third-party components compiled into Notideck and their "
         "licences. See THIRD_PARTY.md for the summary and thanks. Notideck itself is "
         "GPL-3.0-or-later (see LICENSE). Rust crates listed here: %s (target: %s)." % (len(pkgs), target))
o.append("")
o.append("Contents: 1. Android app   2. WebView2Loader.dll (Windows only)   3. Rust crates   4. Licence texts")
o.append("")
o.append(H); o.append("1. Android app (phone app, all Apache-2.0, text [%d])" % apache_id); o.append(H)
o.append("")
for lib in [
    "AndroidX core-ktx 1.13.1, activity-compose 1.9.0, lifecycle-runtime-ktx / lifecycle-viewmodel-compose 2.8.2",
    "Jetpack Compose ui and material3 (Compose BOM 2024.09.03)",
    "CameraX camera2 / lifecycle / view 1.3.4",
    "Media3 ExoPlayer, exoplayer-hls and ui 1.3.1",
    "Kotlin standard library (Kotlin 2.2.21)",
    "kotlinx.coroutines-android 1.8.1",
    "Square Wire runtime 6.4.5",
    "Conscrypt (conscrypt-android) 2.5.2",
    "ZXing core 3.5.3",
]:
    o.append("  - " + lib)
o.append("")
o.append("Licence: Apache License 2.0, full text in section 4 as [%d]." % apache_id)
o.append("")
o.append(H); o.append("2. WebView2Loader.dll (Windows only)"); o.append(H)
o.append("")
o.append("Windows packages ship WebView2Loader.dll, taken unmodified from the locked "
         "webview2-com-sys %s crate (the redistributable loader from Microsoft's WebView2 SDK)." % wv2_ver)
if wv2_files:
    refs = []
    for n, fp in wv2_files:
        refs.append("%s -> [%d]" % (n, ref(read(fp), "%s from the WebView2 loader (webview2-com-sys)" % n, "WebView2Loader.dll")))
    o.append("Licence file(s) found in the crate package: " + "; ".join(refs))
else:
    # The crate ships no licence file for the DLL. packaging/windows/WebView2-LICENSE.txt is the LICENSE.txt
    # of Microsoft's Microsoft.Web.WebView2 NuGet package (nuget.org/packages/Microsoft.Web.WebView2).
    o.append("Licence: Microsoft WebView2 SDK licence (BSD-3-Clause style), from the Microsoft.Web.WebView2 "
             "NuGet package, full text in section 4 as [%d]."
             % ref(read(os.environ["WV2_LICENSE"]), "Microsoft WebView2 SDK (Microsoft.Web.WebView2 NuGet)", "WebView2Loader.dll"))
o.append("")
o.append(H); o.append("3. Rust crates (%d)" % len(pkgs)); o.append(H)
o.append("")
o.append("\n\n".join(blocks))
o.append("")
o.append(H); o.append("4. Licence texts (%d distinct)" % len(texts)); o.append(H)
for t, e in texts.items():
    users = sorted(e["users"])
    shown = ", ".join(users[:6]) + (" and %d more" % (len(users) - 6) if len(users) > 6 else "")
    o.append("")
    o.append("-" * 78)
    o.append("[%d] %s" % (e["id"], e["label"] or "Apache License 2.0"))
    o.append("Used by: " + shown)
    o.append("-" * 78)
    o.append("")
    o.append(t)
o.append("")
with open(out_path, "w", encoding="utf-8", newline="\n") as f:
    f.write("\n".join(o))

sys.stderr.write("%s: %d crates, %d distinct licence texts, %d without a licence file\n"
                 % (out_path, len(pkgs), len(texts), len(nofile)))
for n in nofile:
    sys.stderr.write("  no licence file: %s\n" % n)
PY
