// src/main.rs
//
// reformahtml — fast HTML/Bikeshed reflower
//
// - Collapses intra-paragraph line breaks while preserving indentation/blank lines
//   around structural HTML tags and standalone comments.
// - Inside tags:
//     • Outside quotes: collapse any whitespace runs → single space, EXCEPT when a newline-run
//       is immediately before/after '=' → insert nothing.
//     • Inside quotes: collapse only runs that include a newline → single space.
// - HTML comments:
//     • Standalone (only whitespace before on its line, and next char after '-->' is '\n'):
//         keep verbatim and treat as a structural boundary on BOTH sides.
//     • Otherwise: reflow the comment inline (collapse newline-including runs inside it).
// - Elements with data-noreformat: copy their entire subtree verbatim.
// - RAW-TEXT tags (verbatim): pre, textarea, script, style, xmp, wpt.
// - Bikeshed/Markdown-aware reflow in text nodes (bullets, ordered lists, dt/dd, quotes,
//   hr, ATX/Setext headings, fenced code blocks). List items and dt/dd items reflow wrapped lines.
// - INLINE start tags at start-of-line soft-join into previous text unless exceptions apply.
// - <br> preserves an immediately following '\n'.
// - UTF-8 safe.
//
// CLI flags:
//   --markdown      : force-enable Markdown/Bikeshed reflow
//   --no-markdown   : force-disable Markdown/Bikeshed reflow
// Default: Markdown is enabled iff input file extension is ".bs" (case-insensitive).

use clap::{ArgAction, Parser};
use memchr::{memchr, memrchr};
use std::fs;
use std::io;
use std::path::PathBuf;

/// CLI flags
#[derive(Parser)]
#[command(author, version, about)]
struct Cli {
    /// Force-enable Bikeshed/Markdown-aware reflow
    #[arg(long, action = ArgAction::SetTrue)]
    markdown: bool,

    /// Force-disable Bikeshed/Markdown-aware reflow
    #[arg(long = "no-markdown", action = ArgAction::SetTrue)]
    no_markdown: bool,

    /// Input file
    input: PathBuf,

    /// Output file (default: overwrite input)
    output: Option<PathBuf>,
}

fn main() -> io::Result<()> {
    let cli = Cli::parse();

    let src = fs::read(&cli.input)?;
    let mut out = Vec::with_capacity(src.len() + src.len() / 20 + 2048);

    // Default: enable markdown if input ends with ".bs"
    let default_md = cli
        .input
        .extension()
        .map_or(false, |e| e.to_string_lossy().eq_ignore_ascii_case("bs"));

    // Precedence: explicit flags override default; --no-markdown wins if both are present.
    let use_markdown = if cli.no_markdown {
        false
    } else if cli.markdown {
        true
    } else {
        default_md
    };

    transform(&src, &mut out, use_markdown);

    let out_path = cli.output.as_ref().unwrap_or(&cli.input);
    fs::write(out_path, out)?;
    Ok(())
}

/* =============================== Core sets =============================== */

fn is_inline(name: &[u8]) -> bool {
    matches_ignore_ascii_case(
        name,
        &[
            b"a", b"abbr", b"b", b"bdi", b"bdo", b"cite", b"code", b"data", b"del", b"dfn", b"em",
            b"i", b"ins", b"kbd", b"mark", b"q", b"s", b"samp", b"small", b"span", b"strong",
            b"sub", b"sup", b"time", b"u", b"var", b"ref",
        ],
    )
}

fn is_void(name: &[u8]) -> bool {
    matches_ignore_ascii_case(
        name,
        &[
            b"area", b"base", b"br", b"col", b"embed", b"hr", b"img", b"input", b"link", b"meta",
            b"param", b"source", b"track", b"wbr",
        ],
    )
}

fn is_raw_text(name: &[u8]) -> bool {
    matches_ignore_ascii_case(
        name,
        &[b"pre", b"textarea", b"script", b"style", b"xmp", b"wpt"],
    )
}

fn is_structural(name: &[u8]) -> bool {
    matches_ignore_ascii_case(
        name,
        &[
            b"address", b"article", b"aside", b"blockquote", b"details", b"dialog", b"div",
            b"dl", b"dt", b"dd", b"fieldset", b"figcaption", b"figure", b"footer", b"form", b"h1",
            b"h2", b"h3", b"h4", b"h5", b"h6", b"header", b"hgroup", b"hr", b"main", b"menu",
            b"nav", b"ol", b"p", b"pre", b"search", b"section", b"table", b"thead", b"tbody",
            b"tfoot", b"tr", b"td", b"th", b"caption", b"colgroup", b"ul", b"li", b"optgroup",
            b"option", b"ruby", b"rt", b"rp", b"foreignobject",
        ],
    )
}

/* ============================ Utility predicates ========================= */

#[inline]
fn is_name_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b':'
}

#[inline]
fn is_space_tab(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

#[inline]
fn is_ws(b: u8) -> bool {
    b == b' ' || b == b'\t' || b == b'\n' || b == b'\r'
}

fn matches_ignore_ascii_case(name: &[u8], set: &[&[u8]]) -> bool {
    set.iter().any(|&s| name.eq_ignore_ascii_case(s))
}

fn trim_spaces(buf: &mut Vec<u8>) {
    let mut start = 0usize;
    while start < buf.len() && buf[start] == b' ' {
        start += 1;
    }
    let mut end = buf.len();
    while end > start && buf[end - 1] == b' ' {
        end -= 1;
    }
    if start == 0 && end == buf.len() {
        return;
    }
    let mut tmp = Vec::with_capacity(end - start);
    tmp.extend_from_slice(&buf[start..end]);
    *buf = tmp;
}

/* =============================== Tag parsing ============================= */

#[derive(Clone, Copy, Debug)]
struct TagInfo<'a> {
    name: &'a [u8],
    is_end: bool,
    self_closing: bool,
}

