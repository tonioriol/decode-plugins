"""Command line: run, blind, grade, aggregate. See README.md."""
import argparse
import os
import shutil
import sys
from concurrent.futures import ThreadPoolExecutor

from .adapters import build_argv, load_adapter, run_adapter
from .aggregate import aggregate, markdown
from .blind import read_mapping, unblind, write_blind
from .common import ARMS, build_prompt, load_config, load_json, load_questions, save_json
from .grading import read_grades, run_grading
from .policy import violations


def _run_path(out, qid, arm):
    return os.path.join(out, "runs", "%s__%s.json" % (qid, arm))


def read_runs(out):
    runs = {}
    runs_dir = os.path.join(out, "runs")
    for name in sorted(os.listdir(runs_dir)) if os.path.isdir(runs_dir) else []:
        if name.endswith(".json"):
            record = load_json(os.path.join(runs_dir, name))
            runs[(record["question_id"], record["arm"])] = record
    return runs


def _execute(job):
    out, question, arm, adapter, cfg, model = job
    prompt = build_prompt(question, arm, cfg)
    env = {"MEMEVAL_ARM": arm, "MEMEVAL_QUESTION_ID": question["id"], "MEMEVAL_CAP": str(cfg["cap"])}
    record, stdout, stderr = run_adapter(adapter, arm, prompt, cfg, model, env)
    record.update(question_id=question["id"], arm=arm, prompt=prompt)
    record["violations"] = violations(arm, record["tool_calls"], cfg)
    stem = "%s__%s" % (question["id"], arm)
    raw_dir = os.path.join(out, "raw")
    os.makedirs(raw_dir, exist_ok=True)
    with open(os.path.join(raw_dir, stem + ".stdout"), "w", encoding="utf-8") as handle:
        handle.write(stdout)
    with open(os.path.join(raw_dir, stem + ".stderr"), "w", encoding="utf-8") as handle:
        handle.write(stderr)
    path = _run_path(out, question["id"], arm)
    old = load_json(path) if os.path.exists(path) else None
    if old is not None:  # a failed attempt is kept, never overwritten
        failed_dir = os.path.join(out, "runs", "failed")
        os.makedirs(failed_dir, exist_ok=True)
        earlier = len([n for n in os.listdir(failed_dir) if n.startswith(stem + ".")])
        shutil.move(path, os.path.join(failed_dir, "%s.%d.json" % (stem, earlier + 1)))
    save_json(path, record)
    return stem, record


def cmd_run(args):
    cfg = load_config(args.config)
    questions = load_questions(args.questions)
    arms = args.arms.split(",") if args.arms else cfg["arms"]
    for arm in arms:
        if arm not in ARMS:
            raise SystemExit("unknown arm %r" % arm)
    if args.only:
        wanted = set(args.only.split(","))
        questions = [q for q in questions if q["id"] in wanted]
    adapter = load_adapter(args.adapter)
    out = args.out
    if args.dry_run:
        for question in questions:
            for arm in arms:
                print("# %s %s" % (question["id"], arm))
                print(" ".join(repr(a) if " " in a else a for a in build_argv(adapter, arm, "<prompt>", cfg, args.model)))
                print(build_prompt(question, arm, cfg))
                print()
        return 0
    save_json(os.path.join(out, "plan.json"), {"cfg": cfg, "arms": arms, "question_ids": [q["id"] for q in questions], "adapter": os.path.basename(args.adapter), "model": args.model or ""})
    shutil.copyfile(args.questions, os.path.join(out, "questions.json"))
    existing = read_runs(out)
    jobs = []
    for question in questions:
        for arm in arms:
            done = existing.get((question["id"], arm))
            if done is not None and done["status"] == "ok" and not args.force:
                continue  # an answered cell, violating or not, is never re-rolled
            jobs.append((out, question, arm, adapter, cfg, args.model))
    print("%d cells to run, %d already answered" % (len(jobs), len(questions) * len(arms) - len(jobs)))
    with ThreadPoolExecutor(max_workers=args.jobs) as pool:
        for stem, record in pool.map(_execute, jobs):
            print("%s status=%s calls=%d input_tokens=%d violations=%d" % (stem, record["status"], len(record["tool_calls"]), record["input_tokens"], len(record["violations"])))
    return 0


def cmd_blind(args):
    questions = load_questions(os.path.join(args.out, "questions.json"))
    batches, graded = write_blind(args.out, questions, read_runs(args.out), args.seed, args.batch_size)
    print("%d answers in %d batches under %s/blind (map.private.json holds the arm labels: keep it from the grader)" % (graded, batches, args.out))
    return 0


def cmd_grade(args):
    plan = load_json(os.path.join(args.out, "plan.json"))
    failed = run_grading(args.out, load_adapter(args.adapter), plan["cfg"], args.model, args.force)
    for line in failed:
        print("FAILED " + line, file=sys.stderr)
    print("graded; %d batches failed (details in grades/*.error.txt; rerun the step to retry them)" % len(failed))
    return 1 if failed else 0


def cmd_aggregate(args):
    plan = load_json(os.path.join(args.out, "plan.json"))
    runs = read_runs(args.out)
    mapping = read_mapping(args.out)
    graded = unblind(read_grades(args.out), mapping)
    report = aggregate(plan["question_ids"], plan["arms"], runs, graded)
    text = markdown(report, plan["arms"])
    save_json(os.path.join(args.out, "report.json"), report)
    with open(os.path.join(args.out, "report.md"), "w", encoding="utf-8") as handle:
        handle.write(text)
    print(text)
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(prog="memeval", description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    run = sub.add_parser("run", help="one fresh agent per question x arm")
    run.add_argument("--config", required=True)
    run.add_argument("--questions", required=True)
    run.add_argument("--adapter", required=True)
    run.add_argument("--out", required=True)
    run.add_argument("--model", help="passed through the adapter's model_args")
    run.add_argument("--arms", help="comma list; default: the config's arms")
    run.add_argument("--only", help="comma list of question ids")
    run.add_argument("--jobs", type=int, default=1)
    run.add_argument("--force", action="store_true", help="re-run answered cells too (discouraged)")
    run.add_argument("--dry-run", action="store_true", help="print commands and prompts, run nothing")
    run.set_defaults(func=cmd_run)

    blind = sub.add_parser("blind", help="shuffle and anonymise answers for grading")
    blind.add_argument("--out", required=True)
    blind.add_argument("--seed", type=int, required=True)
    blind.add_argument("--batch-size", type=int, default=5)
    blind.set_defaults(func=cmd_blind)

    grade = sub.add_parser("grade", help="grade each blind batch with a grader agent")
    grade.add_argument("--out", required=True)
    grade.add_argument("--adapter", required=True, help="grader adapter (may differ from the answering one)")
    grade.add_argument("--model")
    grade.add_argument("--force", action="store_true")
    grade.set_defaults(func=cmd_grade)

    agg = sub.add_parser("aggregate", help="join grades and runs into the results table")
    agg.add_argument("--out", required=True)
    agg.set_defaults(func=cmd_aggregate)

    args = parser.parse_args(argv)
    return args.func(args)
