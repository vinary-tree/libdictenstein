#!/usr/bin/env python3
"""Doc-math hygiene gate: reject math delimiters that GitHub silently corrupts.

GitHub's Markdown pass applies CommonMark backslash-escape processing to the
*interior* of `$…$` and `$$…$$` math spans before MathJax ever parses them.  So
`\\_` → `_`, `\\{` → `{`, `\\;` → `;`, `\\,` → `,`, `\\#` → `#`.  Two failure modes
follow:

  * loud   — a bare `_` or `#` reaches MathJax, which aborts with
             "'_' allowed only in math mode";
  * silent — `\\max\\{\\,L\\,\\}` renders as `\\max{,L,}`: the set braces vanish and
             literal commas replace thin-spaces, with no error at all.

Only two delimiter forms survive GitHub's escape pass verbatim:

    inline    $`…`$
    display   ```math … ```

This gate enforces exactly that.  Verified against GitHub's own GFM renderer
(`gh api -X POST /markdown`); see docs/README.md § Math in Markdown.

Two further rules, both established by probing GitHub's renderer:

  * an ASCII letter may not abut an opening ``$` `` — GitHub opens no math there, so
    ``InMem$`\\Rightarrow`$descend`` silently renders as literal text;
  * two or more literal ``\\$`` on one line can pair into a spurious math span, and
    whether they do is not predictable from the source.

HTML is handled by exempting attribute *values* only (see `blank_attribute_values`): GitHub
renders `<img alt="…">` text verbatim, so `$…$` there is not math — but the *body* of an HTML
block (`<td>`, `<summary>`, …) is ordinary Markdown and is scanned like any other line.

Usage:
    python3 scripts/check-doc-math.py            # every tracked *.md
    python3 scripts/check-doc-math.py FILE...    # just these files
    python3 scripts/check-doc-math.py --selftest # tokenizer + rule regression tests

Exits 0 when clean, 1 when any unsafe delimiter remains.
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path
from typing import Iterator, NamedTuple

# `$` + backtick-run + body + matching backtick-run + `$` — the GitHub-safe inline form.
SAFE_INLINE = re.compile(r"\$(`+)(.+?)\1\$")

# A fence opener/closer: three backticks, optionally indented, plus an info string.  The captured
# group is the info string's first word — `math` for a display-math fence, `rust`/`text`/`` for the
# rest.  A ```math body is ordinary LaTeX and is scanned by the body rules (a/b) below; every other
# fenced block is skipped entirely, exactly as before.
FENCE = re.compile(r"^\s*```(\w*)")

# An HTML tag — opening or closing — that begins and ends on one line.  This regex also matches
# `Option<V>` and `Vec<u8>`, which is harmless: the only thing done inside a matched span is
# blanking attribute *values*, and those carry none.  (No tracked doc has a tag spanning lines.)
HTML_TAG = re.compile(r"</?[A-Za-z!][^<>]*>")

# `name="…"` or `name='…'` within a tag.  Anchored on the `=` so a bare quoted phrase in prose is
# never mistaken for an attribute value.
HTML_ATTR_VALUE = re.compile(r"=\s*(\"[^\"]*\"|'[^']*')")

# ─── Body & notation rules (added atop the delimiter gate) ──────────────────────────────────────
#
# The four rules above police the *delimiters* that survive GitHub's CommonMark pass.  The three
# below police what goes *inside* a math span (the body) and what masquerades as math outside one.
# They are documented in docs/notation.md.  All three run only on non-archival files (see
# `is_archival`), because the historical ledgers under docs/design/history/ are a frozen record.
#
#   (a) mid-delimiter  — `\mid` is a LaTeX *relation* (\mathrel), spaced on both sides; it is wrong
#                        for length delimiters (use `\lvert…\rvert`) and for set-builder "such that"
#                        (use `:`).  A prior migration used it as a delimiter 188×; forbid it.
#   (b) unicode-in-math — a non-ASCII glyph inside a math body is an un-rendered literal (`−` U+2212
#                        instead of `-`, `≤` instead of `\le`, `Σ` instead of `\Sigma`, a combining
#                        macron instead of `\bar{}`).  Every such glyph has an ASCII LaTeX form.
#   (c) inert-code-math — big-O / bare-Greek typeset as an inert code span (`` `O(1)` ``) instead of
#                        inline MathJax (``$`O(1)`$``).  Reusing the tokenizer makes this safe: a
#                        real ``$`…`$`` is a `safe` token (never `code`), fenced code is skipped, and
#                        `alt="…"` is blanked — so `O_DIRECT`, `// O(1)` in a Rust fence, and correct
#                        math are all structurally immune.

# `\mid` not followed by a letter (so `\midpoint`, were it ever written, is left alone).
MID = re.compile(r"\\mid(?![A-Za-z])")

# A code span that is really math: the big-O family with a parenthesised argument, or a lone
# upper-case Greek letter.  Matched against the code span's content (backtick runs + surrounding
# space stripped).  Anchored so `O_DIRECT`, `Option<V>`, `to(x)` never match.
INERT_MATH = re.compile(r"^[OoΘΩθω𝒪]\s*\(.+\)$|^[ΘΩΣΠΛ]$")

# Non-ASCII code points permitted inside a math body.  Empty by design — every mathematical glyph
# has an ASCII LaTeX command.  Kept as an explicit, documented escape hatch (a future `\text{café}`
# could add `é` here) rather than a silent tolerance.
MATH_UNICODE_ALLOWLIST: frozenset = frozenset()

# The three body rules are fatal on any file NOT listed here and report-only (a warning) on any file
# that IS — the ratchet that lets the gate land without one giant atomic commit.  A grandfathered
# file that is now *clean* is itself an error (a stale entry), so the set can only shrink; the
# overhaul's final phase asserts it empty and deletes this mechanism.  Populated by an initial
# capture run (`python3 scripts/check-doc-math.py` with this set empty) over the tracked tree.
GRANDFATHERED: frozenset[str] = frozenset(
    {
        # Emptied 2026-07-10: the overhaul cleaned every file that carried pre-standard body-rule
        # debt, so the three body rules are now unconditionally fatal on every non-archival file.
        # The ratchet machinery below is retained (inert while this set is empty) as the mechanism
        # by which any future backlog would be introduced and then burned down.
    }
)

BODY_RULES = frozenset({"mid-delimiter", "unicode-in-math", "inert-code-math"})


def is_archival(path: str) -> bool:
    """True for the frozen historical record, which the body rules skip.

    Matches the owner's exemption: the campaign ledgers under docs/design/history/, and any
    `*ledger*.md` / `*handoff*.md`.  The delimiter rules still run on these — only the body rules
    (a/b/c) are suppressed, because rewriting the ledgers would falsify a scientific record.
    """
    return (
        "/design/history/" in path
        or path.endswith("-ledger.md")
        or "ledger" in path.rsplit("/", 1)[-1]
        or "handoff" in path
    )


def check_math_body(path: str, line: int, body: str) -> Iterator["Violation"]:
    """Yield rule-(a) and rule-(b) violations for one math body (a ``$`…`$`` span or a ```math line)."""
    if MID.search(body):
        yield Violation(path, line, "mid-delimiter", body.strip()[:90])
    bad = sorted({ch for ch in body if ord(ch) > 0x7F and ch not in MATH_UNICODE_ALLOWLIST})
    if bad:
        glyphs = ", ".join(f"{ch!r} (U+{ord(ch):04X})" for ch in bad)
        yield Violation(path, line, "unicode-in-math", f"{glyphs} in  {body.strip()[:70]}")


class Token(NamedTuple):
    kind: str  # 'text' | 'esc' | 'code' | 'safe' | 'inline' | 'display'
    text: str  # the raw source slice
    body: str  # for 'inline'/'display': the math body, delimiters stripped


class Violation(NamedTuple):
    path: str
    line: int
    # delimiter rules: 'inline' | 'display' | 'letter-before' | 'literal-dollar'
    # body rules:      'mid-delimiter' | 'unicode-in-math' | 'inert-code-math'
    kind: str
    snippet: str


FIXES = {
    "inline": "use $`…`$",
    "display": "use a ```math fence",
    "letter-before": "insert a space before the opening `$` (GitHub renders no math otherwise)",
    "literal-dollar": "wrap each literal dollar in inline code, e.g. `$₁`",
    "mid-delimiter": r"\mid is a relation, not a delimiter — use \lvert…\rvert for length, ':' for set-builder",
    "unicode-in-math": r"replace the glyph with its LaTeX command (− → -, ≤ → \le, Σ → \Sigma, s̄ → \bar{s})",
    "inert-code-math": "typeset as inline MathJax — $`O(1)`$, not a `O(1)` code span",
}


