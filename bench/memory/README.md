# Memory recall eval

A small kit to measure one thing: **when an agent has to answer a question about earlier work, how well does it do with each way of looking the answer up, and what does that cost?** It runs on your own notes and chat history, with any command-line agent, so you can check our numbers' method and get your own.

Python 3.8+ and the standard library only. Nothing here is specific to Decode.

## What is measured

Each question is asked to a **fresh agent, once per arm**. An arm is a tool-access policy:

| Arm | The agent may use only |
|---|---|
| `notes` | your notes folder (optionally the harness's notes search tool, then direct reads) |
| `transcripts` | raw chat transcript files in one folder (grep and read) |
| `past_chat` | the harness's own past-chat search tool |
| `code_git` | source code and git history, with notes and docs excluded |

Same question text, same tool-call cap (default 25), same answer format for every arm. Only the access rules differ.

Answers are graded **blind** by a separate agent against **gold facts** you wrote with the question. The grader sees the question, the gold facts and a shuffled, anonymised answer. It never sees the arm, token counts or sources.

- Each gold fact is `supported`, `missing` or `contradicted`.
- Correct = every fact supported and no extra contradiction. Wrong = no fact supported, or most contradicted (an abstention is wrong). Partial = anything else.
- Score: correct 1, partial 0.5, wrong 0. The kit derives the label from the per-fact verdicts itself and ignores the grader's own label (it counts disagreements).
- Every tool call is recorded. A call that breaks the arm's rules (another arm's folder, git outside `code_git`, a web, edit or delegation tool, more calls than the cap) is a **violation**, and a violating answer is **scored wrong** and never sent to the grader. Failed and missing runs also count as wrong, and are shown separately. Nothing is dropped from the denominator.
- Answered cells are never re-rolled. Only runs that failed to produce an answer (error, timeout) are re-run on resume, and the failed attempts are kept.

Output per arm: fully right, partial, wrong, violations, failed, mean score, median input tokens, median tool calls, as a Markdown table.

## Write your questions

Copy [`example/questions.json`](example/questions.json) (three invented questions).

```json
{"version": 1, "questions": [{
  "id": "Q01",
  "question": "Why did X stop doing Y, and what replaced it?",
  "gold_facts": ["Y hit the quota limit", "Z replaced it"],
  "sources": {"notes": ["notes/..."], "transcripts": ["chats/..."], "code": ["src/..."]}
}]}
```

- `gold_facts`: short, atomic and checkable on their own, with exact names, numbers and reasons. Two to four per question. The grader sees only these, so write them to be judged without context.
- `sources` is for you: it records where each fact can be verified. The runner never shows it to an agent. Keep only questions that have **independent evidence** in more than one place (note and chat, and ideally code or git), so no arm is impossible by construction.
- Check facts against the original evidence (git, tickets, the chat), not only against the note: notes can be wrong. A question whose note is wrong is useful, keep it.
- Author the questions from work that has a note, as we did, and say so: results then apply to documented work, not to arbitrary forgotten facts. Do not grade your own answers.
- Use a grader at least as strong as the answering model.

## Set up

Everything the agents may touch is described in a config ([`example/config.json`](example/config.json)). Relative paths resolve against the config file.

| Key | Meaning |
|---|---|
| `workdir` | Folder the agent runs in (usually the project root) |
| `notes_dir` | Notes folder (`notes` arm) |
| `transcripts_dir` | Folder of raw transcripts, **only chats older than the cutoff** (`transcripts` arm). Export or symlink them into one folder |
| `code_dir` | Project for the `code_git` arm |
| `notes_search_tool`, `chat_search_tool` | Tool names the harness exposes for notes search and past-chat search. Leave empty if none. Without `chat_search_tool` the `past_chat` arm has nothing to use: drop it with `--arms` |
| `extra_forbidden_tools` | Regexes of tool names other arms must not call (for example a code-search tool) |
| `cutoff` | Date after which chats must be ignored, because they may discuss the eval itself |
| `cap` | Tool-call cap per answer |

Make sure the eval's own runs do not become searchable history: use the no-persistence flags in the adapters (`--no-session-persistence`, `--ephemeral`). OpenCode has no such flag, so its runs land in its session store; keep them out of the `past_chat` arm's reach.

## Run

All commands run from this folder. `--out` is one results folder per harness.

```bash
# 0. Print every command and prompt without running anything
python3 -m memeval run --config CONFIG --questions QUESTIONS --adapter adapters/claude.json --out results/claude --dry-run

# 1. One fresh agent per question x arm (resume-safe; --only Q01,Q02 and --arms notes,code_git narrow it)
python3 -m memeval run --config CONFIG --questions QUESTIONS --adapter adapters/claude.json --out results/claude --model sonnet --jobs 2

# 2. Shuffle and anonymise the answers; the arm labels go to map.private.json
python3 -m memeval blind --out results/claude --seed 7

# 3. Grade each blind batch with a fresh grader agent (any adapter, ideally a stronger model)
python3 -m memeval grade --out results/claude --adapter adapters/claude.json --model opus

# 4. Join grades and runs: results/claude/report.md and report.json
python3 -m memeval aggregate --out results/claude
```

