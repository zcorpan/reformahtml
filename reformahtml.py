#!/usr/bin/env python3
# reformahtml.py
#
# Usage:
#   python reformahtml.py input.html              # overwrites input.html in place
#   python reformahtml.py input.html output.html  # writes to specified output file
#
# Behavior summary:
# - Collapses intra-paragraph LFs to spaces while preserving indentation/blank lines
#   around structural tags and standalone comments.
# - Inside tags:
#     • Outside quotes: collapse any \s+ → single space, EXCEPT when a newline-run
#       sits immediately before/after '=' → insert nothing.
#     • Inside quotes: collapse only runs that include a newline → single space.
# - HTML comments:
#     • Standalone (only whitespace before on its line, and next char after '-->' is '\n'):
#         - keep verbatim and treat as a structural boundary (preserve whitespace before it).
#     • Otherwise: reflow inline (collapse newline-including runs inside the comment).
# - Elements with data-noreformat: copy their entire subtree verbatim.
# - Leave <pre>, <textarea>, <script>, <style> content untouched.

import sys
import re
from pathlib import Path
from typing import List

RAW_TEXT_TAGS = {"pre", "textarea", "script", "style"}

# Structural boundaries (start/end tags that should preserve surrounding whitespace)
STRUCTURAL_START = {
    "address","article","aside","blockquote","details","dialog","div","dl","dt","dd",
    "fieldset","figcaption","figure","footer","form","h1","h2","h3","h4","h5","h6",
    "header","hgroup","hr","main","menu","nav","ol","p","pre","search","section",
    "table","thead","tbody","tfoot","tr","td","th","caption","colgroup",
    "ul","li","optgroup","option","ruby","rt","rp"
}
STRUCTURAL_END = {
    "dl","ol","ul","table","thead","tbody","tfoot","tr","td","th",
    "caption","colgroup","ruby","optgroup","select","p"
}

VOID_ELEMENTS = {
    "area","base","br","col","embed","hr","img","input","link","meta",
    "param","source","track","wbr"
}

# In text nodes: collapse only runs that include at least one LF
_TEXT_WS_WITH_NL = re.compile(r"[ \t]*\n+[ \t]*")

# data-noreformat attribute (case-insensitive)
_HAS_NOREFORMAT = re.compile(r"\bdata-noreformat\b", re.IGNORECASE)


def collapse_nl_runs_to_space(s: str) -> str:
    return _TEXT_WS_WITH_NL.sub(" ", s)


def find_tag_end(s: str, i_lt: int) -> int:
    """Return index of '>' for a tag starting at i_lt ('<'), respecting quotes."""
    i = i_lt + 1
    n = len(s)
    quote = None
    while i < n:
        ch = s[i]
        if quote:
            if ch == quote:
                quote = None
        else:
            if ch in ('"', "'"):
                quote = ch
            elif ch == '>':
                return i
        i += 1
    return n - 1


def extract_tag_name(tag: str) -> str:
    """Lowercased tag name (start or end), or '' if not a normal tag."""
    i = 1
    n = len(tag)
    if i < n and tag[i] == '/':
        i += 1
    while i < n and tag[i] in " \t\n\r":
        i += 1
    start = i
    while i < n and (tag[i].isalnum() or tag[i] in "-_:"):
        i += 1
    return tag[start:i].lower()


def is_end_tag(tag: str) -> bool:
    return len(tag) >= 2 and tag[1] == '/'


def is_self_closing(tag: str) -> bool:
    inner = tag[1:-1].strip()
    return inner.endswith('/')


def tag_has_noreformat(tag: str) -> bool:
    return bool(_HAS_NOREFORMAT.search(tag))


def _prev_nonspace(s: str, i: int) -> int:
    j = i - 1
    while j >= 0 and s[j].isspace():
        j -= 1
    return j


def _next_nonspace(s: str, i: int) -> int:
    j = i
    n = len(s)
    while j < n and s[j].isspace():
        j += 1
    return j if j < n else -1


def normalize_inside_tag(tag: str) -> str:
    """
    Normalize whitespace inside a tag:
      - Outside quotes: collapse any whitespace runs to single space,
        EXCEPT if the run contains a newline and sits immediately before or after '='
        (then insert nothing).
      - Inside quotes: collapse only runs that include a newline to a single space.
    """
    if len(tag) < 2:
        return tag
    inner = tag[1:-1]
    out: List[str] = []
    i = 0
    n = len(inner)
    quote = None

    while i < n:
        ch = inner[i]
        if quote:
            if ch == quote:
                out.append(ch)
                quote = None
                i += 1
            elif ch in (' ', '\t', '\n', '\r'):
                j = i
                saw_nl = False
                while j < n and inner[j] in (' ', '\t', '\n', '\r'):
                    if inner[j] == '\n':
                        saw_nl = True
                    j += 1
                if saw_nl:
                    if not (out and out[-1] == ' '):
                        out.append(' ')
                else:
                    out.append(inner[i:j])
                i = j
            else:
                out.append(ch)
                i += 1
        else:
            if ch in ('"', "'"):
                quote = ch
                out.append(ch)
                i += 1
            elif ch.isspace():
                j = i
                saw_nl = False
                while j < n and inner[j].isspace():
                    if inner[j] == '\n':
                        saw_nl = True
                    j += 1

                p = _prev_nonspace(inner, i)
                q = _next_nonspace(inner, j)

                if saw_nl and ((p >= 0 and inner[p] == '=') or (q != -1 and inner[q] == '=')):
                    # newline-run touching '=' → no space
                    pass
                else:
                    if not (out and out[-1] == ' '):
                        out.append(' ')
                i = j
            else:
                out.append(ch)
                i += 1

    inner_norm = ''.join(out).strip(' ')
    return '<' + inner_norm + '>'


