#!/usr/bin/env bash
# bd-enforce.sh — scripted enforcement of the beads issue-tracking policy.
# Every code change must be tracked by a bead; debt must be attributed.
#
# Usage:
#   scripts/bd-enforce.sh status            # open-bead count + drift warning
#   scripts/bd-enforce.sh check-commit <msg># 1 if <msg> carries a bead ref or is exempt
#   scripts/bd-enforce.sh audit             # scan src/ for un-attributed debt markers (Myx-enforce)
#   scripts/bd-enforce.sh install           # wire as .git/hooks/pre-commit
#   scripts/bd-enforce.sh hook <commit-msg-file>  # pre-commit entry point

set -uo pipefail
cd "$(git rev-parse --show-toplevel 2>/dev/null || echo .)"

BEAD_RE='Myx-[A-Za-z0-9._-]+'
# Types that may land without a bead ref (they are not tracked work).
EXEMPT_RE='^(Merge |Revert |(release|chore|docs|ci|build|refactor)(\([^)]*\))?:)'

count_open() { bd list 2>/dev/null | grep -cE '^[○◐●]'; }

cmd_status() {
  local n
  n=$(count_open)
  echo "beads: $n open"
  if [ "$n" -gt 5 ]; then
    echo "WARN: backlog >5 — pick from: bd ready"
  elif [ "$n" -eq 0 ]; then
    echo "backlog EMPTY — run the adversarial audit loop before stopping"
  fi
}

check_commit() {
  local msg="$1"
  if grep -qE "$BEAD_RE" <<<"$msg"; then return 0; fi
  if grep -qE "$EXEMPT_RE" <<<"$msg"; then return 0; fi
  return 1
}

cmd_audit() {
  # Bare markers without a bead ref on the same line (or 2 lines below a trailing ref).
  local hits=0
  while IFS=: read -r f l; do
    [ -f "$f" ] || continue
    if ! awk -v ln="$l" 'NR==ln || NR==ln+1 || NR==ln+2' "$f" | grep -qE "$BEAD_RE"; then
      echo "$f:$l: unattributed marker"
      hits=$((hits+1))
    fi
  done < <(grep -rnE '\b(TO''DO|FIX''ME|HA''CK|X''XX)\b' src/ 2>/dev/null | cut -d: -f1,2)
  echo "audit: $hits unattributed marker(s)"
  return 0
}

cmd_hook() {
  # Git runs commit-msg hooks AFTER the message is prepared and passes the
  # message file as $1 — reading COMMIT_EDITMSG here would see the previous
  # commit's message (pre-commit runs before the message is written).
  local msgfile="${1:-}"
  [ -f "$msgfile" ] || { echo "bd-enforce: no commit message"; exit 1; }
  local msg; msg=$(head -1 "$msgfile")
  if ! check_commit "$msg"; then
    echo "bd-enforce: commit rejected — no bead reference (Myx-...) in: '$msg'"
    echo "  tracked work needs a bead: use bd create, then cite it."
    echo "  exempt: merge/revert, release:, chore:, docs:, ci:, build:, refactor:"
    exit 1
  fi
  exit 0
}

cmd_install() {
  local hook=.git/hooks/commit-msg
  cat > "$hook" <<'HOOK'
#!/usr/bin/env bash
exec scripts/bd-enforce.sh hook "$1"
HOOK
  chmod +x "$hook"
  echo "bd-enforce: commit-msg hook installed at $hook"
}

case "${1:-}" in
  status)        cmd_status ;;
  check-commit)  check_commit "${2:-}"; echo $? ;;
  audit)         cmd_audit ;;
  install)       cmd_install ;;
  hook)          cmd_hook "${2:-}" ;;
  *) sed -n '2,9p' "$0" ;;
esac
