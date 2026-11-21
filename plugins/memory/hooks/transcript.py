"""A chat transcript as one list of messages, oldest first, whichever client wrote it.

Decode journals (checkpoint `apiMessages`, `api` patches), Claude Code (`message` records) and Codex rollouts
(`response_item` / `event_msg` records) become {"ts", "role"?, "content": [...], "native"?} messages with
Anthropic-style content items (text, tool_use, tool_result).
"""
import datetime
import json
import re
import shlex
from urllib.parse import unquote, urlparse


def timestamp(value):
    if isinstance(value, str):
        return int(datetime.datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp() * 1000)
    return value


def result_text(item):
    text = item.get("content", "")
    if isinstance(text, list):
        text = "\n".join(part.get("text", "") for part in text if part.get("type") in ("text", "input_text"))
    return text if isinstance(text, str) else ""


def native_item(record):
    # Codex rollout records use response_item payloads, not Claude messages.
    item = record["payload"]
    kind = item.get("type")
    if kind == "message" and item.get("role") == "user":
        content = item.get("content", "")
        if isinstance(content, list):
            parts = [{"type": "text", "text": p.get("text", "")} for p in content if p.get("type") in ("text", "input_text")]
            return ("user", parts) if parts else (None, None)
        return ("user", [{"type": "text", "text": content}]) if isinstance(content, str) and content else (None, None)
    if kind in ("function_call", "custom_tool_call"):
        name = item["name"]
        raw = item.get("arguments", item.get("input", ""))
        params = {"command": raw} if kind == "custom_tool_call" else json.loads(raw)
        if name in ("exec_command", "shell_command"):
            name = "execute_command"
            params["command"] = params.pop("cmd", params.get("command", ""))
        return None, [{"type": "tool_use", "id": item["call_id"], "name": name, "input": params}]
    if kind in ("function_call_output", "custom_tool_call_output"):
        output = result_text({"content": item.get("output", "")})
        return None, [{"type": "tool_result", "tool_use_id": item["call_id"], "content": output,
                       "is_error": bool(re.search(r"Process exited with code [1-9]|Failed to apply patch", output))}]
    return None, None


def native_event(record):
    # Code-mode wrappers need not print their nested tool results (apply_patch
    # returns {}). Codex records the actual completed mutation separately.
    event = record["payload"]
    if event.get("type") != "item_completed":
        return []
    item = event["item"]
    kind = item.get("type")
    if kind not in ("CommandExecution", "FileChange") or item["status"] != "completed":
        return []
    call = item["id"]
    if kind == "CommandExecution":
        name = "execute_command"
        params = {"command": shlex.join(item["command"]), "cwd": unquote(urlparse(item["cwd"]).path)}
        result = {"type": "tool_result", "tool_use_id": call,
                  "content": f"Process exited with code {item['exit_code']}\nFinal output:\n{item['aggregated_output']}",
                  "is_error": item["exit_code"] != 0}
    else:
        name = "apply_patch"
        params = {"paths": list(item["changes"])}
        result = {"type": "tool_result", "tool_use_id": call, "content": item["stdout"],
                  "confirmed_paths": list(item["changes"])}
    return [{"ts": event["started_at_ms"], "native": "codex", "content": [
                {"type": "tool_use", "id": call, "name": name, "input": params}]},
            {"ts": event["completed_at_ms"], "native": "codex", "content": [result]}]


def messages(path):
    out = []
    with open(path) as stream:
        for line in stream:
            record = json.loads(line)
            ts = timestamp(record.get("timestamp", record.get("ts", 0)))
            if record.get("type") == "event_msg":
                out.extend(native_event(record))
                continue
            if record.get("type") == "response_item":
                role, items = native_item(record)
                if items:
                    out.append({"ts": ts, "content": items, "native": "codex", **({"role": role} if role else {})})
                continue
            if record.get("isMeta"):
                continue
            batch = record.get("apiMessages", [])
            if record.get("representation") != "ui":
                batch += record.get("messages", [])
            batch += [record[key] for key in ("message", "apiMessage") if key in record]
            for message in batch:
                message.setdefault("ts", ts)
                if "timestamp" in record and "message" in record:
                    message["native"] = "claude"
            out.extend(batch)
    out.sort(key=lambda message: message.get("ts", 0))
    return out
