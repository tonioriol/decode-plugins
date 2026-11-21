"""Shared types: arms, questions, config, prompts, scoring."""
import json
import os

ARMS = ("notes", "transcripts", "past_chat", "code_git")
VERDICTS = ("supported", "missing", "contradicted")
SCORES = {"correct": 1.0, "partial": 0.5, "wrong": 0.0}


def load_json(path):
    with open(path, encoding="utf-8") as handle:
        return json.load(handle)


def save_json(path, data):
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(data, handle, indent=2, ensure_ascii=False)
        handle.write("\n")


def load_questions(path):
    data = load_json(path)
    questions = data["questions"]
    seen = set()
    for question in questions:
        qid = question.get("id")
        if not isinstance(qid, str) or not qid or "/" in qid or qid in seen:
            raise ValueError("question ids must be unique non-empty strings without '/': %r" % (qid,))
        seen.add(qid)
        if not isinstance(question.get("question"), str) or not question["question"].strip():
            raise ValueError("%s: missing question text" % qid)
        facts = question.get("gold_facts")
        if not isinstance(facts, list) or not facts or not all(isinstance(f, str) and f.strip() for f in facts):
            raise ValueError("%s: gold_facts must be a non-empty list of strings" % qid)
    return questions


def load_config(path):
    """Read config.json; relative directories resolve against the config file."""
    base = os.path.dirname(os.path.abspath(path))
    cfg = load_json(path)

    def resolve(value):
        return os.path.normpath(os.path.join(base, value)) if value else ""

    out = {
        "cap": int(cfg.get("cap", 25)),
        "cutoff": cfg.get("cutoff", ""),
        "workdir": resolve(cfg.get("workdir", ".")),
        "notes_dir": resolve(cfg.get("notes_dir", "")),
        "transcripts_dir": resolve(cfg.get("transcripts_dir", "")),
        "code_dir": resolve(cfg.get("code_dir", cfg.get("workdir", "."))),
        "notes_search_tool": cfg.get("notes_search_tool", ""),
        "chat_search_tool": cfg.get("chat_search_tool", ""),
        "extra_forbidden_tools": list(cfg.get("extra_forbidden_tools", [])),
        "arms": list(cfg.get("arms", ARMS)),
    }
    for arm in out["arms"]:
        if arm not in ARMS:
            raise ValueError("unknown arm %r (choose from %s)" % (arm, ", ".join(ARMS)))
    return out


def derive_overall(facts, extra_contradictions):
    """The rubric, applied to per-fact verdicts. Never trust a grader's own label."""
    total = len(facts)
    supported = sum(1 for f in facts if f["verdict"] == "supported")
    contradicted = sum(1 for f in facts if f["verdict"] == "contradicted")
    if supported == total and not extra_contradictions:
        return "correct"
    if supported == 0 or contradicted * 2 > total:
        return "wrong"
    return "partial"


COMMON_RULES = (
    "You are answering one historical recall question in a controlled evaluation. "
    "Do not delegate, edit files, create notes, change settings, or inspect the evaluation's own files "
    "or other agents. The access rules below define the experiment: ignore any general instruction to use "
    "other tools or sources. Finish with a plain final answer, not a follow-up question."
)

ANSWER_RULES = (
    "Use at most {cap} tool calls in total. Do not evade the budget with scripts or wrappers that issue many "
    "searches in one call. Answer within 350 words. Give the actual facts with citations and explicit unknowns; "
    "never fabricate a reason, measurement, name or commit. Do not claim that present code proves historical "
    "intent: label inference as inference, and report anything not established by evidence as unknown. "
    "Return valid JSON only, with keys question_id, answer (string), sources (array of strings), unknowns "
    "(array of strings). No answer key is provided. Stop after the answer."
)


def arm_rules(arm, cfg):
    cutoff = cfg["cutoff"]
    cutoff_rule = (
        " Ignore anything dated after %s: it may discuss this evaluation." % cutoff if cutoff else ""
    )
    if arm == "notes":
        search = (
            " Use the search tool %s first if you have it, then read the notes directly." % cfg["notes_search_tool"]
            if cfg["notes_search_tool"]
            else " Search and read the notes directly."
        )
        return (
            "You may read only the notes folder %s.%s No transcripts, past-chat search, source code, git history, "
            "web or other agents." % (cfg["notes_dir"], search)
        )
    if arm == "transcripts":
        return (
            "You may search and read only the raw chat transcript files under %s (grep and direct reads). "
            "No notes, past-chat search tool, source code, git history, web or other agents. Do not list or "
            "read the parent of that folder.%s" % (cfg["transcripts_dir"], cutoff_rule)
        )
    if arm == "past_chat":
        return (
            "You may use only the harness's past-chat search tool%s to find and read earlier chats. No notes, raw "
            "transcript files, source code, git history, web or other agents.%s"
            % (" (" + cfg["chat_search_tool"] + ")" if cfg["chat_search_tool"] else "", cutoff_rule)
        )
    if arm == "code_git":
        glob = "**/%s/**" % os.path.basename(cfg["notes_dir"]) if cfg["notes_dir"] else "**/notes/**"
        return (
            "You may use only the source code and git history of %s. No notes or documentation folders, "
            "transcripts, past-chat search, web or other agents. Restrict git log to code paths, for example: "
            "git log -- . ':(exclude,glob)%s'. Do not use unfiltered root logs; restrict content searches to "
            "actual source directories." % (cfg["code_dir"], glob)
        )
    raise ValueError(arm)


def build_prompt(question, arm, cfg):
    return "\n\n".join(
        [
            COMMON_RULES,
            "Question ID: %s\nQuestion: %s" % (question["id"], question["question"]),
            "Access rules: " + arm_rules(arm, cfg),
            ANSWER_RULES.format(cap=cfg["cap"]),
        ]
    )