Try the whole pipeline with no model first: `python3 -m memeval run --config example/config.json --questions example/questions.json --adapter adapters/stub.json --out results/stub`, then steps 2 to 4 with the same adapter.

`results/<harness>/runs/` holds one JSON per cell (answer, tokens, every tool call, violations, duration, the exact prompt); `raw/` holds the harness's raw output for audits.

## Adapters: any harness

An adapter is a JSON file ([`adapters/`](adapters)): a command, optional `model_args`, per-arm extra arguments (`arm_args`, used to pass allowed-tools flags and `grader` for a no-tools grader), how the prompt is passed (`prompt_via`: `stdin` or `arg`), and a `parser`.

| Adapter | Command | Tool restriction |
|---|---|---|
| [`claude.json`](adapters/claude.json) | `claude -p --output-format stream-json --verbose` | `--allowed-tools` per arm with `dontAsk`, grader gets `--tools ""` |
| [`codex.json`](adapters/codex.json) | `codex exec --json --ephemeral -s read-only` | none by flag (sandbox is read-only); audit only |
| [`opencode.json`](adapters/opencode.json) | `opencode run --format json` | none by flag; audit only |
| [`stub.json`](adapters/stub.json) | canned answers, for the smoke test | |

For any other CLI, write a small wrapper that runs one prompt and prints a single JSON object, and use `"parser": "generic"`:

```json
{"answer": "...", "input_tokens": 1234, "output_tokens": 56, "cached_input_tokens": 0,
 "tool_calls": [{"tool": "Read", "input": "{\"file_path\": \"notes/a.md\"}"}]}
```

The runner exports `MEMEVAL_ARM` (`notes`, `transcripts`, `past_chat`, `code_git` or `grader`), `MEMEVAL_QUESTION_ID` and `MEMEVAL_CAP` to the command. `input_tokens` is everything the harness reports as input over all model requests of the run, cached tokens included. Cache accounting differs between harnesses, so compare token counts only within one harness.

### Adapter status (checked on one machine: Claude Code 2.1.325, Codex CLI 0.169.0, OpenCode 1.18.34)

- **Codex**: ran end to end for real (one `notes` answer, then graded by Codex itself). Answer, tool calls and token usage parsed.
- **OpenCode**: ran end to end for real with a free model (`--model opencode/big-pickle`). Answer, tool calls and token usage parsed. With its default model, one run returned an API error, which the runner recorded as a failed run.
- **Claude Code**: every flag in the adapter is accepted by the installed CLI and the request reached the API, but that account was at its usage limit, so no successful response was observed. The parser is tested on fixtures that follow the observed `system` and `result` events and the `tool_use` block shape of Claude's own session logs, not on a live answer.
- A user's own allow rules still apply on top of `--allowed-tools`, and the policy audit below is what catches strays.

## How the policy audit works, and what it cannot do

It reads the recorded tool calls as text. It flags another arm's folder (absolute path, or `folder/` relative to `workdir`), `git` commands outside `code_git`, edit, web and delegation tools, the other arms' search tools, local file or shell tools in `past_chat`, and calls beyond the cap. Exclusions (`:(exclude)...`, `--exclude-dir`, `-g '!...'`) do not count as access.

It is not a sandbox. It can false-positive (a source folder that happens to be called like your notes folder) and miss a read that never names the folder, and it cannot tell whether a chat was newer than the cutoff. Read a sample of `runs/*.json` yourself. Blinding hides the arm, sources and link targets, but an answer's style can still hint at where it came from.

## Reading results

- The table's counts are over every planned question. Check Violations and Failed first: an arm with many is partly measuring prompt-following.
- Compare arms question by question in `report.json` (`cells`), not only by totals. With 30 questions, a gap of a few questions is within what a second run could change. Run twice and with another model before drawing conclusions.
- Accuracy and cost are separate outcomes. The question is usually what an arm costs per correct answer, not which arm wins.

## Tests

```bash
python3 -m unittest discover -s tests -t .   # blind mapping round trip, rubric, aggregation, policy audit, output parsers
python3 tests/break_it.py                    # applies 27 deliberate bugs one at a time and requires the tests to fail
```

## Our published results

30 questions about earlier work that has a note, one fresh agent per question and arm, 25 tool-call cap, answers by Claude Sonnet 5.5, blind grading by Claude Opus 5.5 against gold facts.

| Arm | Fully right (of 30) | Median input tokens |
|---|---|---|
| Notes | 25 | 138k |
| Raw transcripts | 17 | 341k |
| Past-chat search | 7 | 423k |
| Code and git | 5 | 215k |

Caveats:

- One owner's corpus, with questions written from work that has a note. This favours the notes arm by construction; it says nothing about undocumented work.
- One answering model, one grader model, one run per cell, n = 30. No confidence intervals; small gaps are noise.
- Our private runs handled access violations by excluding and replacing the run and reporting it. This kit scores a violation as wrong, which is stricter, so a kit run can differ from our table for that reason alone.
- Our arms used Decode's own tools (its notes search, its chat search, its transcript store). The prompts here are a generalisation of ours, not the same text.
- Token counts are harness-reported input tokens summed over a run. They are not dollars and not subscription quota.
- The questions and answers are private and are not published, so the table cannot be reproduced from this repository. The kit lets you check the method and produce your own numbers on your own corpus.
