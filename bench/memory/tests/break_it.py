#!/usr/bin/env python3
"""Break-it proof: apply one mutation at a time to a copy of the kit and require the tests to go red.

Usage (from bench/memory): python3 tests/break_it.py
"""
import os, shutil, subprocess, sys, tempfile
SRC = sys.argv[1] if len(sys.argv) > 1 else os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MUTATIONS = [
 ("blind: grade returns to the wrong cell", "memeval/blind.py", 'return {(mapping[a]["question_id"], mapping[a]["arm"]): grade', 'return {(mapping[a]["question_id"], "notes"): grade'),
 ("blind: case shows another run's answer", "memeval/blind.py", 'extract_answer(runs[(qid, arm)]["answer"])', 'extract_answer(runs[cells[0]]["answer"])'),
 ("blind: link targets (sources) leak", "memeval/blind.py", 'return LINK_TARGET.sub("]", str(data["answer"])), [LINK_TARGET.sub("]", str(u)) for u in unknowns]', 'return str(data["answer"]), [str(u) for u in unknowns]'),
 ("blind: violating runs get graded", "memeval/blind.py", 'record["status"] == "ok" and not record["violations"]', 'record["status"] == "ok"'),
 ("blind: seed ignored", "memeval/blind.py", 'random.Random(seed).shuffle(cells)', 'random.Random(0).shuffle(cells)'),
 ("aggregate: violation keeps its grade", "memeval/aggregate.py", 'if record["violations"]:', 'if False:'),
 ("aggregate: failed run dropped from wrong", "memeval/aggregate.py", 'return "failed", "wrong"', 'return "failed", "partial"'),
 ("aggregate: median becomes mean", "memeval/aggregate.py", 'statistics.median(tokens)', 'statistics.mean(tokens)'),
 ("aggregate: missing grade tolerated", "memeval/aggregate.py", 'if grade is None:', 'if False:'),
 ("rubric: half contradicted is wrong", "memeval/common.py", 'contradicted * 2 > total', 'contradicted * 2 >= total'),
 ("rubric: extra contradiction ignored", "memeval/common.py", 'if supported == total and not extra_contradictions:', 'if supported == total:'),
 ("grading: bad verdict accepted", "memeval/grading.py", 'if fact.get("verdict") not in VERDICTS:', 'if False:'),
 ("grading: grader label trusted", "memeval/grading.py", '"overall": derive_overall(facts, extra),', '"overall": item.get("overall"),'),
 ("grading: missing case accepted", "memeval/grading.py", 'if seen != set(expected):', 'if False:'),
 ("policy: exclusions count as access", "memeval/policy.py", 'text = EXCLUDES.sub(" ", str(call.get("input", "")))', 'text = str(call.get("input", ""))'),
 ("policy: git allowed everywhere", "memeval/policy.py", 'if forbid_git and GIT_COMMAND.search(text):', 'if False:'),
 ("policy: relative folder not checked", "memeval/policy.py", 'parts.append(r"(?<![\\w.-])" + re.escape(rel) + r"/")', 'pass'),
 ("policy: cap not enforced", "memeval/policy.py", 'if len(tool_calls) > cfg["cap"]:', 'if False:'),
 ("policy: edit/web tools allowed", "memeval/policy.py", 'if low in EXACT_FORBIDDEN_TOOLS or WEB_TOOL.search(low):', 'if False:'),
 ("policy: past_chat may use shell", "memeval/policy.py", 'if arm == "past_chat" and low in LOCAL_READ_TOOLS:', 'if False:'),
 ("policy: chat tool allowed in other arms", "memeval/policy.py", 'if arm != "past_chat" and chat_tool and chat_tool in low:', 'if False:'),
 ("parser claude: repeated tool_use double counted", "memeval/adapters.py", 'and block.get("id") not in seen', ''),
 ("parser claude: cache tokens not in input", "memeval/adapters.py", '+ int(usage.get("cache_creation_input_tokens") or 0) + cached', ''),
 ("parser opencode: answer includes pre-tool chatter", "memeval/adapters.py", '            texts = []\n        elif kind == "text":', '            pass\n        elif kind == "text":'),
 ("parser opencode: cache not in input", "memeval/adapters.py", 'inp += int(tokens.get("input") or 0) + int(cache.get("read") or 0) + int(cache.get("write") or 0)', 'inp += int(tokens.get("input") or 0)'),
 ("parser codex: counts started and completed items", "memeval/adapters.py", 'if kind == "item.completed":', 'if kind in ("item.completed", "item.started"):'),
 ("parser codex: usage not summed", "memeval/adapters.py", 'inp += int(usage.get("input_tokens") or 0)', 'inp = int(usage.get("input_tokens") or 0) * 2'),
]
bad = 0
for name, rel, old, new in MUTATIONS:
    work = tempfile.mkdtemp()
    try:
        dst = os.path.join(work, "kit")
        shutil.copytree(SRC, dst, ignore=shutil.ignore_patterns("__pycache__"))
        path = os.path.join(dst, rel)
        text = open(path).read()
        if old not in text:
            print("MUTATION NOT APPLICABLE:", name); bad += 1; continue
        open(path, "w").write(text.replace(old, new, 1))
        proc = subprocess.run([sys.executable, "-m", "unittest", "discover", "-s", "tests", "-t", "."], cwd=dst, capture_output=True, text=True)
        failed = sorted(set(l.split()[1] for l in proc.stderr.splitlines() if l.startswith(("FAIL:", "ERROR:"))))
        verdict = "RED " if proc.returncode else "GREEN (test gap!)"
        if not proc.returncode: bad += 1
        print("%s %-50s %s" % (verdict, name, ",".join(failed)))
    finally:
        shutil.rmtree(work)
print("unmutated:", subprocess.run([sys.executable, "-m", "unittest", "discover", "-s", "tests", "-t", "."], cwd=SRC, capture_output=True, text=True).stderr.strip().splitlines()[-1])
sys.exit(1 if bad else 0)
