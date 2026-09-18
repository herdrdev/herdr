"""Portable case catalogue and strict evidence verdicts for the local Windows gauntlet.

This is not a host simulator. A report cannot turn absent Windows evidence into a pass.
"""
import argparse
import json
import re
from pathlib import Path

WIDTHS = [80, 119, 120, 121, 132, 160, 240]
HEIGHTS = [24, 50]
MODES = ["native", "legacy", "mok2", "kitty"]


def hex_of(text):
    return text.encode("utf-8").hex()


def catalogue():
    cases = []

    def key(name, vk, text, modifiers=(), enhanced=None, legacy=None):
        mask = (1 if 16 in modifiers else 0) | (2 if 18 in modifiers else 0) | (4 if 17 in modifiers else 0)
        unicode = (vk + 32 if 65 <= vk <= 90 else vk if vk in (8, 9, 13, 27) else 0)
        if 65 <= vk <= 90 and 16 in modifiers:
            unicode = vk
        if 17 in modifiers:
            unicode = vk - 64 if 65 <= vk <= 90 else 10 if vk == 13 else 127 if vk == 8 else 0
        expected = {"native": {"vk": vk, "modifiers": mask, "unicode": unicode, "modifier_keys": list(modifiers)}}
        if text is not None:
            expected.update({mode: {"hex": [hex_of(text)]} for mode in MODES[1:]})
        if legacy is not None:
            expected["legacy"] = {"hex": [hex_of(legacy)]}
        if enhanced is not None:
            expected["mok2"] = {"hex": [hex_of(f"\x1b[27;{mask + 1};{enhanced}~")]}
            expected["kitty"] = {"hex": [hex_of(f"\x1b[{enhanced};{mask + 1}u")]}
        cases.append(dict(id=name, kind="key", chords=[[*modifiers, vk]], expected=expected))

    key("letter-a", 65, "a")
    key("shift-letter", 65, "A", (16,))
    key("enter", 13, "\r")
    key("shift-enter", 13, None, (16,), 13)
    cases[-1]["expected"]["legacy"] = {"loss": "enter", "hex": ["0d"]}
    key("ctrl-enter", 13, None, (17,), 13)
    cases[-1]["expected"]["legacy"] = {"loss": "modifier", "hex": ["0a"]}
    key("ctrl-shift-enter", 13, None, (17, 16), 13)
    cases[-1]["expected"]["legacy"] = {"loss": "modifier", "hex": ["0a"]}
    key("tab", 9, "\t")
    key("shift-tab", 9, "\x1b[Z", (16,))
    cases[-1]["expected"]["mok2"] = {"hex": [hex_of("\x1b[27;2;9~")]}
    cases[-1]["expected"]["kitty"]["hex"].append(hex_of("\x1b[9;2u"))
    key("backspace", 8, "\x7f")
    key("ctrl-backspace", 8, None, (17,), 127, "\x08")
    key("alt-backspace", 8, None, (18,), 127, "\x1b\x7f")
    key("escape", 27, "\x1b")
    # Kitty's disambiguation flag encodes Escape distinctly.
    cases[-1]["expected"]["kitty"] = {"hex": [hex_of("\x1b[27u")]}
    for name, vk, seq, kitty in [("up", 38, "A", 57419), ("down", 40, "B", 57420), ("right", 39, "C", 57418),
                                 ("left", 37, "D", 57417), ("home", 36, "H", 57423), ("end", 35, "F", 57424)]:
        for prefix, modifiers, modifier in [("", (), 1), ("shift-", (16,), 2), ("ctrl-", (17,), 5), ("ctrl-shift-", (17, 16), 6)]:
            if prefix == "ctrl-shift-" and name in ("up", "down", "home", "end"):
                continue
            key(prefix + name, vk, "\x1b[" + ("" if modifier == 1 else f"1;{modifier}") + seq, modifiers)
            cases[-1]["expected"]["kitty"]["hex"].append(hex_of(f"\x1b[{kitty}{'' if modifier == 1 else f';{modifier}'}u"))
    for name, vk, code, kitty in [("insert", 45, 2, 57425), ("delete", 46, 3, 57426),
                                  ("page-up", 33, 5, 57421), ("page-down", 34, 6, 57422)]:
        key(name, vk, f"\x1b[{code}~")
        cases[-1]["expected"]["kitty"]["hex"].append(hex_of(f"\x1b[{kitty}u"))
    for letter in "acdj lmqsu z".replace(" ", ""):
        key("ctrl-" + letter, ord(letter.upper()), None, (17,), ord(letter), chr(ord(letter) - 96))
    key("alt-v", 86, None, (18,), ord("v"), "\x1bv")
    for name, text in [
        ("paste-lf", "line 1\nline 2"),
        ("paste-crlf", "line 1\r\nline 2\r\n"),
        ("paste-cr", "line 1\rline 2"),
        ("paste-whitespace", "  one\t\n\n two  \n"),
        ("paste-unicode", "é e\u0301 日本語\nnext"),
        ("paste-escape-looking", "literal [200~ and \\x1b[31m\nend"),
    ]:
        cases.append(dict(id=name, kind="paste", text=text, expected={mode: {"paste": text} for mode in MODES[1:]}))
    cases.append(dict(id="clipboard-image", kind="clipboard-image",
                      expected={mode: {"clipboard_image": True,
                                       "sha256": "4BA8D4FD5AB42544FEEFF22D50E84412502433CC95952FD1DF9A5293588DDBEA"}
                                for mode in MODES[1:]}))
    cases.append(dict(id="clipboard-mixed", kind="clipboard-mixed", text="clipboard text wins",
                      expected={mode: {"paste": "clipboard text wins"} for mode in MODES[1:]}))
    cases.append(dict(id="mouse-interleave", kind="mouse-interleave", text="mouse\npaste",
                      expected={mode: {"mouse_interleave": True} for mode in MODES[1:]}))
    cases.append(dict(id="mouse-focus-refresh", kind="mouse-focus-refresh",
                      expected={mode: {"mouse_focus_refresh": True} for mode in MODES[1:]}))
    transitions = "a\r" + "b\x1b[27;2;13~" + "c\x1b[13;2u" + "d\x1b[27;2;13~" + "e\r" + "f"
    cases.append(dict(id="mode-transitions", kind="mode-transitions",
                      expected={mode: {"hex": [hex_of(transitions)]} for mode in MODES[1:]}))
    cases.append(dict(id="dead-acute", kind="layout-key", dead="´", base_vk=69,
                      expected={mode: {"hex": [hex_of("é")]} for mode in MODES[1:]}))
    for name, prompt, text in [
        ("dead-grave", "With US-International active, type grave then e, once; no Enter.", "è"),
        ("dead-circumflex", "With US-International active, type circumflex then e, once; no Enter.", "ê"),
        ("dead-tilde", "With US-International active, type tilde then n, once; no Enter.", "ñ"),
        ("dead-diaeresis", "With US-International active, type diaeresis then u, once; no Enter.", "ü"),
        ("dead-space", "With US-International active, type acute then Space, once; no Enter.", "'"),
        ("altgr-euro", "With your declared euro-producing AltGr layout, type € using its AltGr chord, once; no Enter.", "€"),
        ("ime-commit", "Using your declared IME, compose and commit 日本語 (not paste); do not submit the prompt.", "日本語"),
    ]:
        cases.append(dict(id=name, kind="manual", prompt=prompt, expected={mode: {"hex": [hex_of(text)]} for mode in MODES[1:]}))
    # Explicit qualification tasks, never auto-passed by key/byte checks.
    for name, prompt in [
        ("dead-cancel-repeat", "Qualify dead-key cancellation, repeated accent and non-composing next character against the direct-host baseline."),
        ("ime-cancel", "Qualify IME cancel and partial composition without committed/duplicate text."),
        ("native-repeat-release", "Qualify held-key repeat, release identity, and modifier interleaving using native records."),
        ("locks-keypad", "Qualify CapsLock/NumLock, keypad Enter/operators/decimal and restore lock states."),
        ("mouse-right-edge", "Click/drag/wheel at a marked pane column above 120; compare actual report coordinates and visual target."),
        ("wrap-rendering", "Verify the displayed ruler and wrapped multiline text at both window heights and every observed width."),
        ("capture-refresh", "Toggle mouse capture/config reload, refocus, detach/reattach; repeat paste and Shift+Enter."),
        ("setup-recovery", "Inject a recoverable setup failure and late VT activation; verify mode restoration and the same input sentinels."),
        ("ctrl-v", "Qualify raw Ctrl+V only with the Windows Terminal paste binding explicitly removed."),
        ("alt-enter", "Qualify Alt+Enter with the Windows Terminal fullscreen binding explicitly controlled."),
        ("ctrl-shift-up", "Qualify Ctrl+Shift+Up with the Windows Terminal scroll binding explicitly controlled."),
        ("ctrl-shift-down", "Qualify Ctrl+Shift+Down with the Windows Terminal scroll binding explicitly controlled."),
        ("ctrl-shift-home", "Qualify Ctrl+Shift+Home with the Windows Terminal scroll binding explicitly controlled."),
        ("ctrl-shift-end", "Qualify Ctrl+Shift+End with the Windows Terminal scroll binding explicitly controlled."),
        ("paste-supplementary", "Qualify supplementary-plane clipboard text, including emoji, against the direct-host baseline."),
        ("paste-burst", "Qualify a 200-line clipboard burst with the host multiline-paste warning configured or handled explicitly."),
    ]:
        cases.append(dict(id=name, kind="qualification", prompt=prompt, expected={}))
    return dict(schema=1, widths=WIDTHS, heights=HEIGHTS, modes=MODES, cases=cases)