def tokenize(line: str, open_run: int = 0) -> tuple[list[Token], int]:
    """Split one non-fenced Markdown line into math-relevant tokens.

    Handles, in priority order:
      * backslash escapes — `\\$` is a *literal* dollar (the suffix-automaton
        sentinel in docs/theory/scdawg/), never a delimiter;
      * inline code spans — a run of N backticks closes on a run of exactly N;
      * `$$…$$` single-line display math;
      * `$`…`$` GitHub-safe inline math (recognised *before* code spans could
        swallow the backticks);
      * `$…$` bare inline math.

    A CommonMark code span may span line breaks within a paragraph, so `open_run`
    carries the length of a backtick run left unterminated by the previous line.
    Returns the tokens plus the run still open at end-of-line.  Without this a
    line like ``persist.rs$\\approx L529-555$disk_io.rs`` — whose dollars sit
    *inside* a code span opened on the line above — is mistaken for math.
    """
    out: list[Token] = []
    i, n = 0, len(line)

    if open_run:
        # We are inside a code span carried over from a previous line.
        k = 0
        while k < n:
            if line[k] == "`":
                m = k
                while m < n and line[m] == "`":
                    m += 1
                if m - k == open_run:
                    out.append(Token("code", line[: m], ""))
                    i = m
                    open_run = 0
                    break
                k = m
            else:
                k += 1
        else:
            return [Token("code", line, "")], open_run

    while i < n:
        c = line[i]
        prev_i = i

        if c == "\\":
            # A trailing backslash escapes nothing; `line[i:i+2]` is just "\" and the
            # `i += 2` still advances past end-of-line.  Guarding this with
            # `i + 1 < n` instead would fall through to the text branch below, whose
            # scan stops dead on "\" — i never advances and the token list grows
            # without bound (a 93 GB OOM, observed).
            out.append(Token("esc", line[i : i + 2], ""))
            i += 2

        elif c == "`":
            j = i
            while j < n and line[j] == "`":
                j += 1
            run = j - i
            close = -1
            k = j
            while k < n:
                if line[k] == "`":
                    m = k
                    while m < n and line[m] == "`":
                        m += 1
                    if m - k == run:
                        close = k
                        break
                    k = m
                else:
                    k += 1
            if close < 0:
                # Unterminated on this line: the code span continues onto the next.
                out.append(Token("code", line[i:], ""))
                return out, run
            out.append(Token("code", line[i : close + run], ""))
            i = close + run

        elif c == "$":
            if line.startswith("$$", i):
                end = line.find("$$", i + 2)
                if end != -1:
                    out.append(Token("display", line[i : end + 2], line[i + 2 : end]))
                    i = end + 2
                else:
                    out.append(Token("text", "$$", ""))
                    i += 2
            else:
                m = SAFE_INLINE.match(line, i)
                if m:
                    out.append(Token("safe", m.group(0), m.group(2)))
                    i = m.end()
                    continue
                # Scan for the closing unescaped `$`.  Note we must NOT skip over a
                # `$$` here: adjacent spans such as `$\Rightarrow$$\neg$` are two
                # inline spans that happen to abut, and the first one closes on the
                # first of the two dollars.  (GitHub itself fails to render that
                # source at all; rewriting to $`…`$$`…`$ fixes it.)
                j, close = i + 1, -1
                while j < n:
                    if line[j] == "\\":
                        j += 2
                        continue
                    if line[j] == "$":
                        close = j
                        break
                    j += 1
                if close > i + 1:
                    out.append(Token("inline", line[i : close + 1], line[i + 1 : close]))
                    i = close + 1
                else:
                    out.append(Token("text", "$", ""))
                    i += 1

        else:
            j = i
            while j < n and line[j] not in "\\`$":
                j += 1
            out.append(Token("text", line[i:j], ""))
            i = j

        if i <= prev_i:  # every branch must consume at least one character
            raise AssertionError(
                f"tokenize made no progress at column {i} of {line!r} — "
                "this would loop forever and exhaust memory"
            )

    return out, 0


