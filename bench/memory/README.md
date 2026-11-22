# Memory recall eval

Measures how well an agent answers questions about earlier work with each way of looking it up, and what each costs in tokens. Runs on your own notes and chats with any command-line agent. Python 3.8+, standard library only.

## Our results

30 questions about work that has a note. One fresh Claude Sonnet 5.5 agent per question and arm, 25 tool calls max, graded blind by Claude Opus 5.5.

| Arm | Fully right (of 30) | Median input tokens |
|---|---|---|
| Notes | 25 | 138k |
| Raw transcripts | 17 | 341k |
| Past-chat search | 7 | 423k |
| Code and git | 5 | 215k |

- One person's corpus, questions written from work that has a note: this favours notes by design.
- One run per cell, n = 30, no confidence intervals.
- The questions are private, so the table can't be reproduced; the kit lets you run the same method on your own corpus.
- Our runs replaced runs that broke the access rules; this kit scores them wrong instead.

## Method

Each question goes to a fresh agent once per **arm**. Arms differ only in what the agent may use:

| Arm | Allowed |
|---|---|
| `notes` | the notes folder, and the harness's notes search if any |
| `transcripts` | raw chat transcripts in one folder |
| `past_chat` | the harness's past-chat search |
| `code_git` | source code and git history, without notes or docs |

A separate grader sees the question, the gold facts and a shuffled answer with no arm label. It marks each fact `supported`, `missing` or `contradicted`. Correct = all supported (1 point), wrong = none supported or most contradicted (0), partial otherwise (0.5).

Every tool call is recorded. A call outside the arm's rules, or past the cap, makes the answer wrong without grading. Failed runs also count as wrong. The audit reads tool calls as text, so it is not a sandbox; read a sample of `runs/*.json` yourself.

## Write questions

Start from [`example/questions.json`](example/questions.json):

```json
{"version": 1, "questions": [{
  "id": "Q01",
  "question": "Why did X stop doing Y, and what replaced it?",
  "gold_facts": ["Y hit the quota limit", "Z replaced it"],
  "sources": {"notes": ["notes/..."], "transcripts": ["chats/..."], "code": ["src/..."]}
}]}
```

- Two to four short gold facts, each checkable without context.
- `sources` is never shown to agents. Keep questions whose answer exists in more than one place, so no arm is impossible.
- Check facts against git, tickets or the chat, not only the note.

## Configure

[`example/config.json`](example/config.json); relative paths resolve against the file.

| Key | Meaning |
|---|---|
| `workdir` | where the agent runs |
| `notes_dir`, `transcripts_dir`, `code_dir` | what each arm reads; transcripts only from before `cutoff` |
| `notes_search_tool`, `chat_search_tool` | the harness's tool names; without a chat search, drop `past_chat` with `--arms` |
| `extra_forbidden_tools` | regexes of tools no arm may call |
| `cutoff` | ignore chats after this date |
| `cap` | tool calls per answer |

Keep the eval's own runs out of searchable history (`--no-session-persistence`, `--ephemeral`; OpenCode has no such flag).

## Run

From this folder:

```bash
python3 -m memeval run --config CONFIG --questions QUESTIONS --adapter adapters/claude.json --out results/claude --model sonnet --jobs 2
python3 -m memeval blind --out results/claude --seed 7
python3 -m memeval grade --out results/claude --adapter adapters/claude.json --model opus
python3 -m memeval aggregate --out results/claude   # writes report.md and report.json
```

Add `--dry-run` to print prompts without running. `adapters/stub.json` runs the whole pipeline with no model. Runs resume; only failed cells are retried.

## Adapters

| Adapter | Command | Tool restriction |
|---|---|---|
| [`claude.json`](adapters/claude.json) | `claude -p --output-format stream-json` | `--allowed-tools` per arm |
| [`codex.json`](adapters/codex.json) | `codex exec --json --ephemeral -s read-only` | audit only |
| [`opencode.json`](adapters/opencode.json) | `opencode run --format json` | audit only |

For another CLI, wrap it to print one JSON object and use `"parser": "generic"`:

```json
{"answer": "...", "input_tokens": 1234, "output_tokens": 56, "cached_input_tokens": 0,
 "tool_calls": [{"tool": "Read", "input": "{\"file_path\": \"notes/a.md\"}"}]}
```

The command gets `MEMEVAL_ARM`, `MEMEVAL_QUESTION_ID` and `MEMEVAL_CAP`. Compare token counts only within one harness.

## Tests

```bash
python3 -m unittest discover -s tests -t .
python3 tests/break_it.py   # 27 planted bugs; each must fail a test
```
