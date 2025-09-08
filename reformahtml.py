#!/usr/bin/env python3
# reformahtml.py
#
# Usage:
#   python reformahtml.py input.html              # overwrites input.html in place
#   python reformahtml.py input.html output.html  # writes to specified output file
#
# Behavior:
# - Removes line breaks inside text nodes and inside tags/attributes.
# - Preserves indentation/structural newlines between tags.
# - Leaves HTML comments and <pre>, <textarea>, <script>, <style> contents untouched.

import sys
import re
from pathlib import Path
from typing import List

RAW_TEXT_TAGS = {"pre", "textarea", "script", "style"}

# Any run that contains at least one LF becomes a single space.
_WS_NL_RUN = re.compile(r"[ \t]*\n+[ \t]*")

def collapse_nl_runs_to_space(s: str) -> str:
    return _WS_NL_RUN.sub(" ", s)

def extract_tag_name(tag: str) -> str:
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

def normalize_inside_tag(tag: str) -> str:
    """Collapse any whitespace run that includes a newline into a single space,
    but preserve spaces/tabs if there was no newline. Respect quotes."""
    out: List[str] = []
    i = 0
    n = len(tag)
    quote: str | None = None

    def is_ws(ch: str) -> bool:
        return ch in (' ', '\t', '\n', '\r')

    while i < n:
        ch = tag[i]
        if quote:
            if is_ws(ch):
                j = i
                saw_nl = False
                while j < n and is_ws(tag[j]):
                    if tag[j] == '\n':
                        saw_nl = True
                    j += 1
                if saw_nl:
                    if not (out and out[-1] == ' '):
                        out.append(' ')
                    i = j
                    continue
                else:
                    out.append(tag[i:j])
                    i = j
                    continue
            out.append(ch)
            if ch == quote:
                quote = None
            i += 1
        else:
            if ch in ('"', "'"):
                quote = ch
                out.append(ch)
                i += 1
            elif is_ws(ch):
                j = i
                saw_nl = False
                while j < n and is_ws(tag[j]):
                    if tag[j] == '\n':
                        saw_nl = True
                    j += 1
                if saw_nl:
                    if not (out and out[-1] == ' '):
                        out.append(' ')
                else:
                    out.append(tag[i:j])
                i = j
            else:
                out.append(ch)
                i += 1
    return ''.join(out)

def find_tag_end(s: str, i_lt: int) -> int:
    i = i_lt + 1
    n = len(s)
    quote: str | None = None
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

def transform(html: str) -> str:
    out: List[str] = []
    i = 0
    n = len(html)
    raw_stack: List[str] = []

    while i < n:
        # Preserve comments verbatim
        if html.startswith('<!--', i):
            j = html.find('-->', i + 4)
            if j == -1:
                out.append(html[i:])
                break
            out.append(html[i:j+3])
            i = j + 3
            continue

        # Tags
        if html[i] == '<':
            j = find_tag_end(html, i)
            tag = html[i:j+1]
            out.append(normalize_inside_tag(tag))
            name = extract_tag_name(tag)
            if name:
                if is_end_tag(tag):
                    for idx in range(len(raw_stack) - 1, -1, -1):
                        if raw_stack[idx] == name:
                            del raw_stack[idx:]
                            break
                else:
                    if name in RAW_TEXT_TAGS:
                        raw_stack.append(name)
            i = j + 1
            continue

        # Text nodes
        next_lt = html.find('<', i)
        chunk = html[i:] if next_lt == -1 else html[i:next_lt]
        if raw_stack or chunk.strip() == '':
            out.append(chunk)
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
