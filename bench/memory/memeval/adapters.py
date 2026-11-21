"""Run one non-interactive prompt through a harness and normalise the result.

An adapter is a JSON file: a command template plus the name of an output parser. Every parser returns
{answer, input_tokens, output_tokens, cached_input_tokens, tool_calls:[{tool, input}], error}.
input_tokens counts every input token the harness reports across all model requests of the run
(cached tokens included); cached_input_tokens is the cached part of it.
"""
import json
import os
import re
import subprocess
import time

from .common import load_json


def _json_lines(text):
    events = []
    for line in text.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            events.append(json.loads(line))
        except ValueError:
            continue
    return events


def _dump(value):
    return value if isinstance(value, str) else json.dumps(value, ensure_ascii=False, sort_keys=True)


def _result(answer="", inp=0, out=0, cached=0, calls=None, error=None):
    return {
        "answer": answer,
        "input_tokens": inp,
        "output_tokens": out,
        "cached_input_tokens": cached,
        "tool_calls": calls or [],
        "error": error,
    }


def parse_generic(stdout):
    """Custom wrapper contract: print one JSON object with answer, input_tokens, output_tokens, tool_calls."""
    events = _json_lines(stdout)
    if not events:
        return _result(error="no JSON object on stdout")
    data = events[-1]
    return _result(
        answer=str(data.get("answer", "")),
        inp=int(data.get("input_tokens", 0)),
        out=int(data.get("output_tokens", 0)),
        cached=int(data.get("cached_input_tokens", 0)),
        calls=[{"tool": str(c.get("tool", "")), "input": _dump(c.get("input", ""))} for c in data.get("tool_calls", [])],
        error=data.get("error"),
    )


def parse_claude(stdout):
    """claude -p --output-format stream-json --verbose"""
    calls, seen, final = [], set(), None
    for event in _json_lines(stdout):
        if event.get("type") == "assistant":
            content = (event.get("message") or {}).get("content")
            for block in content if isinstance(content, list) else []:
                if block.get("type") == "tool_use" and block.get("id") not in seen:
                    seen.add(block.get("id"))
                    calls.append({"tool": block.get("name", ""), "input": _dump(block.get("input", ""))})
        elif event.get("type") == "result":
            final = event
    if final is None:
        return _result(calls=calls, error="no result event")
    usage = final.get("usage") or {}
    cached = int(usage.get("cache_read_input_tokens") or 0)
    inp = int(usage.get("input_tokens") or 0) + int(usage.get("cache_creation_input_tokens") or 0) + cached
    answer = str(final.get("result", ""))
    return _result(
        answer=answer, inp=inp, out=int(usage.get("output_tokens") or 0), cached=cached, calls=calls,
        error=answer or "error result" if final.get("is_error") else None,
    )


def parse_codex(stdout):
    """codex exec --json"""
    calls, answer, inp, out, cached, error = [], "", 0, 0, 0, None
    for event in _json_lines(stdout):
        kind = event.get("type")
        if kind == "item.completed":
            item = event.get("item") or {}
            itype = item.get("type")
            if itype == "agent_message":
                answer = str(item.get("text", ""))
            elif itype == "command_execution":
                calls.append({"tool": "shell", "input": _dump(item.get("command", ""))})
            elif itype == "mcp_tool_call":
                name = "mcp:%s.%s" % (item.get("server", ""), item.get("tool", ""))
                calls.append({"tool": name, "input": _dump(item.get("arguments", ""))})
            elif itype in ("file_change", "web_search"):
                calls.append({"tool": itype, "input": _dump(item.get("changes") or item.get("query") or "")})
        elif kind == "turn.completed":
            usage = event.get("usage") or {}
            inp += int(usage.get("input_tokens") or 0)
            cached += int(usage.get("cached_input_tokens") or 0)
            out += int(usage.get("output_tokens") or 0)
        elif kind in ("turn.failed", "error"):
            error = _dump(event.get("error") or event.get("message") or kind)
    if not answer and not error:
        error = "no agent message"
    return _result(answer, inp, out, cached, calls, error)


def parse_opencode(stdout):
    """opencode run --format json"""
    calls, seen, texts, inp, out, cached, error = [], set(), [], 0, 0, 0, None
    for event in _json_lines(stdout):
        kind = event.get("type")
        part = event.get("part") or {}
        if kind == "tool_use":
            if part.get("callID") not in seen:
                seen.add(part.get("callID"))
                calls.append({"tool": part.get("tool", ""), "input": _dump((part.get("state") or {}).get("input", ""))})
            texts = []
        elif kind == "text":
            texts.append(str(part.get("text", "")))
        elif kind == "step_finish":
            tokens = part.get("tokens") or {}
            cache = tokens.get("cache") or {}
            cached += int(cache.get("read") or 0)
            inp += int(tokens.get("input") or 0) + int(cache.get("read") or 0) + int(cache.get("write") or 0)
            out += int(tokens.get("output") or 0) + int(tokens.get("reasoning") or 0)
        elif kind == "error":
            error = _dump(event.get("error") or event)
    answer = "".join(texts)
    if not answer and not error:
        error = "no text answer"
    return _result(answer, inp, out, cached, calls, error)


PARSERS = {"generic": parse_generic, "claude": parse_claude, "codex": parse_codex, "opencode": parse_opencode}


def load_adapter(path):
    adapter = load_json(path)
    if adapter.get("parser") not in PARSERS:
        raise ValueError("adapter parser must be one of %s" % ", ".join(sorted(PARSERS)))
    adapter["_dir"] = os.path.dirname(os.path.abspath(path))
    return adapter


def build_argv(adapter, arm, prompt, cfg, model=None):
    subs = {
        "{model}": model or "",
        "{workdir}": cfg["workdir"],
        "{adapter_dir}": adapter["_dir"],
        "{notes_search_tool}": cfg["notes_search_tool"],
        "{chat_search_tool}": cfg["chat_search_tool"],
    }

    def sub(arg):
        for key, value in subs.items():
            arg = arg.replace(key, value)
        if "," in arg:
            arg = re.sub(r",{2,}", ",", arg).strip(",")
        return arg

    argv = [sub(a) for a in adapter["command"]]
    if model:
        argv += [sub(a) for a in adapter.get("model_args", [])]
    argv += [sub(a) for a in adapter.get("arm_args", {}).get(arm, [])]
    if adapter.get("prompt_via", "stdin") == "arg":
        argv.append(prompt)
    return argv


def run_adapter(adapter, arm, prompt, cfg, model=None, env=None):
    """Run one fresh agent. Returns (record, raw_stdout, raw_stderr)."""
    argv = build_argv(adapter, arm, prompt, cfg, model)
    stdin = prompt if adapter.get("prompt_via", "stdin") == "stdin" else None
    started = time.time()
    status, stdout, stderr = "ok", "", ""
    try:
        proc = subprocess.run(
            argv, input=stdin, capture_output=True, text=True, cwd=cfg["workdir"],
            env=dict(os.environ, **(env or {})), timeout=adapter.get("timeout", 900),
        )
        stdout, stderr = proc.stdout, proc.stderr
    except subprocess.TimeoutExpired as exc:
        status = "timeout"
        stdout = exc.stdout.decode() if isinstance(exc.stdout, bytes) else (exc.stdout or "")
    except OSError as exc:
        status, stderr = "error", str(exc)
    parsed = PARSERS[adapter["parser"]](stdout)
    if status == "ok" and parsed["error"]:
        status = "error"
    record = dict(parsed, status=status, duration_s=round(time.time() - started, 2), model=model or "")
    return record, stdout, stderr