def verdict(case, mode, evidence):
    """The full capture, not a matching prefix, is the primary assertion."""
    if evidence.get("status") in {"not_run", "unsupported", "inconclusive"}:
        return evidence["status"], evidence.get("reason", "No qualifying observation")
    expected = case["expected"].get(mode)
    if expected is None:
        return "not_run", "No automatic oracle for this case/profile; requires qualification"
    if not evidence.get("ready") or not evidence.get("focus_verified") or not evidence.get("complete"):
        return "inconclusive", "Missing readiness, focus, or complete capture"
    if evidence.get("error"):
        return "inconclusive", evidence["error"]
    if case["kind"] == "clipboard-image" and evidence.get("path") == "herdr":
        return "not_run", "Clipboard image bridging is active only for remote clients"
    if evidence.get("path") == "herdr" and case["id"] in ("page-up", "page-down") and "vk" not in expected:
        expected = {"hex": [""]}  # Plain page keys intentionally control Herdr's host scrollback.
    def geometry(name):
        value = evidence.get(name)
        return value[:2] if isinstance(value, list) and len(value) >= 2 and all(type(v) is int for v in value[:2]) else None
    outer, pane = geometry("outer_geometry"), geometry("pane_geometry")
    final_pane, final_outer = geometry("final_pane_geometry"), geometry("final_outer_geometry")
    if outer != [evidence.get("width"), evidence.get("height")]:
        return "inconclusive", "Requested geometry was not observed"
    if pane is None or min(pane) <= 0:
        return "inconclusive", "Missing actual pane dimensions"
    if final_pane != pane:
        return "inconclusive", "Pane geometry changed during capture"
    if final_outer != outer:
        return "inconclusive", "Outer geometry changed during capture"
    if evidence.get("path") == "herdr" and case["id"] in ("page-up", "page-down") and "vk" in expected:
        records = evidence.get("records")
        if not isinstance(records, list) or any(not isinstance(record, list) or len(record) != 7
                                                or not all(type(field) is int for field in record) for record in records):
            return "inconclusive", "Malformed native record"
        return (("inconclusive", "No positive host-scrollback evidence for the consumed page key")
                if all(record[0] in (4, 16) for record in records)
                else ("fail", "Plain page key unexpectedly reached the pane"))
    if (evidence.get("path") == "herdr" and case["id"] in ("page-up", "page-down")
            and evidence.get("hex") == ""):
        return "inconclusive", "No positive host-scrollback evidence for the consumed page key"
    if "vk" in expected:
        records = evidence.get("records")
        scans = evidence.get("scans", [])
        chord = case["chords"][0]
        if not isinstance(records, list):
            return "inconclusive", "Malformed native record"
        if not isinstance(scans, list) or len(scans) != len(chord) or not all(type(scan) is int for scan in scans):
            return "inconclusive", "Missing injected scan-code evidence"
        expected_scans = dict(zip(chord, scans))
        aliases = {160: 16, 161: 16, 162: 17, 163: 17, 164: 18, 165: 18}
        held = set()
        keys = []
        for record in records:
            if not isinstance(record, list) or len(record) != 7 or not all(type(field) is int for field in record):
                return "inconclusive", "Malformed native record"
            if record[0] in (4, 16):  # Resize/focus notifications carry no typed text.
                continue
            if record[0] != 1:
                return "fail", "Unexpected non-key native input"
            vk = aliases.get(record[3], record[3])
            if vk not in (16, 17, 18):
                keys.append(record)
                continue
            if vk not in expected["modifier_keys"] or record[5] != 0 or record[2] != 1 or record[4] != expected_scans[vk]:
                return "fail", "Unexpected/corrupted modifier record"
            if record[1] == 1 and vk not in held:
                held.add(vk)
            elif record[1] == 0 and vk in held:
                held.remove(vk)
            else:
                return "fail", "Unbalanced modifier sequence"
        if held:
            return "fail", "Modifier release missing"
        if len(keys) != 2 or [r[1] for r in keys] != [1, 0]:
            return "fail", "Expected exactly one non-modifier down/up pair"
        for record in keys:
            control = record[6]
            modifiers = bool(control & 16) + 2 * bool(control & 3) + 4 * bool(control & 12)
            if (record[3] != expected["vk"] or modifiers != expected["modifiers"] or record[2] != 1
                    or record[5] != expected["unicode"] or record[4] != expected_scans[expected["vk"]]):
                return "fail", "Native identity/modifiers/repeat differ"
        return "pass", "Native down/up pair matches"
    try:
        raw = bytes.fromhex(evidence["hex"])
    except (KeyError, ValueError, TypeError):
        return "inconclusive", "Missing or malformed raw bytes"
    if expected.get("clipboard_image"):
        if evidence.get("path") == "direct":
            return (("pass", "Terminal emitted an empty bracketed paste for image-only clipboard")
                    if raw == b"\x1b[200~\x1b[201~" else
                    ("fail", "Terminal did not emit an empty bracketed paste for image-only clipboard"))
        if evidence.get("path") != "herdr-remote":
            return "not_run", "Clipboard image bridge requires the remote-client gauntlet path"
        if not raw.startswith(b"\x1b[200~") or not raw.endswith(b"\x1b[201~"):
            return "fail", "Remote clipboard image did not reach the pane as one paste"
        try:
            path = raw[6:-6].decode("utf-8")
        except UnicodeDecodeError:
            return "fail", "Staged clipboard image path is not UTF-8"
        valid_path = re.fullmatch(r"[A-Za-z]:\\.*\\herdr-clipboard-images-[^\\]+\\[^\\]+\.png", path)
        if not valid_path:
            return "fail", "Pane did not receive a staged clipboard PNG path"
        return (("pass", "Exact clipboard PNG was staged and its path reached the pane")
                if evidence.get("staged_image_sha256") == expected["sha256"] else
                ("fail", "Staged clipboard image contents differ from the fixture"))
    if expected.get("mouse_interleave"):
        motion = rb"(?:\x1b\[<35;\d+;\d+M)+"
        newline = rb"(?:\r\n|\r|\n)"
        pattern = b"a" + motion + rb"\x1b\[200~mouse" + newline + rb"paste\x1b\[201~" + motion + b"b"
        return ("pass", "Typing, mouse motion and paste remained ordered") if re.fullmatch(pattern, raw) else ("fail", "Mouse interleave order or payload differs")
    if expected.get("mouse_focus_refresh"):
        coords = rb"\d+;\d+"
        gesture = (rb"(?:\x1b\[<35;" + coords + rb"M)*"
                   + rb"\x1b\[<0;" + coords + rb"M"
                   + rb"\x1b\[<0;" + coords + rb"m"
                   + rb"\x1b\[<64;" + coords + rb"M")
        pattern = b"a" + gesture + b"b" + b"c" + gesture + b"d"
        return ("pass", "Click/wheel reporting survived focus loss and regain") if re.fullmatch(pattern, raw) else ("fail", "Mouse reporting failed before or after focus regain")
    if case["id"] == "mode-transitions" and evidence.get("path") == "direct":
        legacy_only = hex_of("a\rb\rc\rd\re\rf")
        kitty_only = hex_of("a\rb\rc\x1b[13;2u" + "d\re\rf")
        if raw.hex() in {legacy_only, kitty_only}:
            return "unsupported", "Direct Windows Terminal ignored modifyOtherKeys during the transition chain"
        return "fail", "Unexpected direct-host runtime transition bytes"
    if "loss" in expected:
        return ("unsupported", "Plain VT cannot preserve modified Enter") if raw.hex() in expected["hex"] else ("fail", "Unexpected plain-VT modified Enter result")
    if "hex" in expected:
        return ("pass", "Exact bytes match") if raw.hex() in expected["hex"] else ("fail", "Bytes differ (including any duplicates/trailing input)")
    if not raw.startswith(b"\x1b[200~") or not raw.endswith(b"\x1b[201~"):
        return "fail", "Missing bracketed-paste envelope; newlines may be key events"
    try:
        actual = raw[6:-6].decode("utf-8")
    except UnicodeDecodeError:
        return "fail", "Paste is not intact UTF-8"
    # Line-ending conversion is expected on Windows; all other payload bytes matter.
    normalize = lambda text: text.replace("\r\n", "\n").replace("\r", "\n")
    return ("pass", "One complete paste matches") if normalize(actual) == normalize(expected["paste"]) else ("fail", "Paste payload differs")


