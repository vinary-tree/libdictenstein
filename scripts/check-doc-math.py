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

# A fence opener/closer: three backticks, optionally indented, plus an info string.
FENCE = re.compile(r"^\s*```")

# A raw-HTML block line.  `$…$` inside an HTML attribute (e.g. an <img alt="…">
# describing a diagram) is never math — GitHub renders attributes verbatim — so such
# lines are skipped wholesale.  We match at line level rather than per-tag on purpose:
# a `<…>` tag regex would happily swallow `Option<V>` and `Vec<u8>` out of real math.
RAW_HTML_LINE = re.compile(r"^\s*</?[A-Za-z]")


class Token(NamedTuple):
    kind: str  # 'text' | 'esc' | 'code' | 'safe' | 'inline' | 'display'
    text: str  # the raw source slice
    body: str  # for 'inline'/'display': the math body, delimiters stripped


class Violation(NamedTuple):
    path: str
    line: int
    kind: str  # 'inline' | 'display' | 'letter-before' | 'literal-dollar'
    snippet: str


FIXES = {
    "inline": "use $`…`$",
    "display": "use a ```math fence",
    "letter-before": "insert a space before the opening `$` (GitHub renders no math otherwise)",
    "literal-dollar": "wrap each literal dollar in inline code, e.g. `$₁`",
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


def scan(path: Path) -> Iterator[Violation]:
    """Yield every GitHub-unsafe math site in `path`."""
    lines = path.read_text(errors="replace").split("\n")
    in_fence = False
    open_run = 0  # a code span carried over from the previous line
    i = 0
    while i < len(lines):
        line = lines[i]
        stripped = line.strip()

        if FENCE.match(line):
            in_fence = not in_fence
            open_run = 0
            i += 1
            continue
        if in_fence or RAW_HTML_LINE.match(line):
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

        toks, open_run = tokenize(line, open_run)
        literal_dollars = 0
        col = 0
        for tok in toks:
            if tok.kind in ("inline", "display"):
                yield Violation(str(path), i + 1, tok.kind, tok.text)
            elif tok.kind == "esc" and tok.text == "\\$":
                literal_dollars += 1
            elif tok.kind == "safe" and col > 0 and line[col - 1].isascii() and line[col - 1].isalpha():
                # GitHub declines to open inline math when the `$` abuts an ASCII letter.
                yield Violation(str(path), i + 1, "letter-before", line[max(0, col - 12) : col + len(tok.text)])
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
    }
    with tempfile.TemporaryDirectory() as td:
        for i, (src, want) in enumerate(expected.items()):
            p = Path(td) / f"t{i}.md"
            p.write_text(src + "\n")
            got = {v.kind for v in scan(p)}
            assert got == want, f"{src!r}: expected {want or '{}'} , got {got or '{}'}"

    print(f"doc-math selftest: OK — {calls} tokenize() calls terminated; 4 rules fire, 0 false positives")
    return 0


def tracked_markdown() -> list[Path]:
    out = subprocess.run(
        ["git", "ls-files", "-z", "*.md"], capture_output=True, text=True, check=True
    ).stdout
    return [Path(p) for p in out.split("\0") if p]


def main(argv: list[str]) -> int:
    if "--selftest" in argv[1:]:
        return selftest()

    paths = [Path(a) for a in argv[1:]] or tracked_markdown()

    violations = [v for p in paths for v in scan(p)]
    if not violations:
        print(f"doc-math: OK — {len(paths)} file(s), no GitHub-unsafe math delimiters")
        return 0

    for v in violations:
        print(f"{v.path}:{v.line}: {v.kind} is GitHub-unsafe — {FIXES[v.kind]}\n    {v.snippet}")

    files = len({v.path for v in violations})
    print(
        f"\nFAIL: {len(violations)} GitHub-unsafe math site(s) in {files} file(s).\n"
        "GitHub strips backslash-escapes inside $…$ and $$…$$ before MathJax sees them,\n"
        "corrupting \\_ \\{ \\} \\; \\, \\# — loudly (parse error) or silently (wrong output).\n"
        "Safe forms: inline $`…`$ · display ```math fence · literal dollars in inline code.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
