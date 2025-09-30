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
//   hr, ATX/Setext headings, fenced code blocks). List items reflow wrapped lines.
// - INLINE start tags at start-of-line soft-join into previous text unless exceptions apply.
// - <br> preserves an immediately following '\n'.
// - 'foreignobject' included in STRUCTURAL_START.
// - UTF-8 safe: input is UTF-8; text reflow routines operate on &str slices and never
//   push raw bytes as chars.
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
            b"sub", b"sup", b"time", b"u", b"var",
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

fn is_structural_start(name: &[u8]) -> bool {
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

fn is_structural_end(name: &[u8]) -> bool {
    matches_ignore_ascii_case(
        name,
        &[
            b"dl", b"ol", b"ul", b"table", b"thead", b"tbody", b"tfoot", b"tr", b"td", b"th",
            b"caption", b"colgroup", b"ruby", b"optgroup", b"select", b"p",
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
    // value may be quoted or unquoted; we only care if any attribute name equals "data-noreformat".
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

    let digits = &line[i..dot_pos]; // digits only
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

fn is_dt(line: &str) -> bool {
    // ^\s*:\s+
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') { i += 1; }
    if i < bytes.len() && bytes[i] == b':' {
        let j = i + 1;
        if j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
            return true;
        }
    }
    false
}

fn is_dd(line: &str) -> bool {
    // ^\s*::\s+
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') { i += 1; }
    if i + 1 < bytes.len() && bytes[i] == b':' && bytes[i + 1] == b':' {
        let j = i + 2;
        if j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
            return true;
        }
    }
    false
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

        let is_structural_line =
            is_atx_heading(line_no_nl) ||
            is_dt(line_no_nl) ||
            is_dd(line_no_nl) ||
            is_blockquote(line_no_nl) ||
            is_hr_line_stripped(line_stripped_ws) ||
            (is_setext_underline_stripped(line_stripped_ws) && prev_nonblank_was_paragraph);

        if is_structural_line {
            flush_para(true, &mut out, &mut para_parts);
            out.push_str(raw);
            prev_nonblank_was_paragraph = false;
            continue;
        }

        // bullets
        if let Some((prefix, first_text)) = starts_with_bullet(line_no_nl) {
            flush_para(true, &mut out, &mut para_parts);
            let mut contents: Vec<String> = vec![first_text];

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
                    || is_dt(nxt) || is_dd(nxt) || is_blockquote(nxt)
                    || is_hr_line_stripped(nxt_stripped)
                    || is_setext_underline_stripped(nxt_stripped)
                {
                    break;
                }
                contents.push(nxt.trim_start_matches([' ', '\t']).to_string());
                lines_iter.next();
            }

            let mut joined = contents.remove(0).trim_end_matches([' ', '\t']).to_string();
            for c in contents {
                joined.push(' ');
                joined.push_str(c.trim_start_matches([' ', '\t']));
            }
            out.push_str(&prefix);
            out.push_str(&joined);
            if had_nl { out.push('\n'); }
            prev_nonblank_was_paragraph = false;
            continue;
        }

        // ordered list
        if let Some((prefix, first_text)) = starts_with_ol(line_no_nl) {
            flush_para(true, &mut out, &mut para_parts);
            let mut contents: Vec<String> = vec![first_text];

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
                    || is_dt(nxt) || is_dd(nxt) || is_blockquote(nxt)
                    || is_hr_line_stripped(nxt_stripped)
                    || is_setext_underline_stripped(nxt_stripped)
                {
                    break;
                }
                contents.push(nxt.trim_start_matches([' ', '\t']).to_string());
                lines_iter.next();
            }

            let mut joined = contents.remove(0).trim_end_matches([' ', '\t']).to_string();
            for c in contents {
                joined.push(' ');
                joined.push_str(c.trim_start_matches([' ', '\t']));
            }
            out.push_str(&prefix);
            out.push_str(&joined);
            if had_nl { out.push('\n'); }
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
                out.push_str(&text[seg_start..i]); // safe: ASCII boundary
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

fn prev_line_ends_with_structural_start(s: &[u8], boundary: usize) -> bool {
    let line_start = memrchr(b'\n', &s[..boundary]).map(|x| x + 1).unwrap_or(0);
    if line_start >= boundary { return false; }
    // Trim trailing spaces/tabs
    let mut end = boundary;
    while end > line_start && is_space_tab(s[end - 1]) { end -= 1; }
    if end <= line_start { return false; }
    if s[end - 1] != b'>' { return false; }
    let lt = memrchr(b'<', &s[line_start..end]).map(|x| x + line_start);
    let lt = match lt { Some(v) => v, None => return false };
    let tag = &s[lt..end];
    let ti = parse_tag_info(tag);
    if ti.is_end { return false; }
    is_structural_start(ti.name)
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
    let lower_name = name.to_ascii_lowercase(); // small
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
        // emit text between j and pos
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
                // Found the matching end tag
                normalize_inside_tag(&src[pos..=end], out);
                return (end + 1, true);
            } else {
                // Some other end tag; treat literally
                out.extend_from_slice(&src[pos..=end]);
                j = end + 1;
                continue;
            }
        } else {
            // Unterminated tag to EOF; treat literally
            out.extend_from_slice(&src[pos..]);
            return (n, false);
        }
    }
}

/* ========================== Text chunk handling ========================= */