def known_host_gap(observation, case, terminal_version):
    """A narrowly observed host limitation, never a blanket version exemption."""
    legacy = case["expected"].get("legacy", {}).get("hex", [])
    mok2 = case["expected"].get("mok2", {}).get("hex", [])
    return (observation.get("path") == "direct"
            and observation.get("mode") == "mok2"
            and case["id"] in {"shift-enter", "ctrl-enter", "ctrl-shift-enter", "shift-tab"}
            and re.match(r"^1\.(?:24|25)\.", str(terminal_version)) and set(legacy) != set(mok2)
            and observation.get("hex") in legacy)


def channel_identity_errors(hosts):
    """Compare both launcher identities and the processes actually activated."""
    identities = {}
    for host in hosts:
        values = identities.setdefault(host.get("channel"), set())
        for field, kind in (("launcher_identity", "file"), ("installation_identity", "installation")):
            if host.get(field):
                values.add((kind, host[field]))
        for run in host.get("runs", []):
            for field, kind in (("image_identity", "file"), ("installation_identity", "installation"), ("process_identity", "process")):
                if run.get(field):
                    values.add((kind, run[field]))
    return ["Stable and Preview share a Terminal executable, installation, or process identity"] if identities.get("stable", set()) & identities.get("preview", set()) else []


