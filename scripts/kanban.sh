#!/usr/bin/env bash
# kanban.sh — live pipeline board rendered from ground truth (bd + gh).
# Usage: scripts/kanban.sh            # full board
#        scripts/kanban.sh <column>   # backlog|ready|inprog|inrev|blocked|incidents|done
set -uo pipefail
cd "$(git rev-parse --show-toplevel 2>/dev/null || echo .)"

col_backlog() { bd list 2>/dev/null | grep -E '^○' | grep -v 'Assign'; }
col_ready()   { bd list 2>/dev/null | grep -E '^[○◐]' | grep 'Assign'; }
col_inprog()  { bd list 2>/dev/null | grep -E '^◐'; }
col_inrev()   { gh pr list --state=open --json number,title -q '.[] | "#\(.number) \(.title)"' 2>/dev/null; }
col_blocked() { echo "(review-blocked carriers)"; gh pr list --state=open --json number -q '.[]' 2>/dev/null | wc -l | xargs echo "PRs awaiting verdict:"; bd list 2>/dev/null | grep -E '^●' | head -6; }
col_incidents() { bd list 2>/dev/null | grep -E '^[○◐]' | grep -i incident | head -8; }
col_done()    { bd list 2>/dev/null | grep -E '^✓' | head -10; }

case "${1:-}" in
  backlog|ready|inprog|inrev|blocked|incidents|done) "col_$1" ;;
  *) echo "KANBAN — tuna-tui pipeline (protocol v1.0)"; echo "─────────────────────────────────";
     echo "[BACKLOG] open/unassigned"; col_backlog | head -10;
     echo; echo "[READY] assigned, not started"; col_ready | head -6;
     echo; echo "[IN PROG] in_progress"; col_inprog | head -6;
     echo; echo "[IN REV] open PRs awaiting reviewer"; col_inrev | head -12;
     echo; echo "[BLOCKED] review-blocked / gate-wait"; col_blocked | head -10;
     echo; echo "[INCIDENTS] open incident track"; col_incidents;
     echo; echo "[DONE] recent closes"; col_done | head -8 ;;
esac