/// Find the '>' for a tag starting at `i` (s[i] == '<'), being quote-aware.
fn find_tag_end(s: &[u8], mut i: usize) -> Option<usize> {
    let n = s.len();
    i += 1;
    let mut quote: u8 = 0;
    while i < n {
        let b = s[i];
        if quote != 0 {
            if b == quote {
                quote = 0;
            }
        } else if b == b'"' || b == b'\'' {
            quote = b;
        } else if b == b'>' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Extract tag name, end/self-closing flags from raw `<...>` bytes.
fn parse_tag_info<'a>(tag: &'a [u8]) -> TagInfo<'a> {
    let n = tag.len();
    let mut i = 1;

    let mut is_end = false;
    if i < n && tag[i] == b'/' {
        is_end = true;
        i += 1;
    }
    while i < n && is_ws(tag[i]) {
        i += 1;
    }
    let start = i;
    while i < n && is_name_char(tag[i]) {
        i += 1;
    }
    let name = &tag[start..i];

    // self-closing? check before '>'
    let mut j = n - 1;
    while j > 0 && is_ws(tag[j - 1]) {
        j -= 1;
    }
    let self_closing = j >= 2 && tag[j - 1] == b'/';

    TagInfo {
        name,
        is_end,
        self_closing,
    }
}

/* ====================== data-noreformat attribute scan =================== */

fn tag_has_noreformat_attr(tag: &[u8]) -> bool {
    // Robust attribute scanner: [name] ( '=' [value] )?
    let len = tag.len();
    if len < 2 {
        return false;
    }
    let mut i = 1usize;

    while i < len && tag[i] != b'>' {
        // skip whitespace and slashes
        while i < len && (is_ws(tag[i]) || tag[i] == b'/') {
            i += 1;
        }
        if i >= len || tag[i] == b'>' {
            break;
        }

        // attribute name
        if !is_name_char(tag[i]) {
            // Not a valid name start; advance to avoid infinite loops.
            i += 1;
            continue;
        }
        let name_start = i;
        i += 1;
        while i < len && is_name_char(tag[i]) {
            i += 1;
        }
        let name = &tag[name_start..i];
        if name.eq_ignore_ascii_case(b"data-noreformat") {
            return true;
        }

        // skip whitespace
        while i < len && is_ws(tag[i]) {
            i += 1;
        }

        // optional "= value"
        if i < len && tag[i] == b'=' {
            i += 1;
            // skip whitespace
            while i < len && is_ws(tag[i]) {
                i += 1;
            }
            if i >= len || tag[i] == b'>' {
                break;
            }

            // quoted value
            if tag[i] == b'"' || tag[i] == b'\'' {
                let q = tag[i];
                i += 1;
                while i < len && tag[i] != q {
                    i += 1;
                }
                if i < len && tag[i] == q {
                    i += 1;
                }
            } else {
                // unquoted value
                while i < len && !is_ws(tag[i]) && tag[i] != b'>' {
                    i += 1;
                }
            }
        }
        // loop continues to parse next attribute
    }
    false
}

/* ======================== Inside-tag normalization ====================== */

fn normalize_inside_tag(tag: &[u8], out: &mut Vec<u8>) {
    if tag.len() < 2 {
        out.extend_from_slice(tag);
        return;
    }
    let inner = &tag[1..tag.len() - 1];

    let mut buf: Vec<u8> = Vec::with_capacity(inner.len());
    let mut i = 0usize;
    let n = inner.len();
    let mut quote: u8 = 0;

    let push_space_once = |buf: &mut Vec<u8>| {
        if !buf.last().map(|b| *b == b' ').unwrap_or(false) {
            buf.push(b' ');
        }
    };

    while i < n {
        let b = inner[i];
        if quote != 0 {
            if b == quote {
                buf.push(b);
                quote = 0;
                i += 1;
            } else if b == b'\n' || b == b'\r' || b == b' ' || b == b'\t' {
                let mut j = i;
                let mut saw_nl = false;
                while j < n {
                    let c = inner[j];
                    if c == b'\n' || c == b'\r' || c == b' ' || c == b'\t' {
                        if c == b'\n' {
                            saw_nl = true;
                        }
                        j += 1;
                    } else {
                        break;
                    }
                }
                if saw_nl {
                    push_space_once(&mut buf);
                } else {
                    buf.extend_from_slice(&inner[i..j]);
                }
                i = j;
            } else {
                buf.push(b);
                i += 1;
            }
            continue;
        }

        if b == b'"' || b == b'\'' {
            quote = b;
            buf.push(b);
            i += 1;
            continue;
        }

        if is_ws(b) {
            let mut j = i;
            let mut saw_nl = false;
            while j < n && is_ws(inner[j]) {
                if inner[j] == b'\n' {
                    saw_nl = true;
                }
                j += 1;
            }
            // Check neighbors around the run (outside quotes)
            let mut p = i;
            while p > 0 && is_ws(inner[p - 1]) {
                p -= 1;
            }
            let left = if p > 0 { inner[p - 1] } else { 0 };
            let mut q = j;
            while q < n && is_ws(inner[q]) {
                q += 1;
            }
            let right = if q < n { inner[q] } else { 0 };

            if saw_nl && (left == b'=' || right == b'=') {
                // newline-run touching '=' → no space
            } else {
                push_space_once(&mut buf);
            }
            i = j;
            continue;
        }

        buf.push(b);
        i += 1;
    }

    trim_spaces(&mut buf);

    out.push(b'<');
    out.extend_from_slice(&buf);
    out.push(b'>');
}

/* ============================== Comments ================================ */

/// Return (end_index_of_dash_in_terminator, is_standalone). If unterminated, end_index = usize::MAX.
fn scan_comment(s: &[u8], i: usize) -> (usize, bool) {
    // Assumes s[i..].starts_with("<!--")
    let mut k = i + 4;
    while let Some(p) = memchr(b'-', &s[k..]) {
        let j = k + p;
        if j + 2 < s.len() && s[j + 1] == b'-' && s[j + 2] == b'>' {
            // standalone if only spaces/tabs since line start AND next char after '-->' is '\n'
            let line_start = memrchr(b'\n', &s[..i]).map(|x| x + 1).unwrap_or(0);
            let mut only_ws = true;
            for &c in &s[line_start..i] {
                if !(c == b' ' || c == b'\t') {
                    only_ws = false;
                    break;
                }
            }
            let next_is_lf = if j + 3 < s.len() { s[j + 3] == b'\n' } else { false };
            return (j, only_ws && next_is_lf);
        }
        k = j + 1;
        if k >= s.len() {
            break;
        }
    }
    (usize::MAX, false)
}

