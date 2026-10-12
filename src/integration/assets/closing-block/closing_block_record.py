"""Extraction and stdlib validation for the closing-block v3 record."""
# HERDR_INTEGRATION_VERSION=6
from __future__ import annotations

import json
import re
from datetime import datetime
from typing import Any

_TRAILER = re.compile(r"(?:\r?\n)?```closing-state[ \t]*\r?\n(?P<body>[^\r\n]*)\r?\n```[ \t]*\r?\n?\Z")


def extract(text: str) -> tuple[dict[str, Any] | None, str]:
    match = _TRAILER.search(text)
    if not match:
        marker = re.search(r"(?:\r?\n)?```closing-state[ \t]*\r?\n", text)
        return (None, text[:marker.start()]) if marker else (None, text)
    try:
        value = json.loads(match.group("body"))
    except (ValueError, TypeError):
        return None, text[:match.start()]
    return (value if isinstance(value, dict) else None), text[:match.start()]


def validate(record: Any) -> list[str]:
    errors: list[str] = []

    def check(value: Any, typ: type | tuple[type, ...], path: str) -> bool:
        if typ is int and isinstance(value, bool):
            errors.append(f"{path}: expected integer")
            return False
        if not isinstance(value, typ):
            errors.append(f"{path}: expected {getattr(typ, '__name__', 'valid type')}")
            return False
        return True

    def obj(value: Any, path: str, required: set[str], allowed: set[str]) -> bool:
        if not check(value, dict, path): return False
        for key in sorted(required - value.keys()): errors.append(f"{path}.{key}: required")
        for key in sorted(value.keys() - allowed): errors.append(f"{path}.{key}: unknown property")
        return True

    top = {"v", "state", "completion", "goal", "verified_when", "items", "decisions", "workers", "wait", "now", "verified"}
    if not obj(record, "record", {"v", "state", "completion", "items", "decisions", "workers", "wait", "now"}, top): return errors
    if type(record.get("v")) is not int or record.get("v") != 3: errors.append("record.v: expected 3")
    if record.get("state") not in ("idle", "working", "blocked", "unknown"): errors.append("record.state: invalid enum")
    if record.get("completion") not in ("complete", "incomplete"): errors.append("record.completion: invalid enum")
    for key in ("goal", "verified_when"):
        if key in record and record[key] is not None and not isinstance(record[key], str): errors.append(f"record.{key}: expected string or null")
    items = record.get("items")
    if check(items, list, "record.items"):
        for i, item in enumerate(items):
            p=f"record.items[{i}]"; allowed={"n","kind","text","plain","exception","shadow","options","refs","default","default_at"}
            if not obj(item,p,{"n","kind","text","options"},allowed): continue
            if type(item.get("n")) is not int or item["n"] < 1: errors.append(f"{p}.n: expected integer >= 1")
            if item.get("kind") not in ("Approve","Decide","Verify","Answer"): errors.append(f"{p}.kind: invalid enum")
            if not isinstance(item.get("text"),str) or not item["text"]: errors.append(f"{p}.text: expected nonempty string")
            opts=item.get("options")
            if check(opts,list,p+".options"):
                for j,opt in enumerate(opts):
                    q=f"{p}.options[{j}]"
                    if not obj(opt,q,{"letter","text"},{"letter","text","recommended"}): continue
                    if not isinstance(opt.get("letter"),str) or not re.fullmatch("[a-z]",opt["letter"]): errors.append(f"{q}.letter: invalid letter")
                    if not isinstance(opt.get("text"),str) or not opt["text"]: errors.append(f"{q}.text: expected nonempty string")
                    if "recommended" in opt and type(opt["recommended"]) is not bool: errors.append(f"{q}.recommended: expected boolean")
            refs=item.get("refs")
            if "refs" in item:
                if obj(refs,p+".refs",set(),{"pr","ticket","url"}):
                    for key in ("ticket","url"):
                        if key in refs and (not isinstance(refs[key],list) or any(not isinstance(x,str) for x in refs[key])): errors.append(f"{p}.refs.{key}: expected string array")
                    if "ticket" in refs and isinstance(refs["ticket"],list):
                        for x in refs["ticket"]:
                            if not re.fullmatch(r"[A-Z][A-Z0-9]+-[0-9]+",x): errors.append(f"{p}.refs.ticket: invalid ticket {x!r}")
                    if "pr" in refs and isinstance(refs["pr"],list):
                        for j,pr in enumerate(refs["pr"]):
                            q=f"{p}.refs.pr[{j}]"
                            if obj(pr,q,{"repo","number"},{"repo","number","url"}):
                                if not isinstance(pr.get("repo"),str): errors.append(f"{q}.repo: expected string")
                                if type(pr.get("number")) is not int: errors.append(f"{q}.number: expected integer")
                                if "url" in pr and not isinstance(pr["url"],str): errors.append(f"{q}.url: expected string")
            if "default" in item and item["default"] is not None and (not isinstance(item["default"],str) or not re.fullmatch("[a-z]",item["default"])): errors.append(f"{p}.default: invalid letter or null")
            if "default_at" in item and item["default_at"] is not None:
                try:
                    value = item["default_at"]
                    if not isinstance(value, str) or not re.fullmatch(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})", value): raise ValueError()
                    datetime.fromisoformat(value.replace("Z", "+00:00"))
                except (ValueError, AttributeError): errors.append(f"{p}.default_at: invalid date-time")
            for key in ("plain","exception","shadow"):
                if key in item and item[key] is not None and not isinstance(item[key],str): errors.append(f"{p}.{key}: expected string or null")
    for key,required,allowed in (("decisions",{"n","text"},{"n","text","action","revert","recommendation","reversible","decided_at"}),("workers",{"name","activity"},{"name","activity","run_id","agent"})):
        values=record.get(key)
        if check(values,list,"record."+key):
            for i,value in enumerate(values):
                p=f"record.{key}[{i}]"
                if obj(value,p,required,allowed):
                    for field in allowed & value.keys():
                        v=value[field]
                        if field in {"n"} and (type(v) is not int or v < 1): errors.append(f"{p}.{field}: expected integer >= 1")
                        elif field in {"text","name","activity"} and not isinstance(v,str): errors.append(f"{p}.{field}: expected string")
                        elif field in {"action","revert","recommendation","decided_at","run_id","agent"} and v is not None and not isinstance(v,str): errors.append(f"{p}.{field}: expected string or null")
                        elif field=="reversible" and type(v) is not bool: errors.append(f"{p}.{field}: expected boolean")
    wait=record.get("wait")
    if wait is not None and obj(wait,"record.wait",{"event"},{"event","wakeup_id","eta_s"}):
        if not isinstance(wait.get("event"),str): errors.append("record.wait.event: expected string")
        if "wakeup_id" in wait and wait["wakeup_id"] is not None and not isinstance(wait["wakeup_id"],str): errors.append("record.wait.wakeup_id: expected string or null")
        if "eta_s" in wait and wait["eta_s"] is not None and type(wait["eta_s"]) is not int: errors.append("record.wait.eta_s: expected integer or null")
    now=record.get("now")
    if obj(now,"record.now",{"kind"},{"kind","text"}):
        if now.get("kind") not in ("worker","wait","stopped","waiting_on_you","done_here"): errors.append("record.now.kind: invalid enum")
        if "text" in now and now["text"] is not None and not isinstance(now["text"],str): errors.append("record.now.text: expected string or null")
    verified=record.get("verified")
    if verified is not None and obj(verified,"record.verified",{"text"},{"text"}) and (not isinstance(verified.get("text"),str) or not verified["text"]): errors.append("record.verified.text: expected nonempty string")
    return errors
