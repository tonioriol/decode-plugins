"""Access-policy audit: which recorded tool calls break an arm's rules.

Prompts and CLI flags only ask the agent to stay in its lane. This audit reads what it actually did.
It is a best-effort text check, not a sandbox: see README for its limits.
"""
import os
import re

EXACT_FORBIDDEN_TOOLS = {
    "write", "edit", "multiedit", "notebookedit", "apply_patch", "patch", "file_change",
    "task", "agent", "todowrite",
}
WEB_TOOL = re.compile(r"web_?(fetch|search)", re.IGNORECASE)
LOCAL_READ_TOOLS = {"read", "grep", "glob", "ls", "list", "bash", "shell", "find", "cat"}
GIT_COMMAND = re.compile(r"(?:^|[\s;&|(\"'])git\s", re.IGNORECASE)
# Exclusions are how an arm is told to keep out of a folder, so they must not count as access.
EXCLUDES = re.compile(
    r"""(?:
        :\((?:top,)?exclude[^)]*\)[^\s"']*
      | --exclude(?:-dir)?[= ]['"]?[^\s"']+
      | (?:-g|--glob)\s+['"]?![^\s"']+
    )""",
    re.VERBOSE,
)


def _root_pattern(root, workdir):
    parts = [re.escape(root) + r"(?![\w.-])"]
    if workdir and root != workdir and root.startswith(workdir.rstrip(os.sep) + os.sep):
        rel = os.path.relpath(root, workdir)
        parts.append(r"(?<![\w.-])" + re.escape(rel) + r"/")
    return re.compile("|".join(parts))


def violations(arm, tool_calls, cfg):
    found = []
    if len(tool_calls) > cfg["cap"]:
        found.append("cap_exceeded: %d calls > %d" % (len(tool_calls), cfg["cap"]))

    forbidden_roots = {
        "notes": [cfg["transcripts_dir"]],
        "transcripts": [cfg["notes_dir"]],
        "past_chat": [cfg["notes_dir"], cfg["transcripts_dir"]],
        "code_git": [cfg["notes_dir"], cfg["transcripts_dir"]],
    }[arm]
    root_patterns = [(r, _root_pattern(r, cfg["workdir"])) for r in forbidden_roots if r]
    forbid_git = arm in ("notes", "transcripts", "past_chat")
    chat_tool = cfg["chat_search_tool"].lower()
    notes_tool = cfg["notes_search_tool"].lower()
    extra = [re.compile(p, re.IGNORECASE) for p in cfg["extra_forbidden_tools"]]

    for index, call in enumerate(tool_calls, 1):
        tool = str(call.get("tool", ""))
        low = tool.lower()
        text = EXCLUDES.sub(" ", str(call.get("input", "")))
        where = "call %d (%s)" % (index, tool)
        if low in EXACT_FORBIDDEN_TOOLS or WEB_TOOL.search(low):
            found.append("%s: forbidden tool (edit, web or delegation)" % where)
        if arm != "past_chat" and chat_tool and chat_tool in low:
            found.append("%s: past-chat search outside the past_chat arm" % where)
        if arm != "notes" and notes_tool and notes_tool in low:
            found.append("%s: notes search outside the notes arm" % where)
        if arm != "code_git" and any(p.search(tool) for p in extra):
            found.append("%s: forbidden tool for this arm" % where)
        if arm == "past_chat" and low in LOCAL_READ_TOOLS:
            found.append("%s: local file or shell tool in the past_chat arm" % where)
        if forbid_git and GIT_COMMAND.search(text):
            found.append("%s: git command outside the code_git arm" % where)
        for root, pattern in root_patterns:
            if pattern.search(text):
                found.append("%s: touches another arm's folder" % where)
                break
    return found