def summarize(document):
    matrix = catalogue()
    cases = {case["id"]: case for case in matrix["cases"]}
    rows = []
    seen = set()
    captures = set()
    for observation in document.get("observations", []):
        identity = tuple(observation.get(k) for k in ("host", "path", "mode", "phase", "width", "height", "case"))
        if identity in seen:
            raise ValueError(f"Duplicate observation identity: {identity}")
        seen.add(identity)
        case = cases[observation["case"]]
        status, reason = verdict(case, observation["mode"], observation)
        scope = "direct_host" if observation.get("path") == "direct" else "through_herdr_not_yet_attributed"
        if status in ("pass", "fail"):
            bound = [run for host in document.get("hosts", []) if host.get("channel") == observation.get("host")
                     for run in host.get("runs", [])
                     if run.get("nonce") == observation.get("nonce") and run.get("path") == observation.get("path")
                     and run.get("mode") == observation.get("mode") and run.get("pid", 0) > 0 and run.get("hwnd", 0) != 0
                     and run.get("elevated") is False and run.get("image_identity") and run.get("installation_identity")]
            capture = observation.get("capture_id")
            if len(bound) != 1 or document.get("controller_elevated") is not False or not capture or capture in captures:
                status, reason = "inconclusive", "Missing non-elevated owned-run binding or fresh capture identity"
            else:
                captures.add(capture)
                if status == "fail" and known_host_gap(observation, case, bound[0].get("terminal_version")):
                    status, reason = "unsupported", "Direct host ignored mOK and emitted the case's legacy bytes"
        rows.append({**observation, "status": status, "reason": reason, "failure_scope": scope})
    counts = {status: sum(r["status"] == status for r in rows) for status in ("pass", "fail", "not_run", "unsupported", "inconclusive")}
    # No run may claim all-green just because it produced zero/missing observations.
    planned = set()
    geometries = [(120, 30, True)] + [(w, h, False) for h in document.get("heights", HEIGHTS) for w in document.get("widths", WIDTHS)] + [(80, 30, False)]
    selected_cases = document.get("cases") or list(cases)
    for host in document.get("channels") or ("stable", "preview"):
        for path in document.get("paths") or ("direct", "herdr"):
            for mode in document.get("modes", MODES):
                for phase, (width, height, full) in enumerate(geometries, 1):
                    for case_id in selected_cases if full else (case_id for case_id in ("letter-a", "shift-enter", "paste-lf", "mouse-focus-refresh") if case_id in selected_cases):
                        planned.add((host, path, mode, phase, width, height, case_id))
    missing = planned - seen
    if seen - planned:
        raise ValueError("Observations outside the declared run matrix")
    hosts = document.get("hosts", [])
    errors = list(document.get("errors", [])) + channel_identity_errors(hosts)
    expected_hosts = set(document.get("channels") or ("stable", "preview"))
    complete = (bool(rows) and not missing and {h.get("channel") for h in hosts} == expected_hosts
                and all(h.get("runs") for h in hosts)
                and not errors and not document.get("cleanup_errors")
                and all(r["status"] == "pass" for r in rows))
    return {**document, "errors": errors, "observations": rows, "counts": counts, "coverage_missing": len(missing), "observed_checks_passed": complete,
            "native_qualification": "Required; this report is not a full Windows support certificate"}


