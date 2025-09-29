#!/usr/bin/env python3
# reformahtml.py
#
# Usage:
#   python reformahtml.py input.html              # overwrites input.html in place
#   python reformahtml.py input.html output.html  # writes to specified output file
#
# Behavior summary (highlights):
# - Collapses intra-paragraph line breaks to spaces while preserving indentation/blank lines
#   around structural HTML tags and standalone HTML comments.
# - Inside tags:
#     • Outside quotes: collapse any whitespace runs → single space, EXCEPT when a newline-run
#       is immediately before/after '=' → insert nothing there.
#     • Inside quotes: collapse only runs that include a newline → single space.
# - HTML comments:
#     • Standalone (only whitespace before on its line, and next char after '-->' is '\n'):
#         keep verbatim and treat as a structural boundary on BOTH sides (also preserves the next
#         text node’s leading indentation/newlines).
#     • Otherwise: reflow the comment inline (collapse newline-including runs inside it).
# - Elements with data-noreformat: copy their entire subtree verbatim.
# - RAW-TEXT tags (verbatim): pre, textarea, script, style, xmp, wpt (kept as atomic blocks).
# - Bikeshed/Markdown-aware reflow in text nodes (bullets, ordered lists, dt/dd, quotes,
#   hr, ATX/Setext headings, fenced code blocks). List items reflow wrapped lines.
# - INLINE start tags at start-of-line should reflow into previous text (single space) unless:
#     • there is a blank line before, or
#     • the previous line ends with a STRUCTURAL_START tag, or
#     • we are immediately after a standalone comment or <br>.
# - <br> preserves an immediately following '\n' (and the next text node’s leading indentation).
# - Includes 'foreignobject' in STRUCTURAL_START.

import sys
import re
from pathlib import Path
from typing import List

# ---------------- Core sets / regex ----------------

RAW_TEXT_TAGS = {"pre", "textarea", "script", "style", "xmp", "wpt"}

# Inline HTML elements that should NOT start a new block when they begin a line
INLINE_ELEMENTS = {
    "a","abbr","b","bdi","bdo","cite","code","data","del","dfn","em","i","ins",
    "kbd","mark","q","s","samp","small","span","strong","sub","sup","time","u","var"
}