def _comment_is_standalone(html: str, i_start: int, j_end: int) -> bool:
    """
    Return True if the <!-- ... --> at [i_start, j_end+3) is only preceded by
    whitespace on its line, AND the next character after '-->' is LF.
    """
    line_start = html.rfind('\n', 0, i_start) + 1
    before = html[line_start:i_start]
    only_ws_before = (before.strip(' \t') == '')
    next_char = html[j_end+3] if j_end + 3 < len(html) else ''
    next_is_lf = (next_char == '\n')
    return only_ws_before and next_is_lf


def _reflow_comment_inline(comment_text: str) -> str:
    """
    Collapse newline-including whitespace runs inside the comment's content to a single space.
    Keep other spacing as-is.
    """
    inner = comment_text[4:-3]  # strip <!-- -->
    inner_norm = collapse_nl_runs_to_space(inner)
    return "<!--" + inner_norm + "-->"


def transform(html: str) -> str:
    out: List[str] = []
    i = 0
    n = len(html)

    raw_stack: List[str] = []        # raw-text contexts
    noreformat_stack: List[str] = [] # data-noreformat subtree contexts

    while i < n:
        # Comments
        if html.startswith('<!--', i):
            j = html.find('-->', i + 4)
            if j == -1:
                out.append(html[i:])
                break

            comment_seg = html[i:j+3]
            standalone = _comment_is_standalone(html, i, j)

            # In raw-text or data-noreformat: always verbatim
            if raw_stack or noreformat_stack:
                out.append(comment_seg)
            else:
                if standalone:
                    # Standalone line comment → preserve verbatim
                    out.append(comment_seg)
                else:
                    # Inline comment → reflow inside, no forced newlines
                    out.append(_reflow_comment_inline(comment_seg))

            i = j + 3
            continue

        # Tags
        if html[i] == '<':
            j = find_tag_end(html, i)
            tag = html[i:j+1]
            name = extract_tag_name(tag)
            end_tag = is_end_tag(tag)
            self_closing = is_self_closing(tag)

            # Emit tag (normalize unless inside data-noreformat)
            if noreformat_stack:
                out.append(tag)
            else:
                out.append(normalize_inside_tag(tag))

            # raw-text tracking
            if name in RAW_TEXT_TAGS:
                if end_tag:
                    for k in range(len(raw_stack)-1, -1, -1):
                        if raw_stack[k] == name:
                            del raw_stack[k:]
                            break
                elif not self_closing:
                    raw_stack.append(name)

            # data-noreformat tracking
            if end_tag:
                if noreformat_stack and noreformat_stack[-1] == name:
                    noreformat_stack.pop()
            else:
                if tag_has_noreformat(tag) and name and name not in VOID_ELEMENTS and not self_closing:
                    noreformat_stack.append(name)

            i = j + 1
            continue

        # Text node
        next_lt = html.find('<', i)
        chunk = html[i:] if next_lt == -1 else html[i:next_lt]

        # In raw-text or data-noreformat: copy text verbatim
        if raw_stack or noreformat_stack:
            out.append(chunk)
            if next_lt == -1:
                break
            i = next_lt
            continue

        # Whitespace-only chunk (indentation / blank lines): keep exactly
        if chunk.strip() == '':
            out.append(chunk)
            if next_lt == -1:
                break
            i = next_lt
            continue

        # Decide whether to preserve the trailing suffix (all trailing LFs/indentation)
        preserve_trailing_suffix = False
        if next_lt != -1:
            # Standalone comment ahead? → structural boundary
            if html.startswith('<!--', next_lt):
                j2 = html.find('-->', next_lt + 4)
                if j2 != -1 and _comment_is_standalone(html, next_lt, j2):
                    preserve_trailing_suffix = True
            else:
                # Structural start/end tag ahead?
                j2 = find_tag_end(html, next_lt)
                tag_ahead = html[next_lt:j2+1]
                name_ahead = extract_tag_name(tag_ahead)
                end_ahead = is_end_tag(tag_ahead)
                if name_ahead:
                    if (not end_ahead and name_ahead in STRUCTURAL_START) or (end_ahead and name_ahead in STRUCTURAL_END):
                        preserve_trailing_suffix = True

        if preserve_trailing_suffix:
            # Split into head + trailing suffix (all trailing whitespace)
            idx = len(chunk) - 1
            has_nl = False
            while idx >= 0 and chunk[idx] in (' ', '\t', '\n'):
                if chunk[idx] == '\n':
                    has_nl = True
                idx -= 1
            head = chunk[:idx+1]
            suffix = chunk[idx+1:]
            if head:
                out.append(collapse_nl_runs_to_space(head))
            out.append(suffix)
        else:
            out.append(collapse_nl_runs_to_space(chunk))

        if next_lt == -1:
            break
        i = next_lt

    return ''.join(out)


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Usage: python reformahtml.py input.html [output.html]")
        sys.exit(1)

    inp_path = Path(sys.argv[1])
    html_text = inp_path.read_text(encoding="utf-8")
    result = transform(html_text)

    if len(sys.argv) >= 3:
        Path(sys.argv[2]).write_text(result, encoding="utf-8")
    else:
        inp_path.write_text(result, encoding="utf-8")
