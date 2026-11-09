#!/usr/bin/env bash
# Fake `gh` for process tests: no existing PR, `pr create` returns PR #77.
case "$1 $2" in
  "pr view") exit 1 ;;
  "pr create") echo "https://github.com/acme/app/pull/77" ;;
  *) echo "fake gh: unsupported $*" >&2; exit 1 ;;
esac