def blank_attribute_values(line: str) -> str:
    """Replace the *interior* of every HTML attribute value with spaces.

    `$…$` inside an attribute (e.g. an `<img alt="…">` describing a diagram) is never math —
    GitHub renders attribute text verbatim — so it must not be scanned.  An earlier version
    skipped any line *starting* with a tag, which also blinded the gate to real math in the
    *body* of an HTML block: `<td>$x\\_y$</td>` and `<details><summary>$x\\_y$</summary>` went
    unchecked forever.  Blanking only the quoted values fixes that while keeping attributes out
    of scope.

    Every blanked character is replaced one-for-one by a space, so the line's length and all
    column offsets are preserved — the `letter-before` rule indexes into this string by column.

    Blanking happens strictly *inside* a matched tag.  That is what keeps `Option<V>` and
    `Vec<u8>` safe: they match `HTML_TAG` but contain no `name="…"`, so nothing is blanked.
    """
    out = list(line)
    for tag in HTML_TAG.finditer(line):
        base = tag.start()
        for attr in HTML_ATTR_VALUE.finditer(tag.group(0)):
            # attr.span(1) covers the quotes too; blank only what lies between them.
            start, end = attr.span(1)
            for k in range(base + start + 1, base + end - 1):
                out[k] = " "
    return "".join(out)


