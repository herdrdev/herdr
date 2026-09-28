"""Parse the existing closing block into herdr status channels.

Critical action points retain their Gate/Answer/Verify labels and every retained
item is a pending human decision. Task completion, active workers, external waits,
and parse quality remain separate facts.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any

# HERDR_INTEGRATION_VERSION=3
HUMAN_INPUT_LABELS = ("Gate", "Answer", "Verify", "Approve", "Decide")
GATE_LABELS = ("Gate", "Approve")
# Label for a human-input item whose own label word is unknown or missing.
FALLBACK_LABEL = "Decide"

_HUMAN_INPUT_LABEL_PATTERN = "|".join(
    re.escape(label) for label in HUMAN_INPUT_LABELS
)

# The header line alone is the trigger: agents drop bold markers, add a
# heading marker or a colon, or vary the case often enough that strictness
# here silently unlatches pending input. Only a full-line heading matches.
_HEADER_RE = re.compile(
    r"^(?:#{1,6}[ \t]*)?(?:\*\*)?(?:"
    r"Critical action points(?:[ \t]*\((?P<critical_n>\d+)[ \t]+blocking\))?"
    r"(?:\*\*)?(?(critical_n)(?:[ \t]*[—–:-].*)?)"
    r"|Needs you[ \t]*\((?P<needs_n>\d+)(?:[ \t]+blocking)?\)"
    r"(?:\*\*)?(?:[ \t]*[—–:-].*)?"
    r")[ \t]*:?[ \t]*\r?$",
    re.MULTILINE | re.IGNORECASE,
)
_NOTHING_RE = re.compile(
    r"^(?:\*\*)?(?:Nothing to act on|Needs you:[ \t]*nothing)\.?(?:\*\*)?[ \t]*\r?$",
    re.MULTILINE | re.IGNORECASE,
)
_AGENTS_RE = re.compile(
    r"^(?:\*\*)?(?P<n>\d+)[ \t]+agents?[ \t]+running:[ \t]*(?P<rest>.+?)"
    r"(?=(?:[ \t]+·[ \t]+(?:Waiting on you\b|Waiting(?:[ \t]+for|[ \t]*:)))|"
    r"(?:\*\*)?[ \t]*\r?$)",
    re.MULTILINE | re.IGNORECASE,
)
_DONE_RE = re.compile(
    r"^(?:\*\*)?Done here\.(?:\*\*)?[ \t]*\r?$", re.MULTILINE | re.IGNORECASE
)
_NOW_RE = re.compile(
    r"^(?:\*\*)?Now:(?:\*\*)?[ \t]*(?P<rest>.*?)(?:\*\*)?[ \t]*\r?$",
    re.MULTILINE | re.IGNORECASE,
)
_ANSWER_INSTRUCTION_RE = re.compile(
    r"^(?:Reply|Answer)[ \t]+\d+[a-z]?[ \t]*/[ \t]*\d+[a-z]?\."
    r"[ \t]*Silence holds\.[ \t]*\r?$",
    re.MULTILINE | re.IGNORECASE,
)
_WAITING_ON_YOU_RE = re.compile(
    r"^(?:(?:\*\*)?\d+[ \t]+agents?[ \t]+running:.*?[ \t]+·[ \t]+)?"
    r"(?:Waiting(?:[ \t]+for|[ \t]*:)[ \t]+.*?"
    r"(?:[ \t]+·[ \t]+|[.;][ \t]+))?"
    r"(?:\*\*)?Waiting on you[ \t]*[—–:-][ \t]*(?P<rest>.+?)(?:\*\*)?[ \t]*\r?$",
    re.MULTILINE | re.IGNORECASE,
)
_EXTERNAL_WAIT_RE = re.compile(
    r"^(?:(?:\*\*)?\d+[ \t]+agents?[ \t]+running:.*?[ \t]+·[ \t]+)?"
    r"(?:\*\*)?Waiting(?:[ \t]+for|[ \t]*:)[ \t]+(?P<rest>.+?)"
    r"(?=(?:(?:[ \t]+·[ \t]+|[.;][ \t]+)Waiting on you\b)|"
    r"(?:\*\*)?[ \t]*\r?$)",
    re.MULTILINE | re.IGNORECASE,
)
_CONTRACT_RE = re.compile(
    r"^CONTRACT:[ \t]+(?P<text>.+?)[ \t]+—[ \t]+(?P<state>met|unmet)[ \t]*\r?$",
    re.MULTILINE | re.IGNORECASE,
)
_FENCE_OPEN_RE = re.compile(
    r"^[ \t]{0,3}(?P<marker>`{3,}|~{3,})(?P<info>.*)$"
)
_FENCE_CLOSE_RE = re.compile(r"^[ \t]{0,3}(?P<marker>`{3,}|~{3,})[ \t]*$")

# A label may open the numbered item or appear on a later top-level line after a
# URL and its plain-language summary. Plain labels require a separator so prose
# such as `Verify the deploy` cannot be half-eaten. Legacy `· non-blocking`
# spelling is accepted only as presentation; it no longer changes semantics.
# When the terminator was stricter, a real item it failed to recognise was
# swallowed into the previous item's body instead: `2)**Gate**` and an indented
# `  2. **Gate**` both vanished into an `Answer` above them, taking a declared
# gate with them and leaving nothing discarded for the count to notice.
#
# The trailing lookahead keeps a decimal at the start of a body line (`1.5x
# faster`) from reading as an item marker, which the old `[ \t]` requirement
# had been doing incidentally.
_ITEM_START = r"^[ \t]*\d+[.)](?=[ \t]|\*\*|$)"
_ITEM_RE = re.compile(
    rf"^[ \t]*(?P<idx>\d+)[.)]\s*(?P<body>.*?)(?={_ITEM_START}|^\*\*What to test\b|"
    r"^\*\*Auto-proceeded decisions\*\*|^\d+\s+agents?\s+running:|"
    r"^(?:\*\*)?Waiting on you\b|^(?:\*\*)?Waiting(?:[ \t]+for|[ \t]*:)|"
    r"^(?:\*\*)?Done here\.|^(?:\*\*)?Now:|"
    r"^(?:Reply|Answer)[ \t]+\d+[a-z]?[ \t]*/[ \t]*\d+[a-z]?\."
    r"[ \t]*Silence holds\.|\Z)",
    re.MULTILINE | re.IGNORECASE | re.DOTALL,
)
_ITEM_LABEL_RE = re.compile(
    r"^[ \t]*(?:"
    rf"\*\*(?P<bold_label>{_HUMAN_INPUT_LABEL_PATTERN})"
    r"(?:[ \t]*·[ \t]*non-blocking)?\*\*"
    r"(?:[ \t]*·[ \t]*non-blocking)?[ \t]*(?:[—–:]|-[ \t])?[ \t]*"
    rf"|(?P<plain_label>{_HUMAN_INPUT_LABEL_PATTERN})"
    r"(?:[ \t]*·[ \t]*non-blocking)?[ \t]*(?:[—–:]|-[ \t])[ \t]*"
    # Any other bold word used as a label. The label vocabulary changes with
    # the policy, so an unknown word must still read as a human decision.
    r"|\*\*(?P<other_label>[A-Za-z][A-Za-z-]{1,23})\*\*[ \t]*(?:[—–:]|-[ \t])[ \t]*"
    r")",
    re.MULTILINE | re.IGNORECASE,
)
_LEGACY_NONBLOCKING_SUFFIX_RE = re.compile(
    r"(?s)(?P<text>.*?)\s*·\s*non-blocking\b\s*[.]*\s*$", re.IGNORECASE
)
_DECISIONS_RE = re.compile(
    r"^\*\*Auto-proceeded decisions\*\*[ \t]*\r?$",
    re.MULTILINE | re.IGNORECASE,
)
_DECISION_ITEM_RE = re.compile(
    r"^(?P<idx>\d+)[.)]\s*(?P<body>.*?)(?=^\d+[.)]\s+|"
    r"^\*\*What to test\b|^\d+\s+agents?\s+running:|"
    r"^Done here\.|^(?:\*\*)?Now:|\Z)",
    re.MULTILINE | re.DOTALL,
)
_WHAT_RE = re.compile(
    r"^\*\*What to test(?P<meta>.*?)\*\*[ \t]*\r?$",
    re.MULTILINE | re.IGNORECASE,
)
_PR_RE = re.compile(r"(?<![A-Za-z0-9])#(?P<pr>\d+)\b")
_TICKET_RE = re.compile(r"\b[A-Z][A-Z0-9]+-\d+\b")
_URL_RE = re.compile(r"https?://[^\s\]>]+", re.IGNORECASE)
_RECOMMENDATION_RE = re.compile(
    r"\b(?:recommend(?:ation|ed)?|proceed(?:ed)?\s+with)\s*[:\-]?\s*"
    r"(?P<value>.+?)(?=\s+(?:because|at\s+\d{1,2}:\d{2}|decided\s+at)|$)",
    re.IGNORECASE,
)
_DECIDED_AT_RE = re.compile(
    r"\b(?:decided\s+at|at)\s+(?P<value>\d{1,2}:\d{2}(?:\s*[A-Z]{2})?)",
    re.IGNORECASE,
)


def _clean_body(body: str) -> str:
    # Remove only the Markdown list separator. Keep the item wording and
    # internal whitespace verbatim.
    return body.strip().lstrip("-—–:").strip()


def _declared_count(header: re.Match[str]) -> int | None:
    value = header.group("critical_n") or header.group("needs_n")
    return int(value) if value is not None else None


def _is_human_input_label(label: str) -> bool:
    return label in HUMAN_INPUT_LABELS


def _canonical_human_input_label(label: str) -> str:
    return next(
        (candidate for candidate in HUMAN_INPUT_LABELS if candidate.casefold() == label.casefold()),
        "",
    )


def _metadata(text: str) -> dict[str, Any]:
    url_match = _URL_RE.search(text)
    url = url_match.group(0).rstrip(".,;:") if url_match else None
    while url and url.endswith(")") and url.count(")") > url.count("("):
        url = url[:-1]
    pr_match = _PR_RE.search(text)
    ticket_match = _TICKET_RE.search(text)
    return {
        "pr": int(pr_match.group("pr")) if pr_match else None,
        "ticket": ticket_match.group(0) if ticket_match else None,
        "url": url,
    }


@dataclass
class Item:
    index: int
    label: str  # Gate | Answer | Verify | What to test
    text: str
    metadata: dict[str, Any] | None = None
    blocking: bool = False

    def wire(self) -> dict[str, Any]:
        metadata = _metadata(self.text)
        if self.metadata:
            metadata.update(self.metadata)
        return {
            "n": self.index,
            "label": self.label,
            "text": self.text,
            "blocking": self.blocking,
            **metadata,
            "default": None,
            "default_at": None,
        }


@dataclass
class Decision:
    index: int
    text: str
    recommendation: str
    decided_at: str | None = None

    def wire(self) -> dict[str, Any]:
        decided_at = self.decided_at
        if decided_at is None:
            decided_at = datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")
        return {
            "n": self.index,
            "text": self.text,
            "recommendation": self.recommendation,
            "reversible": True,
            "decided_at": decided_at,
        }


@dataclass(frozen=True)
class _ClosingBlock:
    start: int
    end: int
    header: re.Match[str] | None
    decisions_end: int


@dataclass
class ClosingBlock:
    present: bool = False
    items: list[Item] = field(default_factory=list)
    decisions: list[Decision] = field(default_factory=list)
    declared_blocking: int | None = None
    agents: list[str] = field(default_factory=list)
    declared_agents: int | None = None
    done_here: bool = False
    contract: str | None = None
    contract_met: bool | None = None
    waiting_on_you: bool = False
    waiting_count: int | None = None
    external_wait: str | None = None
    workers_unknown: bool = False
    # Whether the author labeled anything themselves, and whether any parsed
    # line was discarded for carrying no label. Both are needed to tell a
    # miscounted header apart from an incomplete parse.
    authored_labels: bool = False
    discarded_items: int = 0
    # Set when items were recovered without a recognised heading.
    fallback: bool = False

    @property
    def gates(self) -> list[Item]:
        return [item for item in self.items if item.label in GATE_LABELS]

    @property
    def nongate_items(self) -> list[Item]:
        return [item for item in self.items if item.label not in GATE_LABELS]

    @property
    def action_points(self) -> list[Item]:
        return [
            item
            for item in self.items
            if item.label in HUMAN_INPUT_LABELS and item.label not in GATE_LABELS
        ]

    @property
    def blocking(self) -> int:
        return len(
            [
                item
                for item in self.items
                if item.label in HUMAN_INPUT_LABELS
            ]
        )

    @property
    def parse_status(self) -> str:
        if not self.present:
            return "missing"
        if self.fallback:
            return "malformed"
        if self.discarded_items:
            return "malformed"
        if (
            self.declared_blocking is not None
            and self.declared_blocking != self.blocking
        ):
            return "malformed"
        if self.waiting_count is not None and self.waiting_count != self.blocking:
            return "malformed"
        if self.waiting_on_you and self.blocking == 0:
            return "malformed"
        if self.done_here and (
            self.blocking > 0 or self.agents_running > 0 or self.external_wait
        ):
            return "malformed"
        return "ok"

    @property
    def completion(self) -> str:
        if self.parse_status == "missing":
            return "missing"
        if (
            self.done_here
            and self.parse_status == "ok"
            and self.blocking == 0
            and self.agents_running == 0
            and not self.external_wait
            and not self.workers_unknown
            and self.contract_met is not False
        ):
            return "complete"
        return "incomplete"

    @property
    def agents_running(self) -> int:
        if self.workers_unknown:
            return 0
        if self.declared_agents is not None:
            return self.declared_agents
        return len(self.agents)

    @property
    def herdr_state(self) -> str:
        if (
            self.blocking > 0
            or self.waiting_on_you
            or (self.parse_status == "malformed" and (self.declared_blocking or 0) > 0)
        ):
            return "blocked"
        if self.agents_running > 0 or self.external_wait:
            return "working"
        return "idle"

    def message(self) -> str | None:
        if self.blocking > 0:
            pending = [
                item
                for item in self.items
                if item.label in HUMAN_INPUT_LABELS
            ]
            head = pending[0].text if pending else ""
            if not head:
                return f"{self.blocking} blocking"
            extra = f" (+{self.blocking - 1})" if self.blocking > 1 else ""
            return head[:80] + extra
        if self.agents_running > 0:
            n = self.agents_running
            return f"{n} agent{'s' if n != 1 else ''} running"
        if self.action_points:
            return self.action_points[0].text[:80] or None
        return None

    def wire_gates(self) -> list[dict[str, Any]]:
        return [item.wire() for item in self.gates]

    def wire_items(self) -> list[dict[str, Any]]:
        return [item.wire() for item in self.nongate_items]

    def wire_decisions(self) -> list[dict[str, Any]]:
        return [decision.wire() for decision in self.decisions]


def _fence_ranges(text: str) -> list[tuple[int, int]]:
    ranges: list[tuple[int, int]] = []
    fence_start: int | None = None
    fence_char: str | None = None
    fence_length = 0
    offset = 0

    for line in text.splitlines(keepends=True):
        line_end = offset + len(line)
        content = line.rstrip("\r\n")
        if fence_start is None:
            opener = _FENCE_OPEN_RE.fullmatch(content)
            if opener:
                marker = opener.group("marker")
                fence_start = offset
                fence_char = marker[0]
                fence_length = len(marker)
        else:
            closer = _FENCE_CLOSE_RE.fullmatch(content)
            if closer:
                marker = closer.group("marker")
                if marker[0] == fence_char and len(marker) >= fence_length:
                    ranges.append((fence_start, line_end))
                    fence_start = None
                    fence_char = None
                    fence_length = 0
        offset = line_end

    if fence_start is not None:
        # An unfinished example is treated as fenced through end-of-message;
        # an ambiguous tail must not fabricate live status markers.
        ranges.append((fence_start, len(text)))
    return ranges


def _visible_matches(
    pattern: re.Pattern[str],
    text: str,
    start: int,
    end: int,
    fences: list[tuple[int, int]],
):
    for match in pattern.finditer(text, start, end):
        if not any(
            fence_start <= match.start() < fence_end
            for fence_start, fence_end in fences
        ):
            yield match


def _section_end(
    text: str, start: int, end: int, fences: list[tuple[int, int]]
) -> int:
    candidates = [
        match.start()
        for pattern in (
            _DECISIONS_RE,
            _NOTHING_RE,
            _WHAT_RE,
            _AGENTS_RE,
            _DONE_RE,
            _NOW_RE,
        )
        for match in _visible_matches(pattern, text, start, end, fences)
    ]
    return min(candidates, default=end)


def _decisions_end(
    text: str, start: int, end: int, fences: list[tuple[int, int]]
) -> int:
    # The decisions list may sit before the critical-action-points block (the
    # authoring rules put the closing block last), so the item scan must stop
    # at whichever section opens next or the CAP items parse as decisions.
    candidates = [
        match.start()
        for pattern in (
            _DECISIONS_RE,
            _HEADER_RE,
            _NOTHING_RE,
            _WHAT_RE,
            _AGENTS_RE,
            _DONE_RE,
            _NOW_RE,
        )
        for match in _visible_matches(pattern, text, start, end, fences)
    ]
    return min(candidates, default=end)


def _closing_blocks(
    text: str, fences: list[tuple[int, int]]
) -> list[_ClosingBlock]:
    """Split top-level CAP sections into blocks and bound their decisions.

    The first CAP belongs to the initial block so valid pre-CAP decisions are
    retained. A top-level Nothing marker bounds decisions in that block; the
    last such marker is the closing boundary, which keeps repeated markers in
    one structural block. A Nothing before the first CAP closes the preamble,
    so that stale decisions cannot flow into the later CAP block.
    """
    headers = list(_visible_matches(_HEADER_RE, text, 0, len(text), fences))
    nothings = list(_visible_matches(_NOTHING_RE, text, 0, len(text), fences))
    if not headers:
        if not nothings:
            return []
        return [_ClosingBlock(0, len(text), None, nothings[-1].start())]

    first_start = 0
    if nothings and nothings[0].start() < headers[0].start():
        first_start = headers[0].start()

    blocks: list[_ClosingBlock] = []
    for position, header in enumerate(headers):
        start = first_start if position == 0 else header.start()
        end = (
            headers[position + 1].start()
            if position + 1 < len(headers)
            else len(text)
        )
        block_nothings = [
            marker
            for marker in nothings
            if start <= marker.start() < end
        ]
        decisions_end = block_nothings[-1].start() if block_nothings else end
        blocks.append(_ClosingBlock(start, end, header, decisions_end))
    return blocks


def _parse_what_to_test(
    text: str,
    start: int,
    end: int,
    items: list[Item],
    fences: list[tuple[int, int]],
) -> list[Item]:
    headings = list(_visible_matches(_WHAT_RE, text, start, end, fences))
    parsed: list[Item] = []
    for position, heading in enumerate(headings):
        end_candidates = [
            match.start()
            for pattern in (
                _DECISIONS_RE,
                _NOTHING_RE,
                _AGENTS_RE,
                _DONE_RE,
                _NOW_RE,
            )
            for match in _visible_matches(
                pattern, text, heading.end(), end, fences
            )
        ]
        if position + 1 < len(headings):
            end_candidates.append(headings[position + 1].start())
        section_end = min(end_candidates, default=end)
        body = text[heading.end() : section_end].strip()
        if not body:
            continue
        gate_number = re.search(r"\bGate\s+(?P<n>\d+)\b", heading.group("meta"), re.I)
        index = int(gate_number.group("n")) if gate_number else (
            max((item.index for item in [*items, *parsed]), default=0) + 1
        )
        parsed.append(
            Item(index, "What to test", body, _metadata(heading.group(0) + " " + body))
        )
    return parsed


def _parse_items(
    text: str,
    start: int,
    end: int,
    fences: list[tuple[int, int]],
    *,
    any_bold_label: bool = True,
) -> list[Item]:
    section_end = _section_end(text, start, end, fences)
    matches = list(_visible_matches(_ITEM_RE, text, start, section_end, fences))
    parsed: list[Item] = []
    for position, match in enumerate(matches):
        end_candidates = [section_end]
        if position + 1 < len(matches):
            end_candidates.append(matches[position + 1].start())
        for pattern in (_AGENTS_RE, _DONE_RE, _NOW_RE, _ANSWER_INSTRUCTION_RE):
            end_candidates.extend(
                candidate.start()
                for candidate in _visible_matches(
                    pattern, text, match.end(), section_end, fences
                )
            )
        item_end = min(end_candidates)
        body = text[match.start("body") : item_end]
        item_metadata = _metadata(body)
        label_match = None
        for candidate in _ITEM_LABEL_RE.finditer(body):
            absolute_start = match.start("body") + candidate.start()
            if not any(
                fence_start <= absolute_start < fence_end
                for fence_start, fence_end in fences
            ):
                label_match = candidate
                break
        label = ""
        if label_match:
            label = label_match.group("bold_label") or label_match.group("plain_label") or ""
            if not label and any_bold_label and label_match.group("other_label"):
                label = FALLBACK_LABEL
            before = body[: label_match.start()].rstrip()
            after = body[label_match.end() :].lstrip()
            body = "\n".join(part for part in (after, before) if part)
        cleaned_body = _clean_body(body)
        legacy_suffix = _LEGACY_NONBLOCKING_SUFFIX_RE.match(cleaned_body)
        if legacy_suffix:
            cleaned_body = legacy_suffix.group("text").rstrip()
        normalized_label = _canonical_human_input_label(label)
        parsed.append(
            Item(
                int(match.group("idx")),
                normalized_label,
                cleaned_body,
                metadata=item_metadata,
                blocking=_is_human_input_label(normalized_label),
            )
        )
    return parsed


def _parse_decisions(
    text: str,
    headings: list[re.Match[str]],
    end: int,
    fences: list[tuple[int, int]],
) -> list[Decision]:
    parsed: list[Decision] = []
    for decisions_heading in headings:
        decisions_end = _decisions_end(
            text, decisions_heading.end(), end, fences
        )
        matches = list(
            _visible_matches(
            _DECISION_ITEM_RE,
                text,
                decisions_heading.end(),
                decisions_end,
                fences,
            )
        )
        for position, match in enumerate(matches):
            end_candidates = [decisions_end]
            if position + 1 < len(matches):
                end_candidates.append(matches[position + 1].start())
            for pattern in (_AGENTS_RE, _DONE_RE, _NOW_RE):
                end_candidates.extend(
                    candidate.start()
                    for candidate in _visible_matches(
                        pattern, text, match.end(), decisions_end, fences
                    )
                )
            body = _clean_body(
                text[match.start("body") : min(end_candidates)]
            )
            if not body:
                continue
            recommendation, decided_at = _decision_fields(body)
            parsed.append(
                Decision(int(match.group("idx")), body, recommendation, decided_at)
            )
    return parsed


def _decision_fields(body: str) -> tuple[str, str | None]:
    recommendation_match = re.search(
        r"\brecommendation\s*[:\-]\s*(?P<value>.+?)(?=\s+at\s+\d{1,2}:\d{2}|$)",
        body,
        re.IGNORECASE,
    ) or _RECOMMENDATION_RE.search(body)
    recommendation = (
        recommendation_match.group("value").strip(" .")
        if recommendation_match
        else body
    )
    decided_match = _DECIDED_AT_RE.search(body)
    decided_at = decided_match.group("value").strip() if decided_match else None
    return recommendation, decided_at


# Only the exact policy forms are dropped; any other prose is a worker.
_NOW_NOT_WORK_RE = re.compile(
    r"^(?:waiting[ \t]+on[ \t]+you|idle|nothing[ \t]+running)\.?$", re.IGNORECASE
)
def _now_entries(rest: str) -> tuple[list[str], list[str]]:
    streams: list[str] = []
    waits: list[str] = []
    for entry in re.split(r"[ \t]*·[ \t]*", rest):
        entry = entry.strip()
        if not entry:
            continue
        wait = re.match(r"^wait(?:[ \t]*[—–:][ \t]*|[ \t]+-[ \t]+)(?P<event>.+?)\.?$", entry, re.I)
        if wait:
            waits.append(wait.group("event").strip())
        elif re.match(r"^stopped(?:$|[ \t]*[—–:]|[ \t]+-[ \t]+)", entry, re.I):
            continue
        elif _NOW_NOT_WORK_RE.match(entry):
            # Waiting on the human is already carried by Needs you; it is not
            # a running worker and must not make the worker count unknown.
            # External waits use the `wait — <event>` form; other prose is a
            # worker, so the parser never guesses from free text.
            continue
        else:
            streams.append(entry)
    return streams, waits


def _headerless_fallback(
    text: str, fences: list[tuple[int, int]], block: ClosingBlock
) -> None:
    """Recover pending human input when no closing-block heading matched.

    The heading words change with the policy. When they no longer match, the
    reply still carries structural evidence of a question: numbered items with
    a bold label, or the `Reply 1a / 1b. Silence holds.` answer line. Either
    one marks the pane blocked, so a renamed heading cannot hide a blocker.
    The parse is reported as malformed so the drift stays visible.
    """
    answer_lines = list(
        _visible_matches(_ANSWER_INSTRUCTION_RE, text, 0, len(text), fences)
    )
    anchor = answer_lines[-1].start() if answer_lines else len(text)
    firsts = [
        match
        for match in _visible_matches(_ITEM_RE, text, 0, anchor, fences)
        if int(match.group("idx")) == 1
    ]
    items: list[Item] = []
    if firsts:
        # Without a heading, any bold word is too weak: progress replies bold
        # their list entries. Only the known label words count here.
        items = _parse_items(
            text, firsts[-1].start(), anchor, fences, any_bold_label=False
        )
    labeled = [item for item in items if item.label]
    if labeled:
        items = labeled
    elif answer_lines:
        for item in items:
            item.label = FALLBACK_LABEL
            item.blocking = True
        if not items:
            prose = text[:anchor].strip().splitlines()
            items = [Item(1, FALLBACK_LABEL, prose[-1].strip() if prose else "", blocking=True)]
    else:
        return
    block.present = True
    block.fallback = True
    block.items = items


def parse(text: str) -> ClosingBlock:
    """Parse the last closing block out of a full assistant message."""
    block = ClosingBlock()

    if text.startswith("\ufeff"):
        text = text[1:]
    fences = _fence_ranges(text)
    blocks = _closing_blocks(text, fences)
    selected = blocks[-1] if blocks else None
    start = None
    authoritative_start = selected.start if selected else 0
    if selected:
        block.present = True
        if selected.header:
            block.declared_blocking = _declared_count(selected.header)
            start = selected.header.end()
        else:
            block.declared_blocking = 0

        if start is not None:
            block.items.extend(_parse_items(text, start, selected.end, fences))
            waiting = None
            for candidate in _visible_matches(
                _WAITING_ON_YOU_RE,
                text,
                authoritative_start,
                selected.end,
                fences,
            ):
                waiting = candidate
            waiting_indices: set[int] = set()
            if waiting:
                block.waiting_on_you = True
                count_match = re.search(
                    r"\b(?P<n>\d+)[ \t]+items?\b", waiting.group("rest"), re.I
                )
                if count_match:
                    block.waiting_count = int(count_match.group("n"))
                item_match = re.search(
                    r"\bitem[ \t]+(?P<n>\d+)\b", waiting.group("rest"), re.I
                )
                if item_match:
                    waiting_indices.add(int(item_match.group("n")))
                paren_match = re.search(r"\((?P<items>[\d, \t]+)\)", waiting.group("rest"))
                if paren_match:
                    waiting_indices.update(
                        int(value)
                        for value in paren_match.group("items").split(",")
                        if value.strip().isdigit()
                    )

            # A Waiting-on-you footer is bounded to this authoritative CAP and
            # can safely recover its referenced unlabeled item. Legacy unlabeled
            # blocks still promote only as many entries as their heading declares.
            labeled_items = sum(1 for item in block.items if item.label)
            declared = block.declared_blocking or 0
            block.authored_labels = any(item.label for item in block.items)
            for item in block.items:
                if not item.label and item.index in waiting_indices:
                    item.label = HUMAN_INPUT_LABELS[1]
                    item.blocking = _is_human_input_label(item.label)
                    labeled_items += 1
            if not block.authored_labels:
                for item in block.items:
                    if item.label == "" and labeled_items < declared:
                        item.label = GATE_LABELS[0]
                        item.blocking = _is_human_input_label(item.label)
                        labeled_items += 1
            kept = [item for item in block.items if item.label]
            block.discarded_items = len(block.items) - len(kept)
            block.items = kept
            block.items.extend(
                _parse_what_to_test(
                    text, start, selected.end, block.items, fences
                )
            )

        decision_headings = list(
            _visible_matches(
                _DECISIONS_RE,
                text,
                selected.start,
                selected.decisions_end,
                fences,
            )
        )
        if selected.header:
            post_cap_headings = [
                heading
                for heading in decision_headings
                if heading.start() >= selected.header.end()
            ]
            if post_cap_headings:
                # Pre-CAP decisions are a supported authoring order when no
                # later section exists. Once a selected CAP has a post-CAP
                # section, that section is operative and supersedes all
                # earlier pre-CAP sections. Keep every heading on the
                # operative side so repeated sections in one block survive.
                decision_headings = post_cap_headings

        block.decisions.extend(
            _parse_decisions(
                text, decision_headings, selected.decisions_end, fences
            )
        )

    if not blocks:
        _headerless_fallback(text, fences, block)

    lifecycle_agents = list(
        _visible_matches(_AGENTS_RE, text, authoritative_start, len(text), fences)
    )
    lifecycle_done = list(
        _visible_matches(_DONE_RE, text, authoritative_start, len(text), fences)
    )
    lifecycle_waits = list(
        _visible_matches(_EXTERNAL_WAIT_RE, text, authoritative_start, len(text), fences)
    )
    lifecycle_now = list(
        _visible_matches(_NOW_RE, text, authoritative_start, len(text), fences)
    )
    last_done = lifecycle_done[-1] if lifecycle_done else None
    agents = lifecycle_agents[-1] if lifecycle_agents else None
    external_wait = lifecycle_waits[-1] if lifecycle_waits else None
    last_now = lifecycle_now[-1] if lifecycle_now else None
    latest_legacy_position = max(
        (marker.start() for marker in (agents, external_wait) if marker), default=-1
    )
    now_is_latest = bool(last_now and last_now.start() > latest_legacy_position)
    latest_activity_position = max(
        latest_legacy_position, last_now.start() if last_now else -1
    )
    done_is_final = bool(last_done and last_done.start() > latest_activity_position)

    if done_is_final:
        block.declared_agents = 0
    elif now_is_latest and last_now:
        block.agents, waits = _now_entries(last_now.group("rest"))
        block.declared_agents = len(block.agents)
        block.workers_unknown = block.declared_agents > 0
        if waits:
            block.external_wait = "; ".join(waits)
        # A liveness line alone is a progress reply, not a closing block: it
        # must not report an ok parse with empty blocker arrays, which the
        # server would take as authority to clear a pending human decision.
    else:
        if agents:
            block.declared_agents = int(agents.group("n"))
            block.agents = [
                part.strip()
                for part in re.split(
                    r"(?:[ \t]*;[ \t]+|[ \t]+·[ \t]+)", agents.group("rest")
                )
                if part.strip()
            ]
            # Closing prose is an unverified claim. Keep names as reconciliation
            # hints, but reserve the active count for runtime worker lifecycle.
            block.workers_unknown = block.declared_agents > 0
            block.present = True
        else:
            block.declared_agents = 0
        if external_wait:
            block.external_wait = external_wait.group("rest").strip().rstrip(".")
            block.present = True

    if done_is_final:
        block.done_here = True
        block.present = True

    if block.present:
        contract = None
        for candidate in _visible_matches(
            _CONTRACT_RE, text, 0, len(text), fences
        ):
            contract = candidate
        if contract:
            block.contract = contract.group("text").strip()
            block.contract_met = contract.group("state").lower() == "met"

    return block