fn reflow_inline_comment(comment: &[u8], out: &mut Vec<u8>) {
    // comment like <!-- ... -->
    if comment.len() < 7 {
        out.extend_from_slice(comment);
        return;
    }
    let inner = &comment[4..comment.len() - 3];
    out.extend_from_slice(b"<!--");
    let mut i = 0usize;
    let n = inner.len();
    while i < n {
        let b = inner[i];
        if b == b'\n' {
            // collapse newline + adjoining ws to a single space
            if !out.last().map(|b| *b == b' ').unwrap_or(false) {
                out.push(b' ');
            }
            i += 1;
            while i < n && (inner[i] == b' ' || inner[i] == b'\t' || inner[i] == b'\n') {
                i += 1;
            }
        } else {
            out.push(b);
            i += 1;
        }
    }
    out.extend_from_slice(b"-->");
}

/* ======================== Markdown/Bikeshed reflow ====================== */

#[derive(Clone, Copy)]
struct Fence {
    ch: u8,    // '`' or '~'
    min: usize // min count
}

fn is_hr_line_stripped(s: &str) -> bool {
    let mut c = '\0';
    let mut count = 0usize;
    for ch in s.chars() {
        if ch == ' ' || ch == '\t' { continue; }
        if c == '\0' {
            if ch == '*' || ch == '-' || ch == '_' {
                c = ch;
                count = 1;
            } else {
                return false;
            }
        } else {
            if ch != c { return false; }
            count += 1;
        }
    }
    count >= 3
}

fn is_setext_underline_stripped(s: &str) -> bool {
    let mut c = '\0';
    for ch in s.chars() {
        if ch == ' ' || ch == '\t' { continue; }
        if ch == '-' || ch == '=' {
            if c == '\0' { c = ch; }
            else if c != ch { return false; }
        } else {
            return false;
        }
    }
    let count = s.chars().filter(|&ch| ch == '-' || ch == '=').count();
    count >= 2
}

fn starts_with_bullet(line: &str) -> Option<(String, String)> {
    // ^\s*[*-]\s+
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') { i += 1; }
    if i < bytes.len() && (bytes[i] == b'*' || bytes[i] == b'-') {
        let marker = bytes[i] as char;
        i += 1;
        let mut j = i;
        if j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') { j += 1; }
            let prefix = format!("{}{} ", &line[..i-1], marker);
            let first = line[j..].to_string();
            return Some((prefix, first));
        }
    }
    None
}

fn starts_with_ol(line: &str) -> Option<(String, String)> {
    // ^\s*\d+\.\s+
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') { i += 1; }
    let indent = &line[..i];

    let mut pos = i;
    while pos < bytes.len() && bytes[pos].is_ascii_digit() { pos += 1; }
    if pos == i { return None; }
    if pos >= bytes.len() || bytes[pos] != b'.' { return None; }
    let dot_pos = pos;
    pos += 1; // skip '.'
    if pos >= bytes.len() || !(bytes[pos] == b' ' || bytes[pos] == b'\t') { return None; }
    while pos < bytes.len() && (bytes[pos] == b' ' || bytes[pos] == b'\t') { pos += 1; }

    let digits = &line[i..dot_pos];
    let prefix = format!("{indent}{digits}. ");
    let first = line[pos..].to_string();
    Some((prefix, first))
}

fn is_atx_heading(line: &str) -> bool {
    // ^\s*#{1,6}\s+
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') { i += 1; }
    let mut count = 0usize;
    while i < bytes.len() && bytes[i] == b'#' && count < 6 {
        count += 1;
        i += 1;
    }
    if count == 0 { return false; }
    i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t')
}

fn is_blockquote(line: &str) -> bool {
    // ^\s*>\s?
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') { i += 1; }
    if i < bytes.len() && bytes[i] == b'>' {
        let j = i + 1;
        if j == bytes.len() || bytes[j] == b' ' || bytes[j] == b'\t' { return true; }
    }
    false
}

fn parse_dt(line: &str) -> Option<(String, String)> {
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') { i += 1; }
    if i >= bytes.len() || bytes[i] != b':' { return None; }
    let mut j = i + 1;
    let has_extra_space = j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t');
    if has_extra_space || j == bytes.len() {
        if has_extra_space {
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') { j += 1; }
        }
        let space = if j == bytes.len() { "" } else { " " };
        let prefix = format!("{}:{space}", &line[..i]);
        let first = line[j..].to_string();
        Some((prefix, first))
    } else {
        None
    }
}

fn parse_dd(line: &str) -> Option<(String, String)> {
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') { i += 1; }
    if i + 1 >= bytes.len() || bytes[i] != b':' || bytes[i + 1] != b':' { return None; }
    let mut j = i + 2;
    let has_extra_space = j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t');
    if has_extra_space || j == bytes.len() {
        if has_extra_space {
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') { j += 1; }
        }
        let space = if j == bytes.len() { "" } else { " " };
        let prefix = format!("{}::{space}", &line[..i]);
        let first = line[j..].to_string();
        Some((prefix, first))
    } else {
        None
    }
}

fn fence_open(line: &str) -> Option<Fence> {
    // ^\s*(```+|~~~+)
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') { i += 1; }
    if i >= bytes.len() { return None; }
    if bytes[i] == b'`' || bytes[i] == b'~' {
        let ch = bytes[i];
        let mut j = i;
        while j < bytes.len() && bytes[j] == ch { j += 1; }
        if j - i >= 3 {
            return Some(Fence { ch, min: j - i });
        }
    }
    None
}

fn fence_close(line: &str, f: Fence) -> bool {
    // ^\s*<ch>{min,}\s*$
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') { i += 1; }
    let mut count = 0usize;
    while i < bytes.len() && bytes[i] == f.ch { count += 1; i += 1; }
    if count < f.min { return false; }
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') { i += 1; }
    i == bytes.len()
}

/* ---------- Helpers to keep DT/DD on their own lines during reflow ---------- */

#[inline]
fn body_begins_with_dt_or_dd_after_single_lf(body: &[u8]) -> bool {
    // Matches: "\n" + ws* + ":" [ ":" ] (space/tab or end)
    if body.is_empty() || body[0] != b'\n' { return false; }
    let mut j = 1usize;
    while j < body.len() && (body[j] == b' ' || body[j] == b'\t') { j += 1; }
    if j >= body.len() || body[j] != b':' { return false; }
    j += 1;
    if j < body.len() && body[j] == b':' { j += 1; }
    if j >= body.len() { return true; }
    body[j] == b' ' || body[j] == b'\t'
}

