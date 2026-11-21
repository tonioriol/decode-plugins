"""Blind grading: grader prompt, output validation, and the rubric."""
import json
import os

from .adapters import run_adapter
from .common import VERDICTS, derive_overall, load_json, save_json

GRADER_PROMPT = """Grade the historical-recall answers in this message using only the data given here. Use no tools, searches or files, and do not try to work out how any answer was produced. Each case is independent and carries an anonymous label.

For every case, judge each required fact, in its original order, as one of:
- supported: the answer (read together with its unknowns) states it, in any wording with the same meaning;
- missing: the answer does not state it, or abstains (missing is not contradicted);
- contradicted: the answer states something that conflicts with it.
Do not award a fact from your own knowledge. Extra detail is not an error just because the facts do not list it: put a statement in extra_contradictions only if the given facts establish that it is false. Give each verdict a short reason quoting or pointing at the answer.

Return only a JSON array with one object per case, in this shape, and nothing else:
[{"anonymous_id": "C001", "facts": [{"index": 1, "verdict": "supported", "reason": "..."}], "extra_contradictions": [], "overall": "correct", "reason": "..."}]
"overall" is correct (every fact supported, no extra contradiction), partial (at least one supported, not a majority contradicted, not correct) or wrong (none supported, a majority contradicted, or a complete abstention). Include every case and every fact index.

Cases:
%s
"""


def grader_prompt(batch):
    return GRADER_PROMPT % json.dumps(batch, indent=2, ensure_ascii=False)


def parse_grader_output(text):
    start, end = text.find("["), text.rfind("]")
    if start == -1 or end <= start:
        raise ValueError("no JSON array in grader output")
    data = json.loads(text[start : end + 1])
    if not isinstance(data, list):
        raise ValueError("grader output is not a list")
    return data


def validate_grades(batch, data):
    """Check ids, fact indices and verdicts; return normalised grades with the rubric-derived overall."""
    expected = {case["anonymous_id"]: len(case["gold_facts"]) for case in batch}
    seen, out = set(), []
    for item in data:
        anon = item.get("anonymous_id")
        if anon not in expected or anon in seen:
            raise ValueError("unknown or duplicate anonymous_id %r" % (anon,))
        seen.add(anon)
        facts = item.get("facts")
        if not isinstance(facts, list) or sorted(f.get("index") for f in facts) != list(range(1, expected[anon] + 1)):
            raise ValueError("%s: facts must cover indices 1..%d exactly once" % (anon, expected[anon]))
        for fact in facts:
            if fact.get("verdict") not in VERDICTS:
                raise ValueError("%s: verdict must be one of %s" % (anon, ", ".join(VERDICTS)))
        extra = item.get("extra_contradictions") or []
        facts = sorted(facts, key=lambda f: f["index"])
        out.append(
            {
                "anonymous_id": anon,
                "facts": [{"index": f["index"], "verdict": f["verdict"], "reason": str(f.get("reason", ""))} for f in facts],
                "extra_contradictions": extra,
                "grader_overall": item.get("overall"),
                "overall": derive_overall(facts, extra),
                "reason": str(item.get("reason", "")),
            }
        )
    if seen != set(expected):
        raise ValueError("missing cases: %s" % ", ".join(sorted(set(expected) - seen)))
    return out


def run_grading(out_dir, adapter, cfg, model, force):
    """Grade every blind batch with a fresh grader agent. Returns the list of batch files that failed."""
    blind_dir, grades_dir = os.path.join(out_dir, "blind"), os.path.join(out_dir, "grades")
    work = os.path.join(out_dir, "grader_cwd")
    os.makedirs(work, exist_ok=True)
    grader_cfg = dict(cfg, workdir=work)
    failed = []
    for name in sorted(n for n in os.listdir(blind_dir) if n.startswith("batch-")):
        target = os.path.join(grades_dir, name)
        if os.path.exists(target) and not force:
            continue
        batch = load_json(os.path.join(blind_dir, name))
        record, stdout, stderr = run_adapter(adapter, "grader", grader_prompt(batch), grader_cfg, model, {"MEMEVAL_ARM": "grader"})
        try:
            if record["status"] != "ok":
                raise ValueError("grader run %s: %s" % (record["status"], record["error"]))
            grades = validate_grades(batch, parse_grader_output(record["answer"]))
        except (ValueError, TypeError, AttributeError, KeyError) as exc:
            os.makedirs(grades_dir, exist_ok=True)
            with open(os.path.join(grades_dir, name + ".error.txt"), "w", encoding="utf-8") as handle:
                handle.write("%s\n\n--- answer ---\n%s\n\n--- stderr ---\n%s\n" % (exc, record["answer"], stderr))
            failed.append("%s: %s" % (name, exc))
            continue
        save_json(
            target,
            {
                "grader_model": model or "",
                "grader_tool_calls": len(record["tool_calls"]),
                "grades": grades,
            },
        )
    return failed


def read_grades(out_dir):
    """All graded cases of an output dir as {anonymous_id: grade}."""
    grades_dir = os.path.join(out_dir, "grades")
    found = {}
    for name in sorted(os.listdir(grades_dir)) if os.path.isdir(grades_dir) else []:
        if name.startswith("batch-") and name.endswith(".json"):
            for grade in load_json(os.path.join(grades_dir, name))["grades"]:
                found[grade["anonymous_id"]] = grade
    return found
