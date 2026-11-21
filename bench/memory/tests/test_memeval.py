import json
import unittest

from memeval.adapters import parse_claude, parse_codex, parse_generic, parse_opencode
from memeval.aggregate import aggregate
from memeval.blind import build_blind, extract_answer, unblind
from memeval.common import derive_overall
from memeval.grading import validate_grades
from memeval.policy import violations

CFG = {
    "cap": 3,
    "workdir": "/work/proj",
    "notes_dir": "/work/proj/notes",
    "transcripts_dir": "/work/proj/chats",
    "chat_search_tool": "search_past_chats",
    "notes_search_tool": "search_notes",
    "extra_forbidden_tools": ["^project_search$"],
}


def call(tool, text):
    return {"tool": tool, "input": text}


def record(qid, arm, answer="", status="ok", calls=None, tokens=1000, bad=None):
    return {
        "question_id": qid, "arm": arm, "status": status, "answer": answer, "input_tokens": tokens,
        "tool_calls": calls or [], "violations": bad or [],
    }


class BlindTests(unittest.TestCase):
    QUESTIONS = [
        {"id": "Q%d" % i, "question": "question %d?" % i, "gold_facts": ["fact %d" % i]} for i in range(1, 5)
    ]

    def runs(self):
        out = {}
        for q in self.QUESTIONS:
            for number, arm in enumerate(("notes", "code_git")):
                answer = json.dumps({"answer": "ANSWER-%s-%d [x](/secret/path)" % (q["id"], number), "sources": ["/secret/path"], "unknowns": []})
                out[(q["id"], arm)] = record(q["id"], arm, answer)
        return out

    def test_round_trip_returns_each_grade_to_its_own_cell(self):
        runs = self.runs()
        batches, mapping = build_blind(self.QUESTIONS, runs, seed=3, batch_size=3)
        grades = {}
        for batch in batches:
            for case in batch:  # a grader that just echoes the answer it saw
                grades[case["anonymous_id"]] = {"seen": case["answer"], "question": case["question"]}
        for (qid, arm), grade in unblind(grades, mapping).items():
            self.assertIn("ANSWER-%s-%d" % (qid, ("notes", "code_git").index(arm)), grade["seen"])
            self.assertEqual(grade["question"], "question %s?" % qid[1:])

    def test_labels_do_not_leak_and_order_depends_on_seed(self):
        runs = self.runs()
        a, map_a = build_blind(self.QUESTIONS, runs, seed=1, batch_size=8)
        b, map_b = build_blind(self.QUESTIONS, runs, seed=2, batch_size=8)
        self.assertNotEqual(map_a, map_b)
        blob = json.dumps(a)
        self.assertNotIn("/secret/path", blob)
        self.assertNotIn("code_git", blob)
        self.assertEqual(len(map_a), 8)

    def test_violating_and_failed_runs_are_not_sent_to_the_grader(self):
        runs = self.runs()
        runs[("Q1", "notes")]["violations"] = ["x"]
        runs[("Q2", "notes")]["status"] = "error"
        _, mapping = build_blind(self.QUESTIONS, runs, seed=1, batch_size=8)
        self.assertEqual(len(mapping), 6)
        self.assertNotIn({"question_id": "Q1", "arm": "notes"}, mapping.values())

    def test_extract_answer_falls_back_to_raw_text(self):
        self.assertEqual(extract_answer("plain text"), ("plain text", []))
        self.assertEqual(extract_answer('x {"answer": "a", "unknowns": ["u"]} y'), ("a", ["u"]))


class RubricTests(unittest.TestCase):
    def facts(self, *verdicts):
        return [{"index": i, "verdict": v} for i, v in enumerate(verdicts, 1)]

    def test_rubric(self):
        self.assertEqual(derive_overall(self.facts("supported", "supported"), []), "correct")
        self.assertEqual(derive_overall(self.facts("supported", "supported"), ["x"]), "partial")
        self.assertEqual(derive_overall(self.facts("supported", "missing", "missing"), []), "partial")
        self.assertEqual(derive_overall(self.facts("missing", "missing"), []), "wrong")
        self.assertEqual(derive_overall(self.facts("supported", "contradicted", "contradicted"), []), "wrong")
        self.assertEqual(derive_overall(self.facts("supported", "supported", "contradicted"), []), "partial")
        self.assertEqual(derive_overall(self.facts("supported", "contradicted"), []), "partial")  # half is not a majority

    def test_validation_rejects_incomplete_grades_and_rederives_overall(self):
        batch = [{"anonymous_id": "C001", "gold_facts": ["a", "b"]}]
        good = [{"anonymous_id": "C001", "overall": "correct", "facts": [
            {"index": 1, "verdict": "supported", "reason": "r"}, {"index": 2, "verdict": "missing", "reason": "r"}]}]
        self.assertEqual(validate_grades(batch, good)[0]["overall"], "partial")  # grader said correct; rubric wins
        with self.assertRaises(ValueError):
            validate_grades(batch, [dict(good[0], facts=good[0]["facts"][:1])])
        with self.assertRaises(ValueError):
            validate_grades(batch, [dict(good[0], anonymous_id="C002")])
        with self.assertRaises(ValueError):
            validate_grades(batch, [dict(good[0], facts=[{"index": 1, "verdict": "ok"}, {"index": 2, "verdict": "missing"}])])
        with self.assertRaises(ValueError):
            validate_grades(batch + [{"anonymous_id": "C002", "gold_facts": ["a"]}], good)


