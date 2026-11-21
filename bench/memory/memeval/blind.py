"""Anonymise and shuffle answers so the grader cannot tell which arm produced which."""
import json
import os
import random
import re

from .common import load_json, save_json

LINK_TARGET = re.compile(r"\]\([^)]*\)")


def extract_answer(text):
    """Return (answer, unknowns) from the agent's JSON reply; fall back to the raw text. Sources are dropped."""
    start, end = text.find("{"), text.rfind("}")
    if start != -1 and end > start:
        try:
            data = json.loads(text[start : end + 1])
            if isinstance(data, dict) and "answer" in data:
                unknowns = data.get("unknowns") or []
                if isinstance(unknowns, str):
                    unknowns = [unknowns]
                return LINK_TARGET.sub("]", str(data["answer"])), [LINK_TARGET.sub("]", str(u)) for u in unknowns]
        except ValueError:
            pass
    return LINK_TARGET.sub("]", text), []


def gradable(record):
    return record["status"] == "ok" and not record["violations"]


def build_blind(questions, runs, seed, batch_size):
    """runs: {(question_id, arm): record}. Returns (batches, mapping); mapping stays private."""
    by_id = {q["id"]: q for q in questions}
    cells = sorted(cell for cell, record in runs.items() if gradable(record))
    random.Random(seed).shuffle(cells)
    cases, mapping = [], {}
    for number, (qid, arm) in enumerate(cells, 1):
        anon = "C%03d" % number
        answer, unknowns = extract_answer(runs[(qid, arm)]["answer"])
        cases.append(
            {
                "anonymous_id": anon,
                "question": by_id[qid]["question"],
                "gold_facts": by_id[qid]["gold_facts"],
                "answer": answer,
                "unknowns": unknowns,
            }
        )
        mapping[anon] = {"question_id": qid, "arm": arm}
    batches = [cases[i : i + batch_size] for i in range(0, len(cases), batch_size)]
    return batches, mapping


def unblind(grades_by_anon, mapping):
    """{anon: grade} -> {(question_id, arm): grade}"""
    return {(mapping[a]["question_id"], mapping[a]["arm"]): grade for a, grade in grades_by_anon.items()}


def write_blind(out_dir, questions, runs, seed, batch_size):
    batches, mapping = build_blind(questions, runs, seed, batch_size)
    blind_dir = os.path.join(out_dir, "blind")
    for old in os.listdir(blind_dir) if os.path.isdir(blind_dir) else []:
        os.remove(os.path.join(blind_dir, old))
    for number, batch in enumerate(batches, 1):
        save_json(os.path.join(blind_dir, "batch-%02d.json" % number), batch)
    save_json(os.path.join(blind_dir, "map.private.json"), {"seed": seed, "mapping": mapping})
    return len(batches), len(mapping)


def read_mapping(out_dir):
    return load_json(os.path.join(out_dir, "blind", "map.private.json"))["mapping"]