/// Return true if the **line containing `pos`** begins (after optional spaces/tabs)
/// with `: ` or `:: ` — i.e., a DT/DD marker. This handles the case where `pos`
/// points into the *same line* (e.g., at a `<` that follows the marker).
fn line_at_pos_starts_with_dt_or_dd(src: &[u8], pos: usize) -> bool {
    let n = src.len();
    if pos > n { return false; }
    let line_start = memrchr(b'\n', &src[..pos]).map(|x| x + 1).unwrap_or(0);
    let mut i = line_start;
    while i < n && (src[i] == b' ' || src[i] == b'\t') { i += 1; }
    if i >= n { return false; }
    if src[i] != b':' { return false; }
    i += 1;
    if i < n && src[i] == b':' { i += 1; }
    if i >= n { return true; }
    src[i] == b' ' || src[i] == b'\t'
}

/// If body starts with "\n"+indent+":"[":"], return index of the first ':' (end of indent).
#[inline]
fn leading_lf_indent_end_before_dt_or_dd(body: &[u8]) -> Option<usize> {
    if body.is_empty() || body[0] != b'\n' { return None; }
    let mut j = 1usize;
    while j < body.len() && (body[j] == b' ' || body[j] == b'\t') { j += 1; }
    if j >= body.len() || body[j] != b':' { return None; }
    // optional second ':'
    let mut k = j + 1;
    if k < body.len() && body[k] == b':' { k += 1; }
    if k < body.len() && !(body[k] == b' ' || body[k] == b'\t') {
        return None;
    }
    Some(j)
}

fn reflow_markdown_text(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }

    let mut out = String::with_capacity(text.len());
    let mut para_parts: Vec<String> = Vec::new();
    let mut in_fence: Option<Fence> = None;
    let mut prev_nonblank_was_paragraph = false;

    let mut lines_iter = text.split_inclusive('\n').peekable();

    let flush_para = |add_trailing_nl: bool, out: &mut String, para_parts: &mut Vec<String>| {
        if para_parts.is_empty() { return; }
        if para_parts.len() == 1 {
            out.push_str(&para_parts[0]);
        } else {
            let first = para_parts[0].trim_end_matches([' ', '\t']);
            out.push_str(first);
            for s in para_parts.iter().skip(1) {
                let s2 = s.trim_start_matches([' ', '\t']);
                out.push(' ');
                out.push_str(s2);
            }
        }
        if add_trailing_nl { out.push('\n'); }
        para_parts.clear();
    };

    while let Some(raw) = lines_iter.next() {
        let had_nl = raw.ends_with('\n');
        let line_no_nl = if had_nl { &raw[..raw.len()-1] } else { raw };
        let line_stripped_ws = line_no_nl.trim();

        if let Some(f) = in_fence {
            if fence_close(line_no_nl, f) {
                flush_para(false, &mut out, &mut para_parts);
                out.push_str(raw);
                in_fence = None;
                prev_nonblank_was_paragraph = false;
            } else {
                out.push_str(raw);
            }
            continue;
        }

        if line_stripped_ws.is_empty() {
            flush_para(true, &mut out, &mut para_parts);
            out.push_str(raw);
            prev_nonblank_was_paragraph = false;
            continue;
        }

        if let Some(f) = fence_open(line_no_nl) {
            flush_para(false, &mut out, &mut para_parts);
            in_fence = Some(f);
            out.push_str(raw);
            prev_nonblank_was_paragraph = false;
            continue;
        }

        // Handle UL/OL/DT/DD first
        if let Some((prefix, first_text)) = starts_with_bullet(line_no_nl) {
            flush_para(true, &mut out, &mut para_parts);
            let mut contents: Vec<String> = vec![first_text];
            let mut last_had_nl = had_nl;

            while let Some(peek) = lines_iter.peek() {
                let nxt_raw = *peek;
                let nxt_had_nl = nxt_raw.ends_with('\n');
                let nxt = if nxt_had_nl { &nxt_raw[..nxt_raw.len()-1] } else { nxt_raw };
                let nxt_stripped = nxt.trim();

                if nxt_stripped.is_empty() { break; }
                if fence_open(nxt).is_some()
                    || is_atx_heading(nxt)
                    || starts_with_bullet(nxt).is_some()
                    || starts_with_ol(nxt).is_some()
                    || parse_dt(nxt).is_some() || parse_dd(nxt).is_some()
                    || is_blockquote(nxt)
                    || is_hr_line_stripped(nxt_stripped)
                    || is_setext_underline_stripped(nxt_stripped)
                { break; }
                contents.push(nxt.trim_start_matches([' ', '\t']).to_string());
                last_had_nl = nxt_had_nl;
                lines_iter.next();
            }

            let mut joined = contents.remove(0).trim_end_matches([' ', '\t']).to_string();
            for c in contents {
                joined.push(' ');
                joined.push_str(c.trim_start_matches([' ', '\t']));
            }
            out.push_str(&prefix);
            out.push_str(&joined);
            if last_had_nl { out.push('\n'); }
            prev_nonblank_was_paragraph = false;
            continue;
        }

        if let Some((prefix, first_text)) = starts_with_ol(line_no_nl) {
            flush_para(true, &mut out, &mut para_parts);
            let mut contents: Vec<String> = vec![first_text];
            let mut last_had_nl = had_nl;

            while let Some(peek) = lines_iter.peek() {
                let nxt_raw = *peek;
                let nxt_had_nl = nxt_raw.ends_with('\n');
                let nxt = if nxt_had_nl { &nxt_raw[..nxt_raw.len()-1] } else { nxt_raw };
                let nxt_stripped = nxt.trim();

                if nxt_stripped.is_empty() { break; }
                if fence_open(nxt).is_some()
                    || is_atx_heading(nxt)
                    || starts_with_bullet(nxt).is_some()
                    || starts_with_ol(nxt).is_some()
                    || parse_dt(nxt).is_some() || parse_dd(nxt).is_some()
                    || is_blockquote(nxt)
                    || is_hr_line_stripped(nxt_stripped)
                    || is_setext_underline_stripped(nxt_stripped)
                { break; }
                contents.push(nxt.trim_start_matches([' ', '\t']).to_string());
                last_had_nl = nxt_had_nl;
                lines_iter.next();
            }

            let mut joined = contents.remove(0).trim_end_matches([' ', '\t']).to_string();
            for c in contents {
                joined.push(' ');
                joined.push_str(c.trim_start_matches([' ', '\t']));
            }
            out.push_str(&prefix);
            out.push_str(&joined);
            if last_had_nl { out.push('\n'); }
            prev_nonblank_was_paragraph = false;
            continue;
        }

        if let Some((prefix, first_text)) = parse_dt(line_no_nl) {
            // Definition term
            flush_para(true, &mut out, &mut para_parts);
            let mut contents: Vec<String> = vec![first_text];
            let mut last_had_nl = had_nl;

            while let Some(peek) = lines_iter.peek() {
                let nxt_raw = *peek;
                let nxt_had_nl = nxt_raw.ends_with('\n');
                let nxt = if nxt_had_nl { &nxt_raw[..nxt_raw.len()-1] } else { nxt_raw };
                let nxt_stripped = nxt.trim();

                if nxt_stripped.is_empty() { break; }
                if fence_open(nxt).is_some()
                    || is_atx_heading(nxt)
                    || starts_with_bullet(nxt).is_some()
                    || starts_with_ol(nxt).is_some()
                    || parse_dt(nxt).is_some() || parse_dd(nxt).is_some()
                    || is_blockquote(nxt)
                    || is_hr_line_stripped(nxt_stripped)
                    || is_setext_underline_stripped(nxt_stripped)
                { break; }
                contents.push(nxt.trim_start_matches([' ', '\t']).to_string());
                last_had_nl = nxt_had_nl;
                lines_iter.next();
            }

            let mut joined = contents.remove(0).trim_end_matches([' ', '\t']).to_string();
            for c in contents {
                joined.push(' ');
                joined.push_str(c.trim_start_matches([' ', '\t']));
            }
            out.push_str(&prefix);
            out.push_str(&joined);
            if last_had_nl { out.push('\n'); }
            prev_nonblank_was_paragraph = false;
            continue;
        }

        if let Some((prefix, first_text)) = parse_dd(line_no_nl) {
            // Definition description
            flush_para(true, &mut out, &mut para_parts);
            let mut contents: Vec<String> = vec![first_text];
            let mut last_had_nl = had_nl;

            while let Some(peek) = lines_iter.peek() {
                let nxt_raw = *peek;
                let nxt_had_nl = nxt_raw.ends_with('\n');
                let nxt = if nxt_had_nl { &nxt_raw[..nxt_raw.len()-1] } else { nxt_raw };
                let nxt_stripped = nxt.trim();

                if nxt_stripped.is_empty() { break; }
                if fence_open(nxt).is_some()
                    || is_atx_heading(nxt)
                    || starts_with_bullet(nxt).is_some()
                    || starts_with_ol(nxt).is_some()
                    || parse_dt(nxt).is_some() || parse_dd(nxt).is_some()
                    || is_blockquote(nxt)
                    || is_hr_line_stripped(nxt_stripped)
                    || is_setext_underline_stripped(nxt_stripped)
                { break; }
                contents.push(nxt.trim_start_matches([' ', '\t']).to_string());
                last_had_nl = nxt_had_nl;
                lines_iter.next();
            }

            let mut joined = contents.remove(0).trim_end_matches([' ', '\t']).to_string();
            for c in contents {
                joined.push(' ');
                joined.push_str(c.trim_start_matches([' ', '\t']));
            }
            out.push_str(&prefix);
            out.push_str(&joined);
            if last_had_nl { out.push('\n'); }
            prev_nonblank_was_paragraph = false;
            continue;
        }

        // Generic structural lines
        let is_structural_line =
            is_atx_heading(line_no_nl) ||
            is_blockquote(line_no_nl) ||
            is_hr_line_stripped(line_stripped_ws) ||
            (is_setext_underline_stripped(line_stripped_ws) && prev_nonblank_was_paragraph);

        if is_structural_line {
            flush_para(true, &mut out, &mut para_parts);
            out.push_str(raw);
            prev_nonblank_was_paragraph = false;
            continue;
        }

        // Regular paragraph line
        para_parts.push(line_no_nl.to_string());
        prev_nonblank_was_paragraph = true;
    }

    // flush at end
    if !para_parts.is_empty() {
        let mut buf = String::new();
        let first = para_parts[0].trim_end_matches([' ', '\t']);
        buf.push_str(first);
        for s in para_parts.iter().skip(1) {
            buf.push(' ');
            buf.push_str(s.trim_start_matches([' ', '\t']));
        }
        out.push_str(&buf);
    }

    out
}