class AggregateTests(unittest.TestCase):
    def test_counts_scores_and_medians(self):
        qids = ["Q1", "Q2", "Q3", "Q4"]
        runs = {
            ("Q1", "notes"): record("Q1", "notes", tokens=10_000, calls=[call("Read", "")]),
            ("Q2", "notes"): record("Q2", "notes", tokens=50_000, calls=[call("Read", "")] * 3),
            ("Q3", "notes"): record("Q3", "notes", tokens=20_000, bad=["touches another arm's folder"]),
            ("Q4", "notes"): record("Q4", "notes", status="error", tokens=0),
        }
        grades = {("Q1", "notes"): {"overall": "correct"}, ("Q2", "notes"): {"overall": "partial"}, ("Q3", "notes"): {"overall": "correct"}}
        arm = aggregate(qids, ["notes"], runs, grades)["arms"]["notes"]
        self.assertEqual((arm["correct"], arm["partial"], arm["wrong"]), (1, 1, 2))  # violation scored wrong despite a correct grade
        self.assertEqual((arm["violations"], arm["failed_or_missing"]), (1, 1))
        self.assertAlmostEqual(arm["mean_score"], 1.5 / 4)
        self.assertAlmostEqual(arm["mean_score_graded_only"], 0.75)
        self.assertEqual(arm["median_input_tokens"], 20_000)  # ok runs only: 10k, 50k, 20k

    def test_missing_cell_counts_wrong_and_ungraded_answer_is_an_error(self):
        arm = aggregate(["Q1"], ["notes"], {}, {})["arms"]["notes"]
        self.assertEqual((arm["wrong"], arm["failed_or_missing"]), (1, 1))
        with self.assertRaises(ValueError):
            aggregate(["Q1"], ["notes"], {("Q1", "notes"): record("Q1", "notes")}, {})


class PolicyTests(unittest.TestCase):
    def test_clean_runs_pass(self):
        self.assertEqual(violations("notes", [call("Read", '{"file_path": "/work/proj/notes/a/context.md"}')], CFG), [])
        self.assertEqual(violations("transcripts", [call("Bash", "rg -n sync chats/")], CFG), [])
        self.assertEqual(violations("past_chat", [call("mcp__x__search_past_chats", "{}")], CFG), [])
        self.assertEqual(violations("code_git", [call("Bash", "git log -- . ':(exclude,glob)**/notes/**'")], CFG), [])

    def test_folder_crossing(self):
        self.assertTrue(violations("notes", [call("Read", '{"file_path": "/work/proj/chats/c1.jsonl"}')], CFG))
        self.assertTrue(violations("notes", [call("Bash", "cat chats/c1.jsonl")], CFG))
        self.assertTrue(violations("transcripts", [call("Read", "/work/proj/notes/a.md")], CFG))
        self.assertTrue(violations("code_git", [call("Bash", "cat notes/a/context.md")], CFG))
        self.assertTrue(violations("code_git", [call("Bash", "rg sync /work/proj/chats")], CFG))
        self.assertEqual(violations("code_git", [call("Bash", "rg sync /work/proj/notes2/x")], CFG), [])  # not the notes folder

    def test_exclusions_are_not_access(self):
        for text in ("rg sync --exclude-dir=notes src", "rg sync -g '!notes/**' src", "git log -- . ':(exclude)notes'"):
            self.assertEqual(violations("code_git", [call("Bash", text)], CFG), [], text)

    def test_git_only_in_code_arm(self):
        self.assertTrue(violations("notes", [call("Bash", "git log")], CFG))
        self.assertTrue(violations("notes", [call("shell", "cd x && git show HEAD")], CFG))
        self.assertEqual(violations("notes", [call("Read", "/work/proj/notes/digit-notes.md")], CFG), [])

    def test_tools_and_cap(self):
        for tool in ("Write", "Edit", "WebFetch", "Task", "mcp__exa__web_search_exa", "apply_patch"):
            self.assertTrue(violations("code_git", [call(tool, "{}")], CFG), tool)
        self.assertTrue(violations("code_git", [call("search_past_chats", "{}")], CFG))
        self.assertEqual(violations("notes", [call("search_notes", "{}")], CFG), [])
        self.assertTrue(violations("code_git", [call("search_notes", "{}")], CFG))
        self.assertTrue(violations("notes", [call("project_search", "{}")], CFG))
        self.assertTrue(violations("past_chat", [call("Bash", "ls")], CFG))
        self.assertEqual(violations("notes", [call("Read", "")] * 3, CFG), [])
        self.assertTrue(violations("notes", [call("Read", "")] * 4, CFG))