def scan(path: Path) -> Iterator[Violation]:
    """Yield every math violation in `path` — delimiter rules everywhere, body rules off-archive."""
    lines = path.read_text(errors="replace").split("\n")
    archival = is_archival(str(path))
    in_fence = False
    fence_lang = ""  # the info string of the open fence; "math" bodies are scanned for a/b
    open_run = 0  # a code span carried over from the previous line
    i = 0
    while i < len(lines):
        line = lines[i]
        stripped = line.strip()

        m = FENCE.match(line)
        if m:
            # A fence line toggles fenced state.  On open, remember its language; on close, forget.
            fence_lang = "" if in_fence else (m.group(1) or "").lower()
            in_fence = not in_fence
            open_run = 0
            i += 1
            continue
        if in_fence:
            # A ```math body is ordinary LaTeX: scan it for the body rules (never the delimiter
            # rules — bare content is expected inside a fence).  Every other fence is skipped.
            if fence_lang == "math" and not archival:
                yield from check_math_body(str(path), i + 1, line)
            i += 1
            continue
        if not stripped:  # a blank line ends the paragraph, hence any code span
            open_run = 0
            i += 1
            continue

        # Multi-line display block: a line that is exactly `$$`.
        if stripped == "$$":
            end = i + 1
            while end < len(lines) and lines[end].strip() != "$$":
                end += 1
            yield Violation(str(path), i + 1, "display", "$$ … $$ block")
            i = end + 1
            continue

        # Attribute text is rendered verbatim by GitHub, so it is never math.  Blanking it
        # (length-preserving) lets the body of an HTML block still be scanned.
        scanned = blank_attribute_values(line)

        toks, open_run = tokenize(scanned, open_run)
        literal_dollars = 0
        col = 0
        for tok in toks:
            if tok.kind in ("inline", "display"):
                yield Violation(str(path), i + 1, tok.kind, tok.text)
            elif tok.kind == "esc" and tok.text == "\\$":
                literal_dollars += 1
            elif tok.kind == "safe":
                # GitHub declines to open inline math when the `$` abuts an ASCII letter.
                if col > 0 and scanned[col - 1].isascii() and scanned[col - 1].isalpha():
                    yield Violation(str(path), i + 1, "letter-before", scanned[max(0, col - 12) : col + len(tok.text)])
                # The math body itself: rules (a) `\mid`-delimiter and (b) unicode-in-math.
                if not archival:
                    yield from check_math_body(str(path), i + 1, tok.body)
            elif tok.kind == "code" and not archival:
                # Rule (c): a code span that is really math (big-O / bare Greek).  Strip the backtick
                # runs and surrounding space to recover the content, then match.
                content = tok.text.strip("`").strip()
                if INERT_MATH.match(content):
                    yield Violation(str(path), i + 1, "inert-code-math", tok.text)
            col += len(tok.text)

        # Two or more literal dollars on one line can pair into a spurious math span.
        # GitHub's behaviour here is not predictable from the source (identical-looking
        # `\$` pairs render differently), so we forbid the hazard outright.
        if literal_dollars >= 2:
            yield Violation(str(path), i + 1, "literal-dollar", line.strip()[:90])
        i += 1