# Structural HTML boundaries (start/end tags that should preserve surrounding whitespace)
STRUCTURAL_START = {
    "address","article","aside","blockquote","details","dialog","div","dl","dt","dd",
    "fieldset","figcaption","figure","footer","form","h1","h2","h3","h4","h5","h6",
    "header","hgroup","hr","main","menu","nav","ol","p","pre","search","section",
    "table","thead","tbody","tfoot","tr","td","th","caption","colgroup",
    "ul","li","optgroup","option","ruby","rt","rp",
    "foreignobject"
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

# Markdown patterns (compiled once)
_RE_FENCE_OPEN = re.compile(r"^[ \t]*(?P<delim>`{3,}|~{3,})(?P<rest>.*)$")
def _mk_fence_close_re(ch: str, n: int) -> re.Pattern:
    return re.compile(r"^[ \t]*" + re.escape(ch) + r"{" + str(n) + r",}[ \t]*$")

_RE_ATX_HEADING = re.compile(r"^[ \t]*#{1,6}[ \t]+")
_RE_BULLET = re.compile(r"^([ \t]*)([*-])[ \t]+(.*)$")
_RE_OL = re.compile(r"^([ \t]*)(\d+\.)[ \t]+(.*)$")
_RE_DT = re.compile(r"^[ \t]*:[ \t]+")
_RE_DD = re.compile(r"^[ \t]*::[ \t]+")
_RE_BLOCKQUOTE = re.compile(r"^[ \t]*>[ \t]?")

def _is_hr_line(line_stripped: str) -> bool:
    s = ''.join(ch for ch in line_stripped if ch not in ' \t')
    return len(s) >= 3 and len(set(s)) == 1 and s[0] in '*-_'

def _is_setext_underline(line_stripped: str) -> bool:
    s = ''.join(ch for ch in line_stripped if ch not in ' \t')
    return len(s) >= 2 and set(s) <= {'-', '='}

# ---------------- Utilities ----------------

def collapse_nl_runs_to_space(s: str) -> str:
    return _TEXT_WS_WITH_NL.sub(" ", s)

def find_tag_end(s: str, i_lt: int) -> int:
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

def _count_trailing_lfs_ignoring_spaces(s: str) -> int:
    """Count trailing '\n' in s, ignoring trailing spaces/tabs after them."""
    t = s.rstrip(' \t')
    k = 0
    i = len(t) - 1
    while i >= 0 and t[i] == '\n':
        k += 1
        i -= 1
    return k

# ---------------- Tag normalization ----------------

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
                    pass  # newline-run touching '=' → no space
                else:
                    if not (out and out[-1] == ' '):
                        out.append(' ')
                i = j
            else:
                out.append(ch)
                i += 1

    inner_norm = ''.join(out).strip(' ')
    return '<' + inner_norm + '>'

# ---------------- Comment handling ----------------

def _comment_is_standalone(html: str, i_start: int, j_end: int) -> bool:
    line_start = html.rfind('\n', 0, i_start) + 1
    before = html[line_start:i_start]
    only_ws_before = (before.strip(' \t') == '')
    next_char = html[j_end+3] if j_end + 3 < len(html) else ''
    next_is_lf = (next_char == '\n')
    return only_ws_before and next_is_lf

def _reflow_comment_inline(comment_text: str) -> str:
    inner = comment_text[4:-3]
    inner_norm = collapse_nl_runs_to_space(inner)
    return "<!--" + inner_norm + "-->"

# ---------------- Markdown-aware reflow ----------------

def _reflow_markdown_text(text: str) -> str:
    """
    Bikeshed-flavored Markdown reflow with list handling:
      - Join lines within a paragraph (single newlines) with a single space.
      - Preserve blank lines exactly.
      - Recognize bullets/ol/dt/dd/blockquote/hr/ATX & Setext headings as structural.
      - Reflow wrapped list items: lines following a list marker that are not blank/structural
        are joined into the same item with single spaces.
      - Copy fenced code blocks (``` or ~~~) verbatim until the matching close.
      - Preserve paragraph separation (do not lose blank lines).
      - Preserve edge spaces at paragraph boundaries (no trimming that eats spaces around inline HTML).
    """
    if not text:
        return text

    lines = text.splitlines(keepends=True)
    out: List[str] = []
    para_parts: List[str] = []
    in_fence = False
    fence_char = ''
    fence_len = 0
    prev_nonblank_was_paragraph = False

    def flush_para(add_trailing_nl: bool = False):
        nonlocal para_parts, out
        if not para_parts:
            return
        if len(para_parts) == 1:
            body = para_parts[0]
        else:
            first = para_parts[0].rstrip(' \t')
            rest = [s.lstrip(' \t') for s in para_parts[1:]]
            body = first
            for s in rest:
                body += ' ' + s
        out.append(body)
        if add_trailing_nl:
            out.append('\n')
        para_parts = []

    i = 0
    L = len(lines)
    while i < L:
        raw = lines[i]
        had_nl = raw.endswith('\n')
        line_no_nl = raw[:-1] if had_nl else raw
        line_stripped_ws = line_no_nl.strip()

        # Fenced code blocks
        if in_fence:
            if _mk_fence_close_re(fence_char, fence_len).match(line_no_nl):
                flush_para()
                out.append(raw)
                in_fence = False
                fence_char = ''
                fence_len = 0
            else:
                out.append(raw)
            i += 1
            prev_nonblank_was_paragraph = False
            continue

        # Blank line
        if line_stripped_ws == '':
            flush_para(add_trailing_nl=True)
            out.append(raw)  # preserve blank line exactly
            i += 1
            prev_nonblank_was_paragraph = False
            continue

        # Fence open?
        m_f = _RE_FENCE_OPEN.match(line_no_nl)
        if m_f:
            flush_para()
            delim = m_f.group('delim')
            fence_char = delim[0]
            fence_len = len(delim)
            in_fence = True
            out.append(raw)  # opening fence verbatim
            i += 1
            prev_nonblank_was_paragraph = False
            continue

        # Headings / hr / quotes / dt / dd (structural lines kept verbatim)
        if (_RE_ATX_HEADING.match(line_no_nl) or
            _RE_DT.match(line_no_nl) or
            _RE_DD.match(line_no_nl) or
            _RE_BLOCKQUOTE.match(line_no_nl) or
            _is_hr_line(line_stripped_ws) or
            (_is_setext_underline(line_stripped_ws) and prev_nonblank_was_paragraph)):
            flush_para(add_trailing_nl=True)
            out.append(raw)
            i += 1
            prev_nonblank_was_paragraph = False
            continue

        # BULLET or ORDERED LIST ITEM — reflow wrapped continuation lines
        m_b = _RE_BULLET.match(line_no_nl)
        m_o = _RE_OL.match(line_no_nl)
        if m_b or m_o:
            flush_para(add_trailing_nl=True)
            indent, marker, first_text = (m_b.groups() if m_b else m_o.groups())
            item_prefix = f"{indent}{marker} "
            contents = [first_text]

            j = i + 1
            while j < L:
                nxt_raw = lines[j]
                nxt = nxt_raw[:-1] if nxt_raw.endswith('\n') else nxt_raw
                nxt_stripped = nxt.strip()

                # Stop on blank line or any structural line (including next list item)
                if nxt_stripped == '':
                    break
                if (_RE_FENCE_OPEN.match(nxt) or
                    _RE_ATX_HEADING.match(nxt) or
                    _RE_BULLET.match(nxt) or
                    _RE_OL.match(nxt) or
                    _RE_DT.match(nxt) or
                    _RE_DD.match(nxt) or
                    _RE_BLOCKQUOTE.match(nxt) or
                    _is_hr_line(nxt_stripped) or
                    (_is_setext_underline(nxt_stripped) and True)):
                    break

                contents.append(nxt.lstrip(' \t'))
                j += 1

            first = contents[0].rstrip(' \t')
            rest = [c.lstrip(' \t') for c in contents[1:]]
            joined = first
            for c in rest:
                joined += ' ' + c

            out.append(item_prefix + joined + ('\n' if had_nl else ''))
            i = j
            prev_nonblank_was_paragraph = False
            continue

        # Plain paragraph text: accumulate
        para_parts.append(line_no_nl)
        i += 1
        prev_nonblank_was_paragraph = True

    flush_para()
    return ''.join(out)

# ---------------- Helper for inline-start reflow exception ----------------

def _prev_line_ends_with_structural_start(html: str, boundary_index: int) -> bool:
    """
    Return True if the line *ending at boundary_index* ends with a STRUCTURAL_START tag.
    boundary_index should be the index right *after* the newline you're testing.
    """
    line_start = html.rfind('\n', 0, boundary_index) + 1
    if line_start == 0 and boundary_index == 0:
        return False
    prev_line = html[line_start:boundary_index]
    prev_line_rstrip = prev_line.rstrip(' \t')
    if not prev_line_rstrip.endswith('>'):
        return False
    lt = prev_line_rstrip.rfind('<')
    if lt == -1:
        return False
    tag = prev_line_rstrip[lt:]
    name = extract_tag_name(tag)
    if not name or is_end_tag(tag):
        return False
    return name in STRUCTURAL_START

# ---------------- Main transform ----------------

def transform(html: str) -> str:
    out: List[str] = []
    i = 0
    n = len(html)

    raw_stack: List[str] = []          # raw-text contexts
    noreformat_stack: List[str] = []   # data-noreformat subtree contexts
    after_standalone_comment = False   # preserve leading prefix of NEXT text node
    after_br = False                   # preserve leading prefix after <br>

    while i < n:
        # If inside a RAW-TEXT element, copy verbatim until its matching end tag.
        if raw_stack:
            top = raw_stack[-1]
            lower_html = html.lower()
            end_marker = "</" + top + ">"
            idx = lower_html.find(end_marker, i)
            if idx == -1:
                out.append(html[i:])
                break
            out.append(html[i:idx])
            k = html.find('>', idx)
            if k == -1:
                out.append(html[idx:])
                break
            end_tag_text = html[idx:k+1]
            out.append(normalize_inside_tag(end_tag_text))
            raw_stack.pop()
            after_standalone_comment = False
            after_br = False
            i = k + 1
            continue

        # Comments
        if html.startswith('<!--', i):
            j = html.find('-->', i + 4)
            if j == -1:
                out.append(html[i:])
                break

            comment_seg = html[i:j+3]
            standalone = _comment_is_standalone(html, i, j)

            if noreformat_stack:
                out.append(comment_seg)
            else:
                if standalone:
                    out.append(comment_seg)
                    after_standalone_comment = True
                else:
                    out.append(_reflow_comment_inline(comment_seg))
                    after_standalone_comment = False

            i = j + 3
            continue

        # Tags
        if html[i] == '<':
            j = find_tag_end(html, i)
            tag = html[i:j+1]
            name = extract_tag_name(tag)
            end_tag = is_end_tag(tag)
            self_closing = is_self_closing(tag)

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

            # <br> should preserve a following linebreak (if present)
            if not end_tag and name == 'br':
                if j + 1 < n and html[j + 1] == '\n':
                    out.append('\n')
                    i = j + 2
                    after_br = True
                    continue
                else:
                    after_br = True

            after_standalone_comment = False
            i = j + 1
            continue

        # Text node
        next_lt = html.find('<', i)
        chunk = html[i:] if next_lt == -1 else html[i:next_lt]

        # In data-noreformat: copy text verbatim
        if noreformat_stack:
            out.append(chunk)
            if next_lt == -1:
                break
            i = next_lt
            continue

        # Peek ahead to classify the next tag/comment for whitespace-only handling
        name_ahead = ''
        end_ahead = False
        ahead_is_standalone_comment = False
        ahead_is_inline_start = False
        if next_lt != -1:
            if html.startswith('<!--', next_lt):
                j2 = html.find('-->', next_lt + 4)
                if j2 != -1 and _comment_is_standalone(html, next_lt, j2):
                    ahead_is_standalone_comment = True
            else:
                j2 = find_tag_end(html, next_lt)
                tag_ahead = html[next_lt:j2+1]
                name_ahead = extract_tag_name(tag_ahead)
                end_ahead = is_end_tag(tag_ahead)
                if name_ahead and not end_ahead and name_ahead in INLINE_ELEMENTS:
                    ahead_is_inline_start = True

        # Whitespace-only chunk (indentation / blank lines)
        if chunk.strip() == '':
            if next_lt != -1:
                if ahead_is_standalone_comment:
                    out.append(chunk)
                else:
                    is_structural_ahead = False
                    if not end_ahead and name_ahead in STRUCTURAL_START:
                        is_structural_ahead = True
                    if end_ahead and name_ahead in STRUCTURAL_END:
                        is_structural_ahead = True

                    if is_structural_ahead:
                        out.append(chunk)
                    elif ahead_is_inline_start:
                        nl_count = chunk.count('\n')
                        if nl_count == 1:
                            # Evaluate at the boundary (next_lt)
                            if _prev_line_ends_with_structural_start(html, next_lt):
                                out.append(chunk)
                            else:
                                out.append(' ')
                        else:
                            out.append(chunk)
                    else:
                        out.append(chunk)
            else:
                out.append(chunk)

            if next_lt == -1:
                break
            i = next_lt
            continue

        # Decide whether to preserve trailing suffix (all trailing LFs/indentation)
        preserve_trailing_suffix = False
        if next_lt != -1:
            if ahead_is_standalone_comment:
                preserve_trailing_suffix = True
            else:
                j2 = find_tag_end(html, next_lt)
                tag_ahead = html[next_lt:j2+1]
                name_ahead = extract_tag_name(tag_ahead)
                end_ahead = is_end_tag(tag_ahead)
                if name_ahead:
                    if (not end_ahead and name_ahead in STRUCTURAL_START) or (end_ahead and name_ahead in STRUCTURAL_END):
                        preserve_trailing_suffix = True

        # Preserve leading prefix if immediately after a standalone comment or <br>
        preserve_leading_prefix = after_standalone_comment or after_br

        if preserve_leading_prefix or preserve_trailing_suffix:
            left = 0
            right = len(chunk)

            # Leading prefix (all starting whitespace)
            prefix = ''
            if preserve_leading_prefix:
                while left < right and chunk[left] in (' ', '\t', '\n'):
                    left += 1
                prefix = chunk[:left]

            # Trailing suffix (all ending whitespace) — only if it contains at least one newline
            suffix = ''
            if preserve_trailing_suffix:
                idx = right - 1
                has_nl = False
                while idx >= left and chunk[idx] in (' ', '\t', '\n'):
                    if chunk[idx] == '\n':
                        has_nl = True
                    idx -= 1
                if has_nl:
                    suffix = chunk[idx+1:]
                    right = idx + 1

            body = chunk[left:right]
            if prefix:
                out.append(prefix)
            if body:
                out.append(_reflow_markdown_text(body))
            if suffix:
                out.append(suffix)
        else:
            # Preserve non-newline edge spaces around tags
            m_lead = re.match(r'[ \t]+', chunk)
            m_trail = re.search(r'[ \t]+$', chunk)
            lead = m_lead.group(0) if m_lead else ''
            trail = m_trail.group(0) if m_trail else ''
            body = chunk[len(lead): len(chunk) - (len(trail) if trail else 0)]

            # Soft-wrap at the *start* of this chunk (rare case)
            if (body.startswith('\n') and (len(body) < 2 or body[1] != '\n')
                and not _prev_line_ends_with_structural_start(html, i)
                and not after_br and not after_standalone_comment):
                body = ' ' + body[1:].lstrip(' \t')

            body_reflowed = _reflow_markdown_text(body)

            # If this chunk ends with a single LF (ignoring spaces) and the next token
            # is an INLINE start tag, collapse that LF (+ any indent) to a single space.
            trailing_lfs = _count_trailing_lfs_ignoring_spaces(chunk)
            if ahead_is_inline_start and trailing_lfs == 1 and not _prev_line_ends_with_structural_start(html, i + len(chunk)):
                # Remove exactly one trailing newline (and any trailing spaces after it) from the body output
                body_reflowed = re.sub(r"\n[ \t]*$", "", body_reflowed)
                out.append(lead + body_reflowed + ' ')
            else:
                out.append(lead + body_reflowed + trail)

        after_standalone_comment = False
        after_br = False  # consumed the preservation for <br>

        if next_lt == -1:
            break
        i = next_lt

    return ''.join(out)

# ---------------- Entrypoint ----------------

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
