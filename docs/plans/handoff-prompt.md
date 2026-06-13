# Handoff prompt — implement the awsm-audio MCP improvements

Run a new session from the repo root (`/Users/dakom/Documents/AWSMFUN/REPOS/audio`).
Paste the block below after `/loop ` **with no interval** so the agent self-paces one
chunk per iteration until everything is implemented and verified. (It also works as a
plain one-shot goal/task prompt — just drop the `/loop` prefix and the "self-paced
loop / stop the loop" wording.)

---

```
/loop Implement every recommendation in docs/plans/mcp-improvements.md for this awsm-audio MCP server. This is a self-paced loop: each iteration, make real progress and stop the loop only when the entire P0–P3 set is implemented and verified.

Read docs/plans/mcp-improvements.md in full FIRST. Honor its two framing rules as hard constraints on every change:
- Keep the server STYLE- and SOURCE-AGNOSTIC: provide neutral mechanisms only. Never bake in genres, feels, swing ratios, "groove" presets, or specific sample sources/URLs — those are the agent's decisions.
- The tool is SFX/sound-design-first, not just for songs. The single-Sound / one-shot / no-arrangement path is first-class. Items tagged [music] are arrangement-only; do not let them regress or complicate the SFX workflow.

First iteration only: create docs/plans/mcp-improvements-progress.md — a checklist, one line per item (P0 #1-3, P1 #4-6, P1 #13-15, P2 #7-12, every P3 bullet), each marked TODO. Then orient yourself: how this server is built, run, and tested, and where node/param/automation handling and the docs/instructions resources live.

Every iteration:
- Pick the next TODO item(s) from the progress file (group tightly-related ones).
- Implement it properly in the server code. For items that are doc/instruction changes (#5, #6, #13, #14, #15, and the doc parts of others), updating the server's instructions text and docs/* resources IS the deliverable.
- For the silent-failure bugs (#1, #2, #3): prefer making the operation actually work; if a path truly cannot, return a clear error instead of `ok`, and document it. Never leave a silent no-op.
- Add or update tests proving the new behavior, plus a regression test for each P0 bug. Build the project and run the full test suite; do not move on until both are green.
- Mark the item Done in the progress file with a one-line note on what changed (file/commit ref). Commit on a feature branch with a descriptive message.

Guardrails: don't break existing behavior or other tools; keep changes minimal and consistent with the codebase's conventions; respect the style/source-agnostic and SFX-first constraints above. If an item is genuinely infeasible, implement the closest safe behavior and write a short rationale in the progress file rather than skipping silently.

Definition of done (stop the loop only when ALL hold): every checklist item is Done (or has a written rationale), the build passes, the full test suite passes, and docs/plans/mcp-improvements-progress.md reflects final state. When done, post a per-item summary and open a PR.
```

---

Notes:
- The **progress file** makes the loop resumable — an interrupted run resumes from
  where it left off instead of redoing work.
- It routes the **doc-only items** (#5, #13, #14, #15…) explicitly to "update the
  server instructions / `docs/*`" so they aren't skipped as "not code."
- It bakes in the doc's hard rule: **no silent no-ops** — work or error, never `ok`
  and nothing.