def selftest() -> int:
    """Prove the tokenizer terminates and the four rules fire, before trusting a scan.

    The termination half is not academic: a trailing backslash once fell through every
    branch, `i` stopped advancing, and the token list grew until the kernel OOM-killed
    the process at 93 GB.
    """
    import itertools
    import tempfile

    # 1. Termination + exact reconstruction over every short string of metacharacters.
    calls = 0
    for length in range(5):
        for tup in itertools.product("\\`$x", repeat=length):
            line = "".join(tup)
            for run in (0, 1, 2):
                toks, _ = tokenize(line, run)
                calls += 1
                if run == 0 and not line.endswith("\\"):
                    rebuilt = "".join(t.text for t in toks)
                    assert rebuilt == line, f"reconstruction: {line!r} -> {rebuilt!r}"
    assert tokenize("trailing backslash \\")[0], "trailing backslash must terminate"

    # 2. Each rule must fire on a positive sample, and stay silent on the safe form.
    expected = {
        "bare inline $x\\_y$": {"inline"},
        "$$\n\\max\\{\\,L\\,\\}\n$$": {"display"},
        "letter before InMem$`\\Rightarrow`$descend": {"letter-before"},
        "| markers | \\$₁, \\$₂ |": {"literal-dollar"},
        "safe $`x\\_y`$ and\n\n```math\n\\max\\{\\,L\\,\\}\n```": set(),
        # --- HTML handling: attribute values are exempt, element bodies are not. ---
        # An attribute value is rendered verbatim by GitHub, so `$…$` there is not math.
        '<img src="d.svg" alt="a $x\\_y$ b" width="100%"/>': set(),
        # …but the *body* of an HTML block is ordinary Markdown.  The old whole-line skip
        # blinded the gate to these; they must now be caught.
        "<td>bare $x\\_y$ in a cell</td>": {"inline"},
        "<summary>bare $x\\_y$ in a summary</summary>": {"inline"},
        # A tag on the line must not stop the rest of the line from being scanned.
        '<img src="d.svg" alt="fine"/> then bare $x\\_y$': {"inline"},
        # `Option<V>` / `Vec<u8>` match the tag regex but hold no attributes, so blanking is a
        # no-op and the real violation beside them is still reported.
        "`Option<V>` and `Vec<u8>` with bare $x\\_y$": {"inline"},
        # Literal dollars inside an attribute value cannot pair into a spurious span.
        '<img alt="cost \\$1 and \\$2" src="d.svg"/>': set(),
        # A blanked attribute must not make the letter-before rule misfire on its own text.
        '<img alt="InMem$`\\Rightarrow`$descend" src="d.svg"/>': set(),
        # The safe inline form still passes inside an element body.
        "<td>safe $`x\\_y`$ in a cell</td>": set(),
        # --- Body rule (a): `\mid` is a relation, never a delimiter. ---
        "bad $`O(\\mid q\\mid )`$": {"mid-delimiter"},
        # The colon set-builder form (what `\mid` set-builder must become) is clean.
        "setbuilder $`\\{\\, t : s = h\\cdot t \\in V \\,\\}`$": set(),
        # --- Body rule (b): non-ASCII inside a math body (− is U+2212, s̄ carries U+0304). ---
        "minus $`\\le 2\\cdot \\mid T\\mid − 1`$": {"mid-delimiter", "unicode-in-math"},
        "macron $`O(\\mid key\\mid / s̄)`$": {"mid-delimiter", "unicode-in-math"},
        "arrow-only span $`→`$ here": {"unicode-in-math"},
        # --- Body rule (c): big-O / bare-Greek typeset as an inert code span. ---
        "inert `O(1)` here": {"inert-code-math"},
        "inert `O(N log N)` here": {"inert-code-math"},
        "bare greek `Σ` here": {"inert-code-math"},
        # Structural negatives: real math, identifiers, and fenced code never trip rule (c).
        "safe $`O(\\lvert q\\rvert)`$ row": set(),
        "idents `O_DIRECT` and `Vec<u8>` and `to(x)`": set(),
        "```rust\nlet a = 1; // O(1) amortised\n```": set(),
        # A ```math body IS inspected for body rules (a new control path); a bare fence is not.
        "```math\nO(\\mid q\\mid )\n```": {"mid-delimiter"},
        "```math\nO(\\lvert q\\rvert)\n```": set(),
    }
    with tempfile.TemporaryDirectory() as td:
        for i, (src, want) in enumerate(expected.items()):
            p = Path(td) / f"t{i}.md"
            p.write_text(src + "\n")
            got = {v.kind for v in scan(p)}
            assert got == want, f"{src!r}: expected {want or '{}'} , got {got or '{}'}"

        # 3. Archival exemption: the body rules go silent on a docs/design/history/ path, but the
        #    delimiter rules still fire there.
        hist = Path(td) / "docs" / "design" / "history" / "campaign.md"
        hist.parent.mkdir(parents=True)
        hist.write_text("inert `O(1)` and bad $`O(\\mid q\\mid )`$ but a real $x\\_y$ delimiter\n")
        got = {v.kind for v in scan(hist)}
        assert got == {"inline"}, f"archival: expected {{'inline'}}, got {got or '{}'}"

    # 4. is_archival() classification.
    assert is_archival("docs/design/history/slice3/x.md")
    assert is_archival("docs/experiments/loading-optimization-ledger.md")
    assert is_archival("docs/benchmarks/c1-dawg-core-handoff.md")
    assert not is_archival("docs/notation.md")
    assert not is_archival("docs/theory/scdawg/04-scdawg.md")

    # 5. The grandfather ratchet: fatal off-list, warn on-list, stale when a listed file is clean.
    V = Violation
    grand = frozenset({"a.md"})
    vs = [V("a.md", 1, "inert-code-math", "`O(1)`"), V("b.md", 2, "inert-code-math", "`O(n)`")]
    fatal, warned, stale = partition(vs, grand, full_run=True)
    assert [v.path for v in fatal] == ["b.md"], "off-list body rule must be fatal"
    assert [v.path for v in warned] == ["a.md"], "on-list body rule must only warn"
    assert stale == [], "a.md still offends, so it is not stale"
    # A delimiter violation is fatal even on a grandfathered file.
    fatal2, _, _ = partition([V("a.md", 1, "inline", "$x$")], grand, full_run=True)
    assert [v.path for v in fatal2] == ["a.md"], "delimiter rule is fatal regardless of grandfather"
    # A grandfathered file with no body-rule hit this run is stale (must be de-listed).
    _, _, stale2 = partition([], grand, full_run=True)
    assert stale2 == ["a.md"], "clean-but-grandfathered file must be reported stale"
    # …but only on a full run — a single-file invocation must not flag the rest as stale.
    _, _, stale3 = partition([], grand, full_run=False)
    assert stale3 == [], "staleness is only meaningful over the whole tree"

    print(
        f"doc-math selftest: OK — {calls} tokenize() calls terminated; "
        f"7 rules fire over {len(expected)} scan cases + archival + ratchet, 0 false positives"
    )
    return 0