CODEX = "\n".join(json.dumps(e) for e in [
    {"type": "thread.started", "thread_id": "t"},
    {"type": "item.completed", "item": {"id": "i0", "type": "agent_message", "text": "Looking."}},
    {"type": "item.started", "item": {"id": "i1", "type": "command_execution", "command": "rg x notes", "status": "in_progress"}},
    {"type": "item.completed", "item": {"id": "i1", "type": "command_execution", "command": "rg x notes", "exit_code": 0}},
    {"type": "item.completed", "item": {"id": "i2", "type": "mcp_tool_call", "server": "memory", "tool": "search", "arguments": {"query": "x"}}},
    {"type": "item.completed", "item": {"id": "i3", "type": "agent_message", "text": "FINAL"}},
    {"type": "turn.completed", "usage": {"input_tokens": 1000, "cached_input_tokens": 400, "output_tokens": 20}},
])

OPENCODE = "\n".join(json.dumps(e) for e in [
    {"type": "step_start", "part": {"type": "step-start"}},
    {"type": "tool_use", "part": {"type": "tool", "tool": "read", "callID": "c1", "state": {"status": "completed", "input": {"filePath": "notes/a.md"}}}},
    {"type": "step_finish", "part": {"type": "step-finish", "tokens": {"input": 100, "output": 5, "reasoning": 2, "cache": {"write": 10, "read": 0}}}},
    {"type": "text", "part": {"type": "text", "text": "thinking aloud"}},
    {"type": "tool_use", "part": {"type": "tool", "tool": "glob", "callID": "c2", "state": {"input": {"pattern": "*"}}}},
    {"type": "text", "part": {"type": "text", "text": "FINAL"}},
    {"type": "step_finish", "part": {"type": "step-finish", "tokens": {"input": 50, "output": 7, "reasoning": 0, "cache": {"write": 0, "read": 200}}}},
])

CLAUDE = "\n".join(json.dumps(e) for e in [
    {"type": "system", "subtype": "init"},
    {"type": "assistant", "message": {"content": [{"type": "text", "text": "ok"}, {"type": "tool_use", "id": "u1", "name": "Read", "input": {"file_path": "notes/a.md"}}]}},
    {"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "u1", "name": "Read", "input": {"file_path": "notes/a.md"}}]}},
    {"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "u2", "name": "Grep", "input": {"pattern": "x"}}]}},
    {"type": "result", "is_error": False, "result": "FINAL", "usage": {"input_tokens": 10, "cache_creation_input_tokens": 100, "cache_read_input_tokens": 500, "output_tokens": 30}},
])


class ParserTests(unittest.TestCase):
    def test_codex(self):
        r = parse_codex(CODEX)
        self.assertEqual(r["answer"], "FINAL")
        self.assertEqual([c["tool"] for c in r["tool_calls"]], ["shell", "mcp:memory.search"])
        self.assertEqual((r["input_tokens"], r["cached_input_tokens"], r["output_tokens"]), (1000, 400, 20))

    def test_opencode(self):
        r = parse_opencode(OPENCODE)
        self.assertEqual(r["answer"], "FINAL")  # text before the last tool call is not the answer
        self.assertEqual([c["tool"] for c in r["tool_calls"]], ["read", "glob"])
        self.assertEqual((r["input_tokens"], r["cached_input_tokens"], r["output_tokens"]), (360, 200, 14))

    def test_claude(self):
        r = parse_claude(CLAUDE)
        self.assertEqual(r["answer"], "FINAL")
        self.assertEqual([c["tool"] for c in r["tool_calls"]], ["Read", "Grep"])  # u1 is repeated and counted once
        self.assertEqual((r["input_tokens"], r["cached_input_tokens"], r["output_tokens"]), (610, 500, 30))
        error = parse_claude(CLAUDE.replace('"is_error": false', '"is_error": true'))
        self.assertTrue(error["error"])

    def test_generic_and_failures(self):
        r = parse_generic('noise\n{"answer": "a", "input_tokens": 5, "output_tokens": 1, "tool_calls": [{"tool": "t", "input": {"k": 1}}]}')
        self.assertEqual((r["answer"], r["input_tokens"], r["tool_calls"][0]["input"]), ("a", 5, '{"k": 1}'))
        for parser in (parse_generic, parse_codex, parse_opencode, parse_claude):
            self.assertTrue(parser("not json")["error"])


if __name__ == "__main__":
    unittest.main()