def qualification_matrix(result):
    """Collapse real observations into the user-facing capability summary."""
    rows = result.get("observations", [])
    required_channels = set(result.get("channels") or ("stable", "preview"))
    manual_cases = {case["id"] for case in catalogue()["cases"] if case["kind"] == "manual"}
    groups = [
        ("Printable keys", {"letter-a", "shift-letter"}, None),
        ("Shift+Enter", {"shift-enter"}, None),
        ("Ctrl+Enter", {"ctrl-enter"}, None),
        ("Ctrl+Shift+Enter", {"ctrl-shift-enter"}, None),
        ("Navigation/editing", {"up", "down", "left", "right", "home", "end", "insert", "delete", "tab", "shift-tab", "page-up", "page-down"}, None),
        ("Multiline paste", {"paste-lf"}, None),
        ("CR/LF/CRLF paste", {"paste-lf", "paste-crlf", "paste-cr"}, None),
        ("Unicode/whitespace paste", {"paste-unicode", "paste-whitespace"}, None),
        ("Paste framing/ordering", {case["id"] for case in catalogue()["cases"] if case["kind"] == "paste"}, None),
        ("Remote clipboard image", {"clipboard-image", "clipboard-mixed"}, None),
        ("Resize 120 -> 80", {"letter-a", "shift-enter", "paste-lf"}, 80),
        ("Mouse while typing/pasting", {"mouse-interleave"}, None),
        ("Mouse after focus regain", {"mouse-focus-refresh"}, None),
        ("Mouse after resize", {"mouse-focus-refresh"}, 80),
        ("Dead-key composition", {"dead-acute"}, None),
        ("AltGr", {"altgr-euro"}, None),
        ("IME composition", {"ime-commit"}, None),
        ("Runtime mode transitions", {"mode-transitions"}, None),
    ]

    def cell(case_ids, width, path, modes):
        matched = [row for row in rows if row.get("case") in case_ids and row.get("path") == path
                   and row.get("mode") in modes and (width is None or row.get("width") == width)]
        statuses = {row.get("status") for row in matched}
        if "fail" in statuses:
            return "FAIL"
        if case_ids <= manual_cases and not any(status == "pass" for status in statuses):
            return "MANUAL"
        if {row.get("case") for row in matched} != case_ids:
            return "PARTIAL" if "pass" in statuses else "INCONCLUSIVE" if "inconclusive" in statuses else "NOT TESTED"
        if required_channels and {(row.get("host"), row.get("case")) for row in matched} != {
                (channel, case_id) for channel in required_channels for case_id in case_ids}:
            return "PARTIAL"
        if case_ids == {"mode-transitions"} and path == "direct" and statuses <= {"unsupported", "inconclusive"}:
            return "X - mOK ignored"
        if path == "direct" and modes == {"legacy"} and statuses == {"unsupported"}:
            return "X - becomes Enter" if case_ids == {"shift-enter"} else "X - loses modifier"
        def case_passed(case_id):
            statuses_for_case = {row.get("status") for row in matched if row.get("case") == case_id}
            return "pass" in statuses_for_case or (width == 80 and path == "direct" and modes == {"legacy"}
                                                     and case_id == "shift-enter" and statuses_for_case == {"unsupported"})
        per_case_passed = all(case_passed(case_id) for case_id in case_ids)
        allowed = {"pass", "inconclusive"}
        if width == 80 and path == "direct" and modes == {"legacy"}:
            allowed.add("unsupported")
        if per_case_passed and statuses <= allowed:
            return "PASS**" if "inconclusive" in statuses else "PASS"
        if "inconclusive" in statuses:
            return "INCONCLUSIVE"
        if "unsupported" in statuses:
            return "UNSUPPORTED"
        return "NOT TESTED"

    table = []
    for name, case_ids, width in groups:
        herdr_modes = {"legacy"} if name in {"Mouse after resize", "Runtime mode transitions"} else {"mok2"} if "Enter" in name or name in {"Resize 120 -> 80", "Dead-key composition", "AltGr", "IME composition"} else {"legacy"}
        herdr_path = "herdr-remote" if name == "Remote clipboard image" else "herdr"
        table.append((name, cell(case_ids, width, herdr_path, herdr_modes),
                      cell(case_ids, width, "direct", {"legacy"}), cell(case_ids, width, "direct", {"kitty"})))
    return table


