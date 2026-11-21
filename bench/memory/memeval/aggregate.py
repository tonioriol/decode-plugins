"""Per-arm results: fully right, partial, wrong, mean score, median input tokens."""
import statistics

from .common import ARMS, SCORES


def cell_outcome(record, grade):
    """One cell's outcome. Missing, failed and policy-violating runs count as wrong, never dropped."""
    if record is None:
        return "missing", "wrong"
    if record["status"] != "ok":
        return "failed", "wrong"
    if record["violations"]:
        return "violation", "wrong"
    if grade is None:
        raise ValueError("no grade for %s/%s: run the grade step" % (record["question_id"], record["arm"]))
    return "graded", grade["overall"]


def aggregate(question_ids, arms, runs, grades):
    """runs: {(qid, arm): record}; grades: {(qid, arm): grade}. Returns the report dict."""
    cells, per_arm = [], {}
    for arm in arms:
        rows = []
        for qid in question_ids:
            record = runs.get((qid, arm))
            kind, overall = cell_outcome(record, grades.get((qid, arm)))
            rows.append((kind, overall, record))
            cells.append(
                {
                    "question_id": qid,
                    "arm": arm,
                    "kind": kind,
                    "overall": overall,
                    "score": SCORES[overall],
                    "input_tokens": record["input_tokens"] if record and record["status"] == "ok" else None,
                    "tool_calls": len(record["tool_calls"]) if record else None,
                    "violations": record["violations"] if record else [],
                }
            )
        scores = [SCORES[o] for _, o, _ in rows]
        valid = [SCORES[o] for k, o, _ in rows if k == "graded"]
        tokens = [r["input_tokens"] for k, _, r in rows if r and r["status"] == "ok"]
        calls = [len(r["tool_calls"]) for _, _, r in rows if r and r["status"] == "ok"]
        per_arm[arm] = {
            "n": len(rows),
            "correct": sum(1 for _, o, _ in rows if o == "correct"),
            "partial": sum(1 for _, o, _ in rows if o == "partial"),
            "wrong": sum(1 for _, o, _ in rows if o == "wrong"),
            "violations": sum(1 for k, _, _ in rows if k == "violation"),
            "failed_or_missing": sum(1 for k, _, _ in rows if k in ("failed", "missing")),
            "mean_score": sum(scores) / len(scores) if scores else None,
            "n_graded": len(valid),
            "mean_score_graded_only": sum(valid) / len(valid) if valid else None,
            "median_input_tokens": statistics.median(tokens) if tokens else None,
            "median_tool_calls": statistics.median(calls) if calls else None,
        }
    disagreements = sum(1 for g in grades.values() if g.get("grader_overall") != g["overall"])
    return {"arms": per_arm, "cells": cells, "grader_label_disagreements": disagreements}


def _tokens(value):
    if value is None:
        return "-"
    return "%dk" % round(value / 1000) if value >= 1000 else str(int(value))


def _num(value, digits=2):
    return "-" if value is None else ("%." + str(digits) + "f") % value


def markdown(report, arms):
    lines = [
        "| Arm | Fully right | Partial | Wrong | Violations | Failed | Mean score | Median input tokens | Median tool calls |",
        "|---|---|---|---|---|---|---|---|---|",
    ]
    for arm in arms:
        a = report["arms"][arm]
        lines.append(
            "| %s | %d / %d | %d | %d | %d | %d | %s | %s | %s |"
            % (
                arm, a["correct"], a["n"], a["partial"], a["wrong"], a["violations"], a["failed_or_missing"],
                _num(a["mean_score"]), _tokens(a["median_input_tokens"]), _num(a["median_tool_calls"], 1),
            )
        )
    lines += [
        "",
        "Violating, failed and missing runs are scored wrong (they are inside the Wrong column).",
        "Mean score over graded runs only: "
        + ", ".join("%s %s (n=%d)" % (arm, _num(report["arms"][arm]["mean_score_graded_only"]), report["arms"][arm]["n_graded"]) for arm in arms)
        + ".",
        "Grader labels that differed from the rubric applied to its fact verdicts: %d (the rubric result is used)."
        % report["grader_label_disagreements"],
    ]
    return "\n".join(lines) + "\n"
