---
name: memory
description: Query, create, and update durable feature memory in memory/. Use for recalling prior work, creating context, flushing verified progress, or saving a restartable continuation state, or when the user says "flush", "flush memory", or "save progress".
---

# Memory
Manage long-term feature memory under the root workspace's `memory/`.

**Memory is curated current truth:** the smallest complete account that lets a
future agent trust the state and continue safely without the conversation.
If deleting the conversation would lose actionable detail, the flush is
incomplete.

**Never fabricate.** IDs, paths, commands, outputs, measurements, dates, and
hashes come from the record. Mark a missing fact `<!-- not captured -->`
rather than guessing; a wrong fact in memory is worse than a flagged gap.

## Find the memory
Search `memory/` before creating anything; directories are timestamped, so
listing gives recency. Update the matching feature when one exists. Store
memory in the root workspace, not a reclone or worktree.

The memory plugin's `search` MCP tool searches the current project's `memory/`
and, ranked lower, `docs/`, by meaning. Additional projects are included only
when scan roots are configured. Pass `workspace` so the current project ranks
first, or `project` to search only one configured project.

- Ticketless: `<timestamp>-<slug>/`
- Ticket-backed: `<timestamp>-<ticket-id-lowercase>-<slug>/` — secondary
  tickets go in `related`, never in the directory name
- Slug source: branch feature name, then user argument, then inference

## Documents
- `context.md` — current task state and durable outcomes
- `spec.md` — approved behavior
- `plan.md` — executable checklist (`- [ ]` / `- [x]` / `- [-]`) and cursor

Design belongs in the spec. Execution order belongs in the plan. Context links
to them instead of copying them.

Flat layout is the default. For explicitly approved multi-session phases, root
documents map the feature and the active phase may own its context, spec, and
plan. Existing memory keeps its layout unless the user approves a boundary.
Write phase detail to the active phase; update root state for phase transitions,
cross-phase decisions, and the global continuation point.

## Context shape
Frontmatter plus exactly five body sections:

```markdown
---
title: "Human-readable task title"
id: TASK-123              # ticket-backed work only; not `linear`
status: active           # active | paused | done
repos: [repo-name]       # nested git repos involved; omit if none
tags: [tag1, tag2]       # alphabetical; reuse existing tags across memory/
related: [slug-of-other] # real directory names; omit if none
created: YYYY-MM-DD
---

# [title]

## TASK
**Goal:** one sentence.
**Done when:** concrete completion signal.

## SPEC
[spec.md](./spec.md) — one-line summary of the approved behavior

## FILES
- workspace/relative/path — currently relevant code, tests, documentation

## PLAN
**Plan:** [plan.md](./plan.md)
**Cursor:** where work resumes
**Status:** active
**Traps:** what still costs time and how to get past it; one already fixed in code or a skill belongs in LOG (omit if none)
**Open:** questions only the user can answer, not guesses (omit if none)

## LOG

### YYYY-MM-DD HH:MM — Milestone heading
- Why: problem, symptom, or motivation
- How: exact commands, queries, code refs (`file:line`), including failed
  attempts and corrections
- Key info: evidence (counts, timings, errors — not "it worked"), decisions
  with rejected alternatives, verification output, commit hash
```

No other frontmatter fields. `status` uses only `active` / `paused` / `done`,
in both frontmatter and PLAN; set `done` when the work itself is complete —
rollout handoffs and follow-up tickets do not block it. A merge, deploy or
ticket close is that moment: set `done` in the same flush that records it.

Traps and Open are current state, rewritten on each flush: drop a trap once
the fix lives in a skill or the code, and a question once answered (the answer
becomes a LOG decision).

`related` links both ways: when you add another folder to `related`, add this
folder to that folder's `related` too.

Ticket-backed TASK quotes the ticket's acceptance criteria and out-of-scope
items verbatim under the Goal/Done lines; a paraphrase loses the exact
wording a later reviewer will check against.

## LOG entries
One entry per milestone, chronological — never one entry summarizing a whole
session. Insert each entry at its timestamp's position, never simply at the
end when a parallel agent or late flush logs an earlier milestone. A milestone is a state change: reproduced, diagnosed, decided,
implemented, verified, handed off. Commits, pushes, installs and reloads are
not milestones: cite their hashes in the entry for the result they ship.
Headings name the outcome ("Cold open 1.0→0.7 s"), not the last action
("Round 2 installed"). Keep entries to 3–5 bullets;
depth lives in the number of entries, not their length. Add **Decision**,
**Evidence**, **Verification**, or **Commit** bullets when the milestone has
them.