def herdr_protocol_label(result):
    runs = {(host.get("channel"), run.get("path"), run.get("mode"), run.get("nonce"))
            for host in result.get("hosts", []) for run in host.get("runs", [])
            if run.get("path") in {"herdr", "herdr-remote"} and run.get("nonce")}
    proven = {(row.get("host"), row.get("path"), row.get("mode"), row.get("nonce"))
              for row in result.get("observations", [])
              if row.get("path") in {"herdr", "herdr-remote"} and row.get("input_reader") == "windows-console"
              and row.get("input_transport") == "win32-serialized" and row.get("nonce")}
    return "Win32 (Herdr)*" if runs and runs <= proven else "Herdr default (UNKNOWN)*"


def print_qualification_matrix(result):
    table = [("Thing", herdr_protocol_label(result), "Plain VT", "Kitty"), *qualification_matrix(result)]
    widths = [max(len(str(row[column])) for row in table) for column in range(4)]
    line = lambda row: " | ".join(str(value).ljust(widths[index]) for index, value in enumerate(row))
    print("\nWindows input qualification results")
    print(line(table[0]))
    print("-+-".join("-" * width for width in widths))
    for row in table[1:]:
        print(line(row))
    print("* Herdr protocol label comes from runtime evidence; UNKNOWN is never treated as Win32.")
    print("** At least one capable host passed; host capability gaps remain visible in report.json.")
    print("MANUAL requires an operator-assisted -Manual run; no automated result is claimed.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["matrix", "report"])
    parser.add_argument("--input", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = catalogue() if args.command == "matrix" else summarize(json.loads(args.input.read_text(encoding="utf-8-sig")))
    args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    if args.command == "report":
        counts = result["counts"]
        through_failures = sum(row["status"] == "fail" and row.get("path") != "direct" for row in result["observations"])
        direct_failures = sum(row["status"] == "fail" and row.get("path") == "direct" for row in result["observations"])
        print(f"Observed: {counts['pass']} pass, {counts['fail']} fail, {counts['unsupported']} unsupported, "
              f"{counts['inconclusive']} inconclusive, {counts['not_run']} not run; "
              f"{result['coverage_missing']} planned rows missing")
        if through_failures:
            print(f"Assessment: {through_failures} through-Herdr failures need attribution; inspect report.json before treating them as product regressions")
        elif result.get("errors") or result.get("cleanup_errors"):
            print("Assessment: harness or cleanup failed; this run does not qualify input behavior")
        elif direct_failures:
            print(f"Assessment: {direct_failures} direct-host differences observed; these are not automatically Herdr bugs")
        elif counts["inconclusive"] or counts["not_run"] or counts["unsupported"] or result["coverage_missing"]:
            print("Assessment: observed assertions passed, but host limitations or qualification gaps remain")
        else:
            print("Assessment: all planned observed assertions passed")
        if result.get("errors"):
            print("Run errors: " + " | ".join(result["errors"]))
        if result.get("cleanup_errors"):
            print("Cleanup errors: " + " | ".join(result["cleanup_errors"]))
        print_qualification_matrix(result)
        return 1 if result["counts"]["fail"] or result.get("errors") or result.get("cleanup_errors") else 2 if not result["observed_checks_passed"] else 0
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