// UTF-8 safe plain-text reflow: collapse newline-including runs to a single space.
fn reflow_plain_text(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    let mut seg_start = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'\n' {
            if seg_start < i {
                out.push_str(&text[seg_start..i]); // safe: char boundary
            }
            if !out.ends_with(' ') {
                out.push(' ');
            }
            i += 1;
            while i < bytes.len() && (bytes[i] == b'\n' || bytes[i] == b' ' || bytes[i] == b'\t') {
                i += 1;
            }
            seg_start = i;
        } else {
            i += 1;
        }
    }
    if seg_start < bytes.len() {
        out.push_str(&text[seg_start..]);
    }
    out
}

fn reflow_text(text: &str, use_markdown: bool) -> String {
    if use_markdown {
        reflow_markdown_text(text)
    } else {
        reflow_plain_text(text)
    }
}

/* ==================== Structural boundary helper ======================== */

fn prev_line_ends_with_structural_start(s: &[u8], mut boundary: usize) -> bool {
    loop {
        let line_start = memrchr(b'\n', &s[..boundary]).map(|x| x + 1).unwrap_or(0);
        if line_start >= boundary { return false; }
        // Trim trailing spaces/tabs
        let mut end = boundary;
        while end > line_start && is_space_tab(s[end - 1]) { end -= 1; }
        if end > line_start {
            // non-empty after trim
            if s[end - 1] != b'>' { return false; }
            let lt = memrchr(b'<', &s[line_start..end]).map(|x| x + line_start);
            let lt = match lt { Some(v) => v, None => return false };
            let tag = &s[lt..end];
            let ti = parse_tag_info(tag);
            if ti.is_end { return false; }
            return is_structural(ti.name);
        } else {
            // empty line, go back
            if line_start == 0 { return false; }
            boundary = line_start - 1; // before the \n
        }
    }
}

fn has_single_lf(chunk: &[u8]) -> bool {
    let mut count = 0usize;
    for &c in chunk {
        if c == b'\n' { count += 1; if count > 1 { return false; } }
    }
    count == 1
}

