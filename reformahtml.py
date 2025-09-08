#!/usr/bin/env python3
# reformahtml.py
#
# Usage:
#   python reformahtml.py input.html > output.html
#
# Behavior:
# - Replaces '\n' (LF) inside tags and inside non-whitespace text with a single space.
# - Leaves whitespace-only text (indent/formatting between tags) exactly as-is.
# - Doesn't alter <!-- comments -->.
# - Leaves <pre>, <textarea>, <script>, <style> content unchanged.

import sys, re
from typing import List

RAW_TEXT_TAGS = {"pre", "textarea", "script", "style"}

_ws_nl_run = re.compile(r'[ \t]*\n+[ \t]*')  # any run containing at least one LF -> one space

def collapse_nl_runs_to_space(s: str) -> str:
    return _ws_nl_run.sub(' ', s)

def find_tag_end(s: str, i_lt: int) -> int:
    i = i_lt + 1
    n = len(s)
    quote = None
    while i < n:
        ch = s[i]
        if quote:
            if ch == quote:
                quote = None
            i += 1
        else:
            if ch in ('"', "'"):
                quote = ch
                i += 1
            elif ch == '>':
                return i
            else:
                i += 1
    return n - 1  # fallback; malformed input

def extract_tag_name(tag: str) -> str:
    i = 1
    n = len(tag)
    if i < n and tag[i] == '/':
        i += 1
    while i < n and tag[i] in ' \t\n':
        i += 1
    start = i
    while i < n and (tag[i].isalnum() or tag[i] in '-_:'):
        i += 1
    return tag[start:i].lower()

def is_end_tag(tag: str) -> bool:
    return len(tag) >= 2 and tag[1] == '/'

def normalize_inside_tag(tag: str) -> str:
    # Replace any whitespace run that includes at least one LF with a single space,
    # but otherwise keep spaces/tabs as-is. Respect quotes.
    out: List[str] = []
    i, n, quote = 0, len(tag), None
    while i < n:
        ch = tag[i]
        if quote:
            if ch == '\n' or ch in ' \t':
                j = i
                saw_nl = False
                while j < n and tag[j] in ' \t\n':
                    if tag[j] == '\n':
                        saw_nl = True
                    j += 1
                if saw_nl:
                    if not (out and out[-1] == ' '):
                        out.append(' ')
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
            elif ch == '\n' or ch in ' \t':
                j = i
                saw_nl = False
                while j < n and tag[j] in ' \t\n':
                    if tag[j] == '\n':
                        saw_nl = True
                    j += 1
                if saw_nl:
                    if not (out and out[-1] == ' '):
                        out.append(' ')
                    i = j
                else:
                    out.append(tag[i:j])  # keep plain spaces/tabs
                    i = j
            else:
                out.append(ch)
                i += 1
    return ''.join(out)

def transform(html: str) -> str:
    out: List[str] = []
    i, n = 0, len(html)
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
            if name in RAW_TEXT_TAGS:
                if is_end_tag(tag):
                    for k in range(len(raw_stack)-1, -1, -1):
                        if raw_stack[k] == name:
                            del raw_stack[k:]
                            break
                else:
                    raw_stack.append(name)
            i = j + 1
            continue

        # Text nodes
        next_lt = html.find('<', i)
        if next_lt == -1:
            chunk = html[i:]
            if raw_stack or chunk.strip() == '':
                out.append(chunk)
            else:
                out.append(collapse_nl_runs_to_space(chunk))
            break

        chunk = html[i:next_lt]
        if raw_stack or chunk.strip() == '':
            out.append(chunk)
        else:
            out.append(collapse_nl_runs_to_space(chunk))
        i = next_lt

    return ''.join(out)

def main():
    import sys
    inp = sys.stdin.read() if not sys.stdin.isatty() else open(sys.argv[1], 'r', encoding='utf-8').read()
    sys.stdout.write(transform(inp))

if __name__ == "__main__":
    main()
