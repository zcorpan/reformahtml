#!/usr/bin/env python3
# reformahtml.py
#
# Usage:
#   python reformahtml.py input.html              # overwrites input.html in place
#   python reformahtml.py input.html output.html  # writes to specified output file
#
# What this does:
# - Collapses whitespace INSIDE TAGS:
#     • Outside quotes: any \s+ → single space
#     • Inside quotes: only runs that include a newline → single space
# - Collapses intra-paragraph newlines in TEXT NODES to spaces,
#   BUT preserves the entire trailing suffix (newline(s) + indentation) when the
#   next tag is “structural” (see sets below). This keeps blank lines & layout.
# - Preserves whitespace-only text nodes exactly (all empty lines/indentation).
# - Skips everything verbatim within any element carrying data-noreformat.
# - Leaves comments and <pre>, <textarea>, <script>, <style> as-is.

import sys
import re
from pathlib import Path
from typing import List

# Raw-text tags (never touch their content)
RAW_TEXT_TAGS = {"pre", "textarea", "script", "style"}

# Treat these start or end tags as structural boundaries where we must preserve
# surrounding empty lines / indentation exactly.
STRUCTURAL_START = {
    # sectioning / grouping / headings
    "address","article","aside","blockquote","details","dialog","div","dl","dt","dd",
    "fieldset","figcaption","figure","footer","form","h1","h2","h3","h4","h5","h6",
    "header","hgroup","hr","main","menu","nav","ol","p","pre","search","section",
    # tables & lists & ruby
    "table","thead","tbody","tfoot","tr","td","th","caption","colgroup",
    "ul","li","optgroup","option","ruby","rt","rp"
}
STRUCTURAL_END = {
    # common containers worth preserving before their end tags
    "dl","ol","ul","table","thead","tbody","tfoot","tr","td","th",
    "caption","colgroup","ruby","optgroup","select","p"
}

# Void elements (not pushed to open stack)
VOID_ELEMENTS = {
    "area","base","br","col","embed","hr","img","input","link","meta",
    "param","source","track","wbr"
}

# Text-node collapsing: only collapse runs that contain at least one newline.
_TEXT_WS_WITH_NL = re.compile(r"[ \t]*\n+[ \t]*")

# data-noreformat (case-insensitive): skip reformatting in that subtree
_HAS_NOREFORMAT = re.compile(r"\bdata-noreformat\b", re.IGNORECASE)


def collapse_nl_runs_to_space(s: str) -> str:
    return _TEXT_WS_WITH_NL.sub(" ", s)


def find_tag_end(s: str, i_lt: int) -> int:
    """Find the index of '>' starting search at position i_lt ('<'), honoring quotes."""
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
    return n - 1  # fallback


def extract_tag_name(tag: str) -> str:
    """Return lowercased tag name (for start or end), '' if not found."""
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


def normalize_inside_tag(tag: str) -> str:
    """
    Normalize whitespace inside a tag:
      - Outside quotes: collapse any whitespace runs to single space
      - Inside quotes: collapse only runs that include a newline to a single space
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
                while j < n and inner[j].isspace():
                    j += 1
                if not (out and out[-1] == ' '):
                    out.append(' ')
                i = j
            else:
                out.append(ch)
                i += 1

    inner_norm = ''.join(out).strip(' ')
    return '<' + inner_norm + '>'


def transform(html: str) -> str:
    out: List[str] = []
    i = 0
    n = len(html)

    raw_stack: List[str] = []        # track raw-text contexts
    noreformat_stack: List[str] = [] # track data-noreformat subtrees

    while i < n:
        # Comments: copy verbatim
        if html.startswith('<!--', i):
            j = html.find('-->', i + 4)
            if j == -1:
                out.append(html[i:])
                break
            out.append(html[i:j+3])
            i = j + 3
            continue

        if html[i] == '<':
            # Parse tag
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
                    # pop last matching
                    for k in range(len(raw_stack)-1, -1, -1):
                        if raw_stack[k] == name:
                            del raw_stack[k:]
                            break
                elif not self_closing:
                    raw_stack.append(name)

            # data-noreformat subtree tracking
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

        # Otherwise: possibly preserve trailing suffix if the next tag is structural
        preserve_trailing_suffix = False
        name_ahead = ''
        end_ahead = False
        if next_lt != -1:
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
            # collapse only inside the head, keep suffix verbatim
            if head:
                out.append(collapse_nl_runs_to_space(head))
            out.append(suffix)
        else:
            # No structural boundary ahead: collapse all newline-containing runs
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