fn trailing_lf_count_ignoring_spaces(chunk: &[u8]) -> usize {
    let mut i = chunk.len();
    while i > 0 && (chunk[i - 1] == b' ' || chunk[i - 1] == b'\t') { i -= 1; }
    let mut k = 0usize;
    while i > 0 && chunk[i - 1] == b'\n' {
        k += 1;
        i -= 1;
    }
    k
}

/* ============================ Raw-text copying ========================== */

/// Copy bytes from `i` until the **matching** end tag `</name>` is found.
/// Returns (new_index_after_end_tag, closed_found).
fn copy_raw_text_until_end(src: &[u8], i: usize, name: &[u8], out: &mut Vec<u8>) -> (usize, bool) {
    let n = src.len();
    let lower_name = name.to_ascii_lowercase();
    let name_ref = lower_name.as_slice();

    let mut j = i;
    loop {
        if j >= n {
            return (n, false);
        }
        let Some(pos) = memchr(b'<', &src[j..]).map(|off| j + off) else {
            out.extend_from_slice(&src[j..]);
            return (n, false);
        };
        // emit text between j and pos verbatim
        out.extend_from_slice(&src[j..pos]);

        // Not enough room for "</"
        if pos + 2 >= n || src[pos + 1] != b'/' {
            // literal '<'
            out.push(b'<');
            j = pos + 1;
            continue;
        }

        // Try to parse an end tag
        if let Some(end) = find_tag_end(src, pos) {
            let ti = parse_tag_info(&src[pos..=end]);
            if ti.name.eq_ignore_ascii_case(name_ref) {
                normalize_inside_tag(&src[pos..=end], out);
                return (end + 1, true);
            } else {
                out.extend_from_slice(&src[pos..=end]);
                j = end + 1;
                continue;
            }
        } else {
            out.extend_from_slice(&src[pos..]);
            return (n, false);
        }
    }
}

/* ========================== Text chunk handling ========================= */

fn classify_ahead(src: &[u8], next_lt: usize) -> (bool, bool, Option<TagInfo<'_>>) {
    if next_lt >= src.len() { return (false, false, None); }
    if src[next_lt..].starts_with(b"<!--") {
        let (j_end, standalone) = scan_comment(src, next_lt);
        if j_end == usize::MAX { return (false, false, None); }
        return (standalone, !standalone, None);
    }
    if src[next_lt] == b'<' {
        if let Some(j) = find_tag_end(src, next_lt) {
            let ti = parse_tag_info(&src[next_lt..=j]);
            return (false, false, Some(ti));
        }
    }
    (false, false, None)
}