fn classify_ahead(src: &[u8], next_lt: usize) -> (bool /*standalone comment*/, Option<TagInfo<'_>>) {
    if next_lt >= src.len() { return (false, None); }
    if src[next_lt..].starts_with(b"<!--") {
        let (j_end, standalone) = scan_comment(src, next_lt);
        if j_end == usize::MAX { return (false, None); }
        return (standalone, None);
    }
    if src[next_lt] == b'<' {
        if let Some(j) = find_tag_end(src, next_lt) {
            let ti = parse_tag_info(&src[next_lt..=j]);
            return (false, Some(ti));
        }
    }
    (false, None)
}

fn reflow_text_chunk(
    chunk: &[u8],
    src: &[u8],
    next_lt: usize,
    out: &mut Vec<u8>,
    use_markdown: bool,
    after_standalone_comment: bool,
    after_br: bool,
    at_index_i: usize,
) {
    let (ahead_is_standalone_comment, ahead_tag) = classify_ahead(src, next_lt);

    let chunk_is_ws_only = chunk.iter().all(|&b| is_ws(b));
    if chunk_is_ws_only {
        if next_lt < src.len() {
            if ahead_is_standalone_comment {
                out.extend_from_slice(chunk);
            } else if let Some(ti) = ahead_tag {
                let structural_ahead = (!ti.is_end && is_structural_start(ti.name))
                    || (ti.is_end && is_structural_end(ti.name));
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
            if (!ti.is_end && is_structural_start(ti.name))
                || (ti.is_end && is_structural_end(ti.name))
            {
                preserve_trailing_suffix = true;
            }
        }
    }
    let preserve_leading_prefix = after_standalone_comment || after_br;

    if preserve_leading_prefix || preserve_trailing_suffix {
        // prefix: leading whitespace
        let mut left = 0usize;
        if preserve_leading_prefix {
            while left < chunk.len() && is_ws(chunk[left]) { left += 1; }
            out.extend_from_slice(&chunk[..left]);
        }
        // suffix: ALL trailing whitespace (preserve exactly before structural/comment)
        let mut idx = chunk.len();
        while idx > left && is_ws(chunk[idx - 1]) {
            idx -= 1;
        }
        let suffix_start = idx;
        let body = &chunk[left..suffix_start];

        if !body.is_empty() {
            // Soft wrap at start-of-body
            if body.starts_with(b"\n") && (body.len() == 1 || body[1] != b'\n')
                && !prev_line_ends_with_structural_start(src, at_index_i)
                && !after_br && !after_standalone_comment
            {
                // replace that single leading LF + indentation with a space
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

        if preserve_trailing_suffix {
            out.extend_from_slice(&chunk[suffix_start..]); // <-- preserve spaces before structural/comment
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

    // Soft-wrap at start-of-body
    let mut tmp = String::new();
    let body_str = if body.starts_with(b"\n") && (body.len() == 1 || body[1] != b'\n')
        && !prev_line_ends_with_structural_start(src, at_index_i)
        && !after_br && !after_standalone_comment
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
    }

    out.extend_from_slice(&chunk[..lead_len]);
    out.extend_from_slice(reflowed.as_bytes());
    out.extend_from_slice(&chunk[chunk.len() - trail_len..]);
}

/* ============================== Transform =============================== */

fn transform(src: &[u8], out: &mut Vec<u8>, use_markdown: bool) {
    let mut i = 0usize;
    let n = src.len();

    // Stacks/state
    let mut raw_stack: Vec<Vec<u8>> = Vec::new();        // names of raw-text tags in lowercase
    let mut noreformat_stack: Vec<Vec<u8>> = Vec::new(); // element names with data-noreformat
    let mut after_standalone_comment = false;
    let mut after_br = false;

    while i < n {
        // If inside a RAW-TEXT element, copy verbatim until its matching end tag.
        if let Some(current_raw) = raw_stack.last().cloned() {
            let (new_i, closed) = copy_raw_text_until_end(src, i, &current_raw, out);
            i = new_i;
            after_standalone_comment = false;
            after_br = false;
            if closed {
                raw_stack.pop();
            } else {
                return;
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
            if !noreformat_stack.is_empty() {
                out.extend_from_slice(seg);
            } else if standalone {
                out.extend_from_slice(seg);
                after_standalone_comment = true;
            } else {
                reflow_inline_comment(seg, out);
                after_standalone_comment = false;
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

            if !noreformat_stack.is_empty() {
                out.extend_from_slice(tag);
            } else {
                normalize_inside_tag(tag, out);
            }

            // raw-text tracking
            if is_raw_text(ti.name) {
                if ti.is_end {
                    // ignore unmatched closes in non-raw context
                } else if !ti.self_closing {
                    raw_stack.push(ti.name.to_ascii_lowercase());
                }
            }

            // data-noreformat tracking
            if ti.is_end {
                if let Some(last) = noreformat_stack.last() {
                    if last.eq_ignore_ascii_case(ti.name) {
                        noreformat_stack.pop();
                    }
                }
            } else if !ti.self_closing && !is_void(ti.name) && tag_has_noreformat_attr(tag) {
                noreformat_stack.push(ti.name.to_ascii_lowercase());
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

            after_standalone_comment = false;
            i = j + 1;
            continue;
        }

        // Text run
        let next_lt = memchr(b'<', &src[i..]).map(|off| i + off).unwrap_or(n);
        let chunk = &src[i..next_lt];

        if !noreformat_stack.is_empty() {
            out.extend_from_slice(chunk);
            i = next_lt;
            continue;
        }

        reflow_text_chunk(
            chunk,
            src,
            next_lt,
            out,
            use_markdown,
            after_standalone_comment,
            after_br,
            i,
        );

        after_standalone_comment = false;
        after_br = false;
        i = next_lt;
    }
}
