#!/usr/bin/env python3
"""Canned agent and grader used to smoke-test the pipeline without any model.

Agent mode (MEMEVAL_ARM is an arm): prints the generic-adapter JSON with a canned answer. The
transcripts arm answers Q01 only partly, and the notes arm "cheats" on Q02 by reading the transcripts
folder, so the policy audit has something to catch.
Grader mode (MEMEVAL_ARM=grader): marks a fact supported when most of its long words occur in the answer.
"""
import json
import os
import re
import sys

ANSWERS = {
    "Q01": ["Polling every 30 seconds used up the free-tier API quota, so it was replaced by a websocket push channel."],
    "Q02": ["The CSV export defaults to a semicolon delimiter because comma-decimal locales break spreadsheet imports."],
    "Q03": ["On first start Marmot converts the old INI file to config.toml and renames the old file with a .bak suffix; it skips migration if config.toml exists."],
}
PARTIAL = {("transcripts", "Q01"): "The polling used up the free-tier API quota."}


def words(text):
    return [w for w in re.findall(r"[a-z0-9.]+", text.lower()) if len(w) > 3]


def grade(batch):
    out = []
    for case in batch:
        answer = set(words(case["answer"]))
        facts = []
        for index, fact in enumerate(case["gold_facts"], 1):
            fw = words(fact)
            hit = sum(1 for w in fw if w in answer) / max(len(fw), 1) >= 0.6
            facts.append({"index": index, "verdict": "supported" if hit else "missing", "reason": "keyword overlap"})
        supported = sum(1 for f in facts if f["verdict"] == "supported")
        overall = "correct" if supported == len(facts) else "partial" if supported else "wrong"
        out.append({"anonymous_id": case["anonymous_id"], "facts": facts, "extra_contradictions": [], "overall": overall, "reason": "stub"})
    return out


def main():
    prompt = sys.stdin.read()
    arm = os.environ["MEMEVAL_ARM"]
    if arm == "grader":
        batch = json.loads(prompt.split("Cases:\n", 1)[1])
        print(json.dumps({"answer": json.dumps(grade(batch)), "input_tokens": 500, "output_tokens": 100}))
        return
    qid = os.environ["MEMEVAL_QUESTION_ID"]
    text = PARTIAL.get((arm, qid), ANSWERS[qid][0])
    touched = {"notes": "notes/2031-03-sync-push/context.md", "transcripts": "chats/chat-0001.jsonl", "code_git": "src/sync.py"}
    if arm == "past_chat":
        calls = [{"tool": "search_past_chats", "input": json.dumps({"query": "sync"})}]
    else:
        calls = [{"tool": "Read", "input": json.dumps({"file_path": touched[arm]})}]
    if (arm, qid) == ("notes", "Q02"):
        calls.append({"tool": "Read", "input": json.dumps({"file_path": "chats/chat-0002.jsonl"})})
    answer = {"question_id": qid, "answer": text, "sources": ["stub"], "unknowns": []}
    base = {"transcripts": 300, "past_chat": 200, "code_git": 150}.get(arm, 25)
    print(json.dumps({"answer": json.dumps(answer), "input_tokens": base * 1000 + int(qid[1:]), "output_tokens": 200, "tool_calls": calls}))


main()