fn reflow_text_chunk(
    chunk: &[u8],
    src: &[u8],
    next_lt: usize,
    out: &mut Vec<u8>,
    use_markdown: bool,
    after_boundary: bool,
    after_br: bool,
    at_index_i: usize,
) {
    let (ahead_is_standalone_comment, ahead_is_inline_comment, ahead_tag) = classify_ahead(src, next_lt);

    let chunk_is_ws_only = chunk.iter().all(|&b| is_ws(b));
    if chunk_is_ws_only {
        if next_lt < src.len() {
            if ahead_is_standalone_comment {
                out.extend_from_slice(chunk);
            } else if ahead_is_inline_comment {
                if has_single_lf(chunk) {
                    if prev_line_ends_with_structural_start(src, next_lt) {
                        out.extend_from_slice(chunk);
                    } else {
                        out.push(b' ');
                    }
                } else {
                    out.extend_from_slice(chunk);
                }
            } else if let Some(ti) = ahead_tag {
                let structural_ahead = is_structural(ti.name);
                if structural_ahead {
                    out.extend_from_slice(chunk);
                } else if !ti.is_end && is_inline(ti.name) {
                    if has_single_lf(chunk) {
                        if prev_line_ends_with_structural_start(src, next_lt) {
                            out.extend_from_slice(chunk);
                        } else {
                            out.push(b' ');
                        }
                    } else {
                        out.extend_from_slice(chunk);
                    }
                } else {
                    out.extend_from_slice(chunk);
                }
            } else {
                out.extend_from_slice(chunk);
            }
        } else {
            out.extend_from_slice(chunk);
        }
        return;
    }

    // Non-whitespace chunk
    let mut preserve_trailing_suffix = false;
    if next_lt < src.len() {
        if ahead_is_standalone_comment {
            preserve_trailing_suffix = true;
        } else if let Some(ti) = ahead_tag {
            if is_structural(ti.name) {
                preserve_trailing_suffix = true;
            }
        }
    }

    // If the line that contains `next_lt` (often a DT/DD line) begins with : or ::, keep suffix.
    let boundary_end = at_index_i + chunk.len();
    if use_markdown && line_at_pos_starts_with_dt_or_dd(src, boundary_end) {
        preserve_trailing_suffix = true;
    }

    let preserve_leading_prefix = after_boundary || after_br;

    if preserve_leading_prefix || preserve_trailing_suffix {
        // prefix: leading whitespace
        let mut left = 0usize;
        if preserve_leading_prefix {
            while left < chunk.len() && is_ws(chunk[left]) { left += 1; }
            out.extend_from_slice(&chunk[..left]);
        }
        // suffix: ALL trailing whitespace (preserve exactly before structural/comment/DT/DD)
        let mut idx = chunk.len();
        while idx > left && is_ws(chunk[idx - 1]) {
            idx -= 1;
        }
        let suffix_start = idx;
        let body = &chunk[left..suffix_start];

        if !body.is_empty() {
            // SPECIAL: Keep DT/DD on their own line when body starts with "\n" + indent + ":"[":"]
            if use_markdown {
                if let Some(indent_end) = leading_lf_indent_end_before_dt_or_dd(body) {
                    // Emit "\n" + indentation
                    out.push(b'\n');
                    out.extend_from_slice(&body[1..indent_end]); // indentation
                    let rest = std::str::from_utf8(&body[indent_end..]).unwrap();
                    let reflowed = reflow_text(rest, use_markdown);
                    out.extend_from_slice(reflowed.as_bytes());
                } else if body.starts_with(b"\n") && (body.len() == 1 || body[1] != b'\n')
                    && !prev_line_ends_with_structural_start(src, at_index_i)
                    && !after_br && !after_boundary
                    && !(use_markdown && body_begins_with_dt_or_dd_after_single_lf(body))
                {
                    // Soft wrap single LF → space
                    let mut j = 1usize;
                    while j < body.len() && (body[j] == b' ' || body[j] == b'\t') { j += 1; }
                    let rest = std::str::from_utf8(&body[j..]).unwrap();
                    let mut body_str = String::with_capacity(1 + rest.len());
                    body_str.push(' ');
                    body_str.push_str(rest);
                    let reflowed = reflow_text(&body_str, use_markdown);
                    out.extend_from_slice(reflowed.as_bytes());
                } else {
                    let body_str = std::str::from_utf8(body).unwrap();
                    let reflowed = reflow_text(body_str, use_markdown);
                    out.extend_from_slice(reflowed.as_bytes());
                }
            } else {
                // Plain text mode
                if body.starts_with(b"\n") && (body.len() == 1 || body[1] != b'\n')
                    && !prev_line_ends_with_structural_start(src, at_index_i)
                    && !after_br && !after_boundary
                {
                    let mut j = 1usize;
                    while j < body.len() && (body[j] == b' ' || body[j] == b'\t') { j += 1; }
                    let rest = std::str::from_utf8(&body[j..]).unwrap();
                    let mut body_str = String::with_capacity(1 + rest.len());
                    body_str.push(' ');
                    body_str.push_str(rest);
                    let reflowed = reflow_text(&body_str, use_markdown);
                    out.extend_from_slice(reflowed.as_bytes());
                } else {
                    let body_str = std::str::from_utf8(body).unwrap();
                    let reflowed = reflow_text(body_str, use_markdown);
                    out.extend_from_slice(reflowed.as_bytes());
                }
            }
        }

        if preserve_trailing_suffix {
            out.extend_from_slice(&chunk[suffix_start..]); // preserve spaces/newlines before DT/DD/comment/structural
        } else if (ahead_tag.map_or(false, |ti| !ti.is_end && is_inline(ti.name)) || ahead_is_inline_comment) && suffix_start < chunk.len() {
            out.push(b' ');
        }
        return;
    }

    // Preserve non-newline edge spaces around tags:
    let mut lead_len = 0usize;
    while lead_len < chunk.len() && is_space_tab(chunk[lead_len]) { lead_len += 1; }
    let mut trail_len = 0usize;
    while trail_len < chunk.len() && is_space_tab(chunk[chunk.len() - 1 - trail_len]) {
        trail_len += 1;
    }
    let body = &chunk[lead_len..chunk.len() - trail_len];

    // SPECIAL: DT/DD must start on a new line — emit the newline + indentation, then reflow the rest.
    if use_markdown {
        if let Some(indent_end) = leading_lf_indent_end_before_dt_or_dd(body) {
            out.extend_from_slice(&chunk[..lead_len]); // leading spaces (no newlines here)
            out.push(b'\n');
            out.extend_from_slice(&body[1..indent_end]); // indentation
            let rest = std::str::from_utf8(&body[indent_end..]).unwrap();
            let reflowed = reflow_text(rest, use_markdown);
            out.extend_from_slice(reflowed.as_bytes());
            out.extend_from_slice(&chunk[chunk.len() - trail_len..]);
            return;
        }
    }

    // Soft-wrap at start-of-body — but NOT if that newline introduces a DT/DD line.
    let mut tmp = String::new();
    let body_str = if body.starts_with(b"\n") && (body.len() == 1 || body[1] != b'\n')
        && !prev_line_ends_with_structural_start(src, at_index_i)
        && !after_br && !after_boundary
        && !(use_markdown && body_begins_with_dt_or_dd_after_single_lf(body))
    {
        let mut j = 1usize;
        while j < body.len() && (body[j] == b' ' || body[j] == b'\t') { j += 1; }
        let rest = std::str::from_utf8(&body[j..]).unwrap();
        tmp.push(' ');
        tmp.push_str(rest);
        &tmp
    } else {
        std::str::from_utf8(body).unwrap()
    };

    let mut reflowed = reflow_text(body_str, use_markdown);

    // If this chunk ends with exactly one LF (ignoring spaces) and next token is inline-start,
    // collapse that single LF (+ indent) to a single space (unless prev line ended with structural start).
    let trailing_lfs = trailing_lf_count_ignoring_spaces(chunk);
    if let Some(ti) = ahead_tag {
        if !ti.is_end && is_inline(ti.name) && trailing_lfs == 1
            && !prev_line_ends_with_structural_start(src, at_index_i + chunk.len())
        {
            while reflowed.ends_with(' ') || reflowed.ends_with('\t') { reflowed.pop(); }
            if reflowed.ends_with('\n') {
                reflowed.pop();
                while reflowed.ends_with(' ') || reflowed.ends_with('\t') { reflowed.pop(); }
            }
            out.extend_from_slice(&chunk[..lead_len]); // leading spaces
            out.extend_from_slice(reflowed.as_bytes());
            out.push(b' ');
            return;
        }
    } else if ahead_is_inline_comment {
        if trailing_lfs == 1 && !prev_line_ends_with_structural_start(src, at_index_i + chunk.len()) {
            while reflowed.ends_with(' ') || reflowed.ends_with('\t') { reflowed.pop(); }
            if reflowed.ends_with('\n') {
                reflowed.pop();
                while reflowed.ends_with(' ') || reflowed.ends_with('\t') { reflowed.pop(); }
            }
            out.extend_from_slice(&chunk[..lead_len]);
            out.extend_from_slice(reflowed.as_bytes());
            out.push(b' ');
            return;
        }
    } else if ahead_tag.is_none() && !ahead_is_standalone_comment {
        if trailing_lfs == 1 && !prev_line_ends_with_structural_start(src, at_index_i + chunk.len()) {
            while reflowed.ends_with(' ') || reflowed.ends_with('\t') { reflowed.pop(); }
            if reflowed.ends_with('\n') {
                reflowed.pop();
                while reflowed.ends_with(' ') || reflowed.ends_with('\t') { reflowed.pop(); }
            }
            out.extend_from_slice(&chunk[..lead_len]);
            out.extend_from_slice(reflowed.as_bytes());
            return;
        }
    }

    out.extend_from_slice(&chunk[..lead_len]);
    out.extend_from_slice(reflowed.as_bytes());
    out.extend_from_slice(&chunk[chunk.len() - trail_len..]);
}

/* ============================== Transform =============================== */