Each entry stands alone without the conversation and says why and how, not
just "modified X". Label conclusions: **implemented**, **partial**,
**investigatory**, or **rejected**. "Read file X" or "ran Y" without a durable
conclusion is not an entry.

## Curate
Preserve verified conclusions with enough evidence to trust or reproduce them:
measurements, decisive commands, source locations, tests, commits, accepted
decisions, current risks, and the exact continuation point.

Before writing, reconcile the latest related work unit:

- refine wording when evidence sharpens the same conclusion;
- replace claims superseded by stronger evidence;
- consolidate entries that now describe one outcome;
- append when the implementation or accepted decision genuinely changed over
  time.

Earlier entries are history: correct them by appending, not rewriting. But a
correction must not leave the file contradicting itself. When a new entry
refutes an earlier claim, append the correction **and** mark the old line
in place (`~~claim~~ → superseded YYYY-MM-DD: see <heading>`), and update any
TASK/FILES/PLAN text that repeated it. A reader who stops at the first claim
must not walk away believing it. Git history preserves prior versions. Ask
before cleanup beyond the latest related work unit.

Investigation may remain in the conversation until it yields a durable result.
When an unsuccessful approach leaves an important constraint, preserve the
constraint and its evidence.

Memory is task history, never the only home of reusable knowledge. When a
flush settles a quirk, contract or rule another task would need, write it to
its proper home first (the relevant skill, or AGENTS.md for cross-cutting
rules) and let the LOG entry cite that commit.

## Operate
### Query
Read matching memory first and answer from its verified current state. Resolve
conflicting claims against evidence before relying on them.

### Create
Create memory for substantial work needing cross-session continuity. Record the
goal, completion condition, approved design and plan when available, relevant
files, verified state, and continuation point. With no context but substantial
work already done, reconstruct by walking the conversation chronologically —
one LOG entry per milestone, with timestamps.

### Update
1. Read the relevant context and linked spec and plan.
2. Inspect relevant Git state, tests, and observed output.
3. Walk the conversation for every milestone since the last flush; if a claim
   has no command, query, or output behind it, gather that first.
4. Reconcile related memory, then append the new entries.
5. Refresh task state, files, plan cursor, and status where truth changed.
6. Check for contradictions, unsupported claims, and stale continuation points;
   in particular, a claim of success must cite the run that succeeded, not
   one that failed; verify the directory's ticket component matches `id`
   case-insensitively.
7. Commit the memory repository separately when workspace rules require it.

## Cadence
Flush when a durable result lands: a diagnosis, a verification verdict, a
user-accepted decision, a task or phase boundary, imminent context loss, or an
explicit user request. A commit, push, install or reload alone is not a reason
to flush or an entry.

One flush per settled work unit: fold everything settled since the last flush
into a single coherent update rather than replaying conversation chronology.
A flush that changes task state also moves the `context.md` PLAN cursor.
Subagents don't flush: they report what settled and the parent folds it into
memory. A subagent writes memory only when its brief assigns it a specific
update. Independent agents started with `root` own their task and flush.

## Hooks
The plugin's `hooks/` (beside `skills/`) holds lifecycle hooks registered in
its `hooks/hooks.json`; they run only in top-level sessions of workspaces
with `memory/`.

- `session-start-index.sh` (SessionStart): the flush rule plus up to 10 open
  (`active`/`paused`) features, most recently committed first, as directory slugs only
  (no timestamp prefix or filenames). Set `active` when you resume a feature and `paused`/`done` when its
  work stops, so this list stays accurate.
- `flush-hooks.sh` (Stop, PreCompact): work is a successful `git commit`, a
  saved edit outside `memory/` and `docs/`, or a completed subagent result in
  the transcript. It counts as flushed when git shows a commit touching a
  `memory/<folder>` this chat named, at or after the latest work. Absolute
  feature paths and workspace-relative paths retain their repository; relative
  command paths resolve against the tool's `cwd`. It works whether `memory/` is
  its own repository, part of the project's, or in a nested repository. Stop
  reminds once per item; PreCompact postpones the condense and hands the
  worker the flush instruction, and the condense runs by itself at the next
  request once that commit exists. It cannot see a user's "don't flush"; report that rather
  than retrying around it.
  `hooks/test-hooks.sh` runs the fixture tests.

## Finish
Report which documents changed, whether memory was added or reconciled, the
memory commit, and the continuation point or completion state.

**Final test:** a future agent can continue safely without reading the original
conversation or resolving known contradictions.
