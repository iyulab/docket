#!/bin/bash
# Tests scripts/install.sh (run through `sh`, as the documented `curl | sh`
# does) without touching the network: a fake `curl` writes a
# fixture instead of downloading and a fake `uname` pins the platform, so the
# same checks run on Linux and macOS.
#
# What it pins down:
#   * re-running the installer never rewrites an installed launcher in place.
#     macOS keeps a binary's code signature cached per file, so overwriting a
#     launcher that has run before makes the next launch die with "Code
#     Signature Invalid". A second name for the old file (a hard link) keeps
#     its content only if the installer replaced the path instead of writing
#     through it.
#   * a failed download leaves the existing install exactly as it was - both
#     launchers, not just the one fetched first - and leaves no temp files.
#   * downloads retry transient errors.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
installer="$here/install.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fakebin="$work/fakebin"
mkdir -p "$fakebin"

cat >"$fakebin/uname" <<'EOF'
#!/bin/sh
case "$1" in
  -s) echo Linux ;;
  -m) echo x86_64 ;;
  *) echo Linux ;;
esac
EOF

# FAKE_VERSION is what the "download" contains; FAKE_FAIL_ON makes a download
# fail (curl exit 22, as `-f` does on an HTTP error) when the URL contains it.
cat >"$fakebin/curl" <<'EOF'
#!/bin/sh
out=""
url=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    -*) echo "$1" >>"$FAKE_CURL_LOG"; shift ;;
    *) url="$1"; shift ;;
  esac
done
if [ -n "${FAKE_FAIL_ON:-}" ]; then
  case "$url" in *"$FAKE_FAIL_ON"*) exit 22 ;; esac
fi
printf '#!/bin/sh\n# %s %s\n' "${FAKE_VERSION}" "${url##*/}" >"$out"
EOF
chmod +x "$fakebin/uname" "$fakebin/curl"

export FAKE_CURL_LOG="$work/curl-args.log"
: >"$FAKE_CURL_LOG"

failures=0
fail() { echo "FAIL: $*" >&2; failures=$((failures + 1)); }

run_install() { # <install dir> <version> [fail-on]
  PATH="$fakebin:$PATH" DOCKET_INSTALL_DIR="$1" FAKE_VERSION="$2" FAKE_FAIL_ON="${3:-}" \
    sh "$installer" >/dev/null 2>"$work/stderr"
}

# --- a fresh install -----------------------------------------------------
dir="$work/fresh"
if run_install "$dir" v1; then
  for w in docket-mcp docket-cc; do
    [ -x "$dir/$w" ] || fail "fresh: $w is missing or not executable"
    grep -q '^# v1 ' "$dir/$w" || fail "fresh: $w has the wrong content"
  done
  [ "$(ls -A "$dir" | wc -l | tr -d ' ')" = 2 ] || fail "fresh: stray files in the install dir: $(ls -A "$dir")"
else
  fail "fresh: installer exited non-zero: $(cat "$work/stderr")"
fi

# --- a re-run replaces the file, it does not write through it --------------
ln "$dir/docket-mcp" "$work/old-mcp" 2>/dev/null || fail "re-run: cannot create the hard link this check needs"
if run_install "$dir" v2; then
  grep -q '^# v2 ' "$dir/docket-mcp" || fail "re-run: docket-mcp was not updated"
  grep -q '^# v2 ' "$dir/docket-cc" || fail "re-run: docket-cc was not updated"
  [ -x "$dir/docket-mcp" ] || fail "re-run: docket-mcp lost its executable bit"
  grep -q '^# v1 ' "$work/old-mcp" || fail "re-run: the installed file was rewritten in place (its other name now reads: $(cat "$work/old-mcp"))"
else
  fail "re-run: installer exited non-zero: $(cat "$work/stderr")"
fi

# --- a failed download leaves the whole install untouched --------------------
if run_install "$dir" v3 docket-cc-launcher; then
  fail "partial failure: installer exited zero although a download failed"
fi
grep -q '^# v2 ' "$dir/docket-mcp" || fail "partial failure: docket-mcp was updated although docket-cc failed"
grep -q '^# v2 ' "$dir/docket-cc" || fail "partial failure: docket-cc changed"
[ "$(ls -A "$dir" | wc -l | tr -d ' ')" = 2 ] || fail "partial failure: temp files left behind: $(ls -A "$dir")"

# --- transient download errors are retried ------------------------------------
grep -q -- '--retry' "$FAKE_CURL_LOG" || fail "downloads do not ask curl to retry"

if [ "$failures" -ne 0 ]; then
  echo "$failures check(s) failed" >&2
  exit 1
fi
echo "install.sh tests passed"
