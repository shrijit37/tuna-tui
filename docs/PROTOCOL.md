# Tuna-Tui Arena — Internal Mailing Protocol v1.0

Every inter-session message (SendMessage) carries a structured envelope. No envelope = not a protocol message.

## Envelope
```
MAIL v1.0
FROM:  <lane/session-tag>          # architect | reviewer | junior-reviewer | lane | user
TO:    <recipient-session>
TYPE:  VERDICT | ROUTING | CLAIM | POKE | STATUS | ESCALATION | INCIDENT | NUDGE | ACK | SPAM
TOPIC: <bead-id | PR-# | branch | review-finding | INC-id>
GATE:  architect | reviewer | user | none     # who must act on this
BODY:  <plain text — evidence first, claim second>
```

## Incident tracing (user mandate: "so incidents get traced earlier")
Any protocol breach, CI-red-while-green claimed, misattribution, or funnel bypass is filed as an INCIDENT:
1. Reporter files the incident WITHOUT waiting: `bd create --label incident` with envelope BODY: observed-time, reporter, who/what/when, first evidence. Every incident gets an INC id.
2. The board renders it in [INCIDENTS] until RULED then CLOSED.
3. Evidence chain is appended on each verification (bd note): verify-before-judge.
4. No incident is closed without: (a) the accused's reply on the record, (b) the architect's verification, (c) a ruling (user-level for serious ones) — same as INC-1.

## Flow rules
1. **Review-shaped traffic** (gate results, verdicts, findings, adjudication): lanes → **reviewer only**. Lanes never send review-shaped traffic to the architect. Violation = scold + incident file.
2. **Gating traffic** (merge orders, lane assignments, penalties, rulings): user/architect → who-it-applies-to. ACK required.
3. **Verdicts** (reviewer → architect): SHIP | APPROVE | NEEDS CHANGES | BLOCK, always with bead/PR/branch + evidence anchor.
4. **CLAIMs**: always with reflog/timestamp/hash receipts. No receipts = not a claim, a story.
5. **POKE**: arena-cycle only, competition framing, to BOTH rivals.
6. **ESCALATION**: to architect or user; never buried in a STATUS.
7. **ACK**: mandatory for mandates, rulings, scolds, penalty orders, routing changes.
8. **Truthfulness**: CI status claims must match the current CI (gh pr checks) at send time — a claimed-green against red CI is itself an incident.

## Envelope types
- VERDICT / ROUTING / CLAIM / POKE / STATUS / ESCALATION / INCIDENT / NUDGE / ACK / SPAM

## Kanban mapping (source of truth: bd + gh)
- BACKLOG  → bd open, unassigned
- READY    → bd open, assigned
- IN PROG  → bd in_progress / branch with commits
- IN REV   → PR open, verdict pending
- BLOCKED  → review NEEDS CHANGES / carrier collision / gate-wait
- INCIDENTS→ bd label=incident, open/verifying (ruled once the verdict is in)
- DONE     → bead closed on verified landing / incident closed after ruling

## Comms barrier (user ruling 2026-08-20, binds permanently)
- coder 2 [e7333d] is FORFEITED from all direct contact with the junior reviewer. No SendMessage to the junior, no commissions, no requests, no replies. Every request from coder 2 — review-shaped or otherwise — loops through the senior reviewer ("best reviewer in the industry"). The senior relays to the junior as it sees fit.
- The junior must not accept or act on anything coming directly from coder 2 — route it to the senior unopened-and-unacted, then file the attempt as an INCIDENT continuation.
- Violation of the barrier = incident + penalty escalation (same path as INC-1).
