#!/bin/sh
# Prints the CHANGELOG.md section for one version — the text of that release's
# GitHub Release notes — and fails when there is none.
#
#   scripts/release-notes.sh 0.31.0            # section "## [0.31.0] - ..."
#   scripts/release-notes.sh 0.31.0 path.md    # a different changelog file
#
# Used twice: auto-tag.yml runs it before it creates a tag, so a version bump
# without release notes never becomes a release; release.yml runs it to write the
# release body. An empty section fails the same as a missing one.
set -eu

version="${1:?usage: release-notes.sh <version> [changelog]}"
file="${2:-$(dirname "$0")/../CHANGELOG.md}"

if [ ! -f "$file" ]; then
  echo "error: $file not found" >&2
  exit 1
fi

notes=$(awk -v head="## [$version]" '
  { sub(/\r$/, "") }
  index($0, "## [") == 1 {
    if (found) exit
    if (substr($0, 1, length(head)) == head) { found = 1; next }
  }
  found { print }
' "$file")

# Drop leading and trailing blank lines; what is left must contain text.
notes=$(printf '%s\n' "$notes" | awk 'NF { started = 1 } started { lines[++n] = $0 } END { while (n > 0 && lines[n] !~ /[^[:space:]]/) n--; for (i = 1; i <= n; i++) print lines[i] }')

if [ -z "$notes" ]; then
  echo "error: $file has no release notes for $version — add a '## [$version] - YYYY-MM-DD' section" >&2
  exit 1
fi

printf '%s\n' "$notes"