def partition(
    violations: list[Violation], grandfathered: frozenset[str], full_run: bool
) -> tuple[list[Violation], list[Violation], list[str]]:
    """Split violations into (fatal, warned, stale) under the grandfather ratchet.

    Delimiter rules are always fatal.  A body rule is fatal on a file that is not grandfathered and
    a mere warning on one that is.  When the whole tree is scanned (`full_run`), a grandfathered
    file that trips *no* body rule is `stale` — it must be dropped from the list so the set only
    ever shrinks; the overhaul's final phase asserts it empty.
    """
    fatal = [v for v in violations if not (v.kind in BODY_RULES and v.path in grandfathered)]
    warned = [v for v in violations if v.kind in BODY_RULES and v.path in grandfathered]
    stale: list[str] = []
    if full_run:
        still_offending = {v.path for v in violations if v.kind in BODY_RULES}
        stale = sorted(grandfathered - still_offending)
    return fatal, warned, stale


def tracked_markdown() -> list[Path]:
    out = subprocess.run(
        ["git", "ls-files", "-z", "*.md"], capture_output=True, text=True, check=True
    ).stdout
    return [Path(p) for p in out.split("\0") if p]


def main(argv: list[str]) -> int:
    if "--selftest" in argv[1:]:
        return selftest()

    explicit = [a for a in argv[1:] if not a.startswith("-")]
    paths = [Path(a) for a in explicit] or tracked_markdown()
    full_run = not explicit  # the staleness check is only meaningful over the whole tracked tree

    violations = [v for p in paths for v in scan(p)]
    fatal, warned, stale = partition(violations, GRANDFATHERED, full_run)

    for v in fatal:
        print(f"{v.path}:{v.line}: {v.kind} is GitHub-unsafe — {FIXES[v.kind]}\n    {v.snippet}")

    if warned:
        wfiles = len({v.path for v in warned})
        # On a targeted run (explicit file args) print each site — this is the author's worklist for
        # cleaning a grandfathered file.  On a full-tree run keep it terse, to avoid CI noise.
        if not full_run:
            for v in warned:
                print(f"{v.path}:{v.line}: {v.kind} (grandfathered, report-only) — {FIXES[v.kind]}\n    {v.snippet}")
        print(
            f"\nnote: {len(warned)} grandfathered body-rule site(s) in {wfiles} file(s) "
            "(report-only until that file is cleaned; see GRANDFATHERED in this script).",
            file=sys.stderr,
        )

    if stale:
        print(
            "\nFAIL: these files are grandfathered but now clean — remove them from GRANDFATHERED:\n  "
            + "\n  ".join(stale),
            file=sys.stderr,
        )

    if not fatal and not stale:
        msg = f"doc-math: OK — {len(paths)} file(s), no GitHub-unsafe math"
        if warned:
            msg += f" ({len(warned)} grandfathered body-rule site(s) pending, report-only)"
        print(msg)
        return 0

    if fatal:
        files = len({v.path for v in fatal})
        print(
            f"\nFAIL: {len(fatal)} GitHub-unsafe math site(s) in {files} file(s).\n"
            "GitHub strips backslash-escapes inside $…$ and $$…$$ before MathJax sees them,\n"
            "corrupting \\_ \\{ \\} \\; \\, \\# — loudly (parse error) or silently (wrong output).\n"
            "Delimiters: inline $`…`$ · display ```math fence · literal dollars in inline code.\n"
            "Bodies: \\lvert…\\rvert not \\mid · ASCII LaTeX not unicode · $`O(1)`$ not `O(1)`.\n"
            "See docs/notation.md.",
            file=sys.stderr,
        )
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
