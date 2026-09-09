#!/usr/bin/env bash
# Fail when a relative Markdown link does not resolve on disk.
#
# rustdoc validates intra-doc links but never relative *file* links, and
# nothing validates them inside docs/ at all — so a path at the wrong depth,
# or one left behind by a moved file, ships silently. Every such link is
# resolved against the filesystem here.
#
# Remote URLs are deliberately out of scope: CI must not depend on network
# reachability, nor on a link target being publicly readable.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 2

status=0
checked=0

while IFS= read -r file; do
  while IFS= read -r hit; do
    line="${hit%%:*}"
    link="${hit#*:}"
    case "$link" in
      http://*|https://*|mailto:*|/*) continue ;;
    esac
    link="${link%%#*}"
    [ -n "$link" ] || continue
    checked=$((checked + 1))
    if [ ! -e "$(dirname "$file")/$link" ]; then
      echo "$file:$line: relative link does not resolve -> $link" >&2
      status=1
    fi
  done < <(grep -nEo '\]\([^)]+\.md(#[^)]*)?\)' "$file" \
           | sed -E 's/^([0-9]+):\]\((.*)\)$/\1:\2/')
done < <(find crates docs -type f \( -name '*.rs' -o -name '*.md' \) && echo README.md)

if [ "$status" -eq 0 ]; then
  echo "relative Markdown links: $checked checked, all resolve"
else
  echo "relative Markdown links: $checked checked, see failures above" >&2
fi
exit "$status"
