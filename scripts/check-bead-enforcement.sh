#!/usr/bin/env bash
# Enforce bd (beads) for issue tracking: any TODO/FIXME/XXX/BUG/HACK marker
# or unchecked "- [ ]" list item added to the tree must reference a bead id
# (Myx-xxxx), so no work item lives outside the issue tracker.
#
# Usage:
#   scripts/check-bead-enforcement.sh --staged        # staged additions (local hook)
#   scripts/check-bead-enforcement.sh --diff < diff   # added lines of a unified diff (CI)
#   scripts/check-bead-enforcement.sh file...         # whole files
#
# Exit 1 with the offending lines listed when a marker lacks a bead ref.
# When the `bd` CLI is available, referenced ids are also checked to exist
# (local use only; CI sets BD_ENFORCEMENT_FORMAT_ONLY=1 to skip that).
set -u

MODE="${1:-}"
case "$MODE" in
  --staged)
    mapfile -t LINES < <(git diff --cached -U0 | sed -n 's/^+//p' | grep -v '^+++')
    ;;
  --diff)
    mapfile -t LINES < <(sed -n 's/^+//p' | grep -v '^+++')
    ;;
  *)
    mapfile -t LINES < <(cat "$@")
    ;;
esac

MARKER='(TODO|FIXME|XXX|BUG|HACK|TBD)|- \[ \]'
BEAD='Myx-[a-z0-9]+'
fail=0
for line in "${LINES[@]}"; do
    if echo "$line" | grep -qE "$MARKER"; then
        if ! echo "$line" | grep -qE "$BEAD"; then
            printf 'no bead ref: %s\n' "$line" >&2
            fail=1
        elif [ "${BD_ENFORCEMENT_FORMAT_ONLY:-0}" != "1" ] && command -v bd >/dev/null 2>&1; then
            id=$(echo "$line" | grep -oE "$BEAD" | head -1)
            if ! bd show "$id" >/dev/null 2>&1; then
                printf 'unknown bead: %s (line: %s)\n' "$id" "$line" >&2
                fail=1
            fi
        fi
    fi
done
if [ "$fail" -ne 0 ]; then
    printf 'Issue tracking is beads-only: file a bead (bd new) and reference it in the marker.\n' >&2
fi
exit "$fail"