#[derive(Clone)]
struct OpenElement {
    name: Vec<u8>,
    has_noreformat: bool,
}

fn transform(src: &[u8], out: &mut Vec<u8>, use_markdown: bool) {
    let mut i = 0usize;
    let n = src.len();

    // Stacks/state
    let mut raw_stack: Vec<Vec<u8>> = Vec::new();        // names of raw-text tags in lowercase
    let mut open_stack: Vec<OpenElement> = Vec::new();
    let mut after_boundary = false;
    let mut after_br = false;

    let p_closing: &[&[u8]] = &[
        b"address", b"article", b"aside", b"blockquote", b"center", b"details", b"dialog", b"dir",
        b"div", b"dl", b"fieldset", b"figcaption", b"figure", b"footer", b"header", b"hgroup",
        b"main", b"menu", b"nav", b"ol", b"p", b"search", b"section", b"summary", b"ul",
    ];

    while i < n {
        // If inside a RAW-TEXT element, copy verbatim until its matching end tag.
        if let Some(current_raw) = raw_stack.last() {
            let (new_i, closed) = copy_raw_text_until_end(src, i, current_raw, out);
            i = new_i;
            after_boundary = false;
            after_br = false;
            if closed {
                raw_stack.pop();
                open_stack.pop();
            }
            continue;
        }

        // Comments
        if src[i..].starts_with(b"<!--") {
            let (j_end, standalone) = scan_comment(src, i);
            if j_end == usize::MAX {
                out.extend_from_slice(&src[i..]);
                return;
            }
            let seg = &src[i..=j_end + 2]; // includes "-->"
            let is_verbatim = open_stack.iter().any(|e| e.has_noreformat);
            if is_verbatim {
                out.extend_from_slice(seg);
            } else if standalone {
                out.extend_from_slice(seg);
                after_boundary = true;
            } else {
                reflow_inline_comment(seg, out);
                after_boundary = false;
            }
            i = j_end + 3;
            continue;
        }

        // Tags
        if src[i] == b'<' {
            let Some(j) = find_tag_end(src, i) else {
                out.extend_from_slice(&src[i..]);
                return;
            };
            let tag = &src[i..=j];
            let ti = parse_tag_info(tag);

            let has_this_noreformat = tag_has_noreformat_attr(tag);
            let is_verbatim = open_stack.iter().any(|e| e.has_noreformat) || (!ti.is_end && has_this_noreformat);
            if is_verbatim {
                out.extend_from_slice(tag);
            } else {
                normalize_inside_tag(tag, out);
            }

            // open_stack handling
            let mut name_lower = ti.name.to_vec();
            name_lower.make_ascii_lowercase();
            if ti.is_end {
                while let Some(top) = open_stack.last() {
                    if top.name == name_lower {
                        open_stack.pop();
                        break;
                    } else {
                        open_stack.pop();
                    }
                }
            } else if !ti.self_closing && !is_void(ti.name) {
                // implied closes
                if name_lower == b"li" {
                    if let Some(top) = open_stack.last() {
                        if top.name == b"li" {
                            open_stack.pop();
                        }
                    }
                } else if name_lower == b"dt" || name_lower == b"dd" {
                    if let Some(top) = open_stack.last() {
                        if top.name == b"dt" || top.name == b"dd" {
                            open_stack.pop();
                        }
                    }
                } else if matches_ignore_ascii_case(&name_lower, p_closing) {
                    if let Some(top) = open_stack.last() {
                        if top.name == b"p" {
                            open_stack.pop();
                        }
                    }
                }

                open_stack.push(OpenElement {
                    name: name_lower.clone(),
                    has_noreformat: has_this_noreformat,
                });
            }

            // raw-text tracking
            if is_raw_text(ti.name) && !ti.is_end && !ti.self_closing {
                raw_stack.push(name_lower.clone());
            }

            // <br> rule
            if !ti.is_end && ti.name.eq_ignore_ascii_case(b"br") {
                if j + 1 < n && src[j + 1] == b'\n' {
                    out.push(b'\n');
                    i = j + 2;
                    after_br = true;
                    continue;
                } else {
                    after_br = true;
                }
            }

            // Set after_boundary for structural start tags
            if !ti.is_end && is_structural(&name_lower) {
                after_boundary = true;
            } else {
                after_boundary = false;
            }

            i = j + 1;
            continue;
        }

        // Text run
        let next_lt = memchr(b'<', &src[i..]).map(|off| i + off).unwrap_or(n);
        let chunk = &src[i..next_lt];

        let is_verbatim = open_stack.iter().any(|e| e.has_noreformat);
        if is_verbatim {
            out.extend_from_slice(chunk);
        } else {
            reflow_text_chunk(
                chunk,
                src,
                next_lt,
                out,
                use_markdown,
                after_boundary,
                after_br,
                i,
            );
        }

        after_boundary = false;
        after_br = false;
        i = next_lt;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, DirEntry};
    use std::path::Path;

    #[test]
    fn regression_tests() {
        let inputs_dir = Path::new("tests/fixtures/inputs");
        let expected_dir = Path::new("tests/fixtures/expected");
        let update_expected = std::env::var("UPDATE_EXPECTED").is_ok();

        if !inputs_dir.exists() {
            return; // No fixtures yet, skip
        }

        let entries: Vec<DirEntry> = fs::read_dir(inputs_dir).unwrap().map(|e| e.unwrap()).collect();

        for entry in entries {
            let input_path = entry.path();
            let ext = input_path.extension().unwrap_or_default().to_str().unwrap_or("");
            if ext != "bs" && ext != "html" {
                continue;
            }

            let stem = input_path.file_stem().unwrap().to_str().unwrap();
            let expected_path = expected_dir.join(format!("{}.{}", stem, ext));

            let src = fs::read(&input_path).unwrap();
            let mut out = Vec::new();

            // Enable markdown for .bs, disable for .html
            let use_markdown = ext == "bs";

            transform(&src, &mut out, use_markdown);

            let actual = String::from_utf8(out).unwrap();

            if update_expected {
                fs::create_dir_all(expected_dir).unwrap();
                fs::write(&expected_path, actual.as_bytes()).unwrap();
            } else {
                let expected = fs::read_to_string(&expected_path).unwrap_or_else(|_| panic!("Expected file not found: {:?}", expected_path));
                assert_eq!(actual, expected, "Mismatch for test: {}", stem);
            }
        }
    }
}
