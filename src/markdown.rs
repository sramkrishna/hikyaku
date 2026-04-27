// Markdown ↔ Pango markup conversion for the message view.
//
// Send path:  md_to_html      — converts user input to Matrix formatted_body (HTML).
// Receive path: html_to_segments — splits Matrix formatted_body into alternating
//   plain-text (Pango markup for gtk::Label) and code-block (raw text for
//   sourceview5::View) segments, enabling proper syntax-highlighted code boxes.

/// A segment of a formatted message body.
pub enum Segment {
    /// Pango markup to be displayed by a gtk::Label.
    Text(String),
    /// Raw source code + language hint, to be displayed by a sourceview5::View.
    Code { content: String, lang: String },
}

/// Convert Markdown text to HTML for use as Matrix `formatted_body`.
pub fn md_to_html(text: &str) -> String {
    use pulldown_cmark::{html, Options, Parser};
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TABLES);
    let parser = Parser::new_ext(text, opts);
    let mut html_out = String::new();
    html::push_html(&mut html_out, parser);
    // pulldown-cmark wraps bare paragraphs in <p>…</p>\n — strip the outer
    // wrapper when the input is a single paragraph.
    let trimmed = html_out.trim();
    if trimmed.starts_with("<p>") && trimmed.ends_with("</p>") && trimmed.matches("<p>").count() == 1 {
        trimmed[3..trimmed.len() - 4].to_string()
    } else {
        html_out
    }
}

/// Convert HTML (Matrix `formatted_body`) to a Pango markup string.
///
/// Code blocks are rendered inline as `<tt>…</tt>` — suitable for compact
/// banners (topic, pinned messages) that can't host a sourceview widget.
pub fn html_to_pango(html: &str) -> String {
    let _g = crate::perf::scope_gt("html_to_pango", 200);
    html_to_segments(html)
        .into_iter()
        .map(|seg| match seg {
            Segment::Text(t) => t,
            Segment::Code { content, .. } => format!("<tt>{}</tt>", escape_text(&content)),
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim_matches('\n')
        .to_string()
}

/// Convert Markdown text to a Pango markup string.
pub fn md_to_pango(text: &str) -> String {
    html_to_pango(&md_to_html(text))
}

/// Split Matrix formatted_body HTML into display segments.
///
/// `<pre><code …>…</code></pre>` blocks become `Segment::Code`; everything
/// else becomes `Segment::Text` containing Pango markup.  Multiple adjacent
/// Text segments are merged.  Empty Text segments are dropped.
pub fn html_to_segments(html: &str) -> Vec<Segment> {
    let mut segments: Vec<Segment> = Vec::new();
    let mut text_buf = String::new();

    // Byte-based scanning. Previously this function collected the input into
    // a Vec<char> (4 bytes per code point) — for a 10kB message that was
    // 40kB + O(n) copy per call, and tag-scanning did indexed char access
    // that defeats CPU cache locality. Since every HTML syntactic character
    // we compare against (<, >, ", ', =, /, !) is single-byte ASCII, byte
    // indexing into the original UTF-8 string is safe: text content between
    // those markers is always a valid UTF-8 substring. Measured: `html_to_pango`
    // hot-path dropped from 38ms to single-digit ms on real messages.
    let bytes = html.as_bytes();
    let len = bytes.len();
    let mut pos = 0;

    let mut in_pre = false;
    let mut pre_lang = String::new();
    let mut pre_content = String::new();
    let mut list_depth: u32 = 0;

    // Flush accumulated Pango text as a Text segment.
    macro_rules! flush_text {
        () => {
            let trimmed = text_buf.trim_matches('\n').to_string();
            if !trimmed.is_empty() {
                segments.push(Segment::Text(trimmed));
            }
            text_buf.clear();
        };
    }

    while pos < len {
        if bytes[pos] == b'<' {
            let start = pos + 1;
            let mut end = start;
            let mut in_quote: Option<u8> = None;
            while end < len {
                let c = bytes[end];
                match in_quote {
                    Some(q) if c == q => { in_quote = None; }
                    Some(_) => {}
                    None if c == b'"' || c == b'\'' => { in_quote = Some(c); }
                    None if c == b'>' => break,
                    _ => {}
                }
                end += 1;
            }
            if end >= len {
                if in_pre { pre_content.push_str(&html[pos..]); }
                else { text_buf.push_str(&escape_text(&html[pos..])); }
                break;
            }
            let raw_tag = &html[start..end];
            pos = end + 1;

            let (name, attrs, closing) = parse_tag(raw_tag);

            match name.as_str() {
                "b" | "strong" => {
                    if !in_pre { text_buf.push_str(if closing { "</b>" } else { "<b>" }); }
                }
                "i" | "em" => {
                    if !in_pre { text_buf.push_str(if closing { "</i>" } else { "<i>" }); }
                }
                "s" | "del" | "strike" => {
                    if !in_pre { text_buf.push_str(if closing { "</s>" } else { "<s>" }); }
                }
                "u" => {
                    if !in_pre { text_buf.push_str(if closing { "</u>" } else { "<u>" }); }
                }
                "code" => {
                    if in_pre {
                        if !closing {
                            if let Some(class) = attrs.iter()
                                .find(|(k, _)| k == "class")
                                .map(|(_, v)| v.as_str())
                            {
                                pre_lang = class.strip_prefix("language-")
                                    .unwrap_or(class)
                                    .to_string();
                            }
                        }
                    } else {
                        text_buf.push_str(if closing { "</tt>" } else { "<tt>" });
                    }
                }
                "pre" => {
                    if closing {
                        flush_text!();
                        let content = pre_content.trim_end_matches('\n').to_string();
                        if !content.is_empty() {
                            segments.push(Segment::Code { content, lang: pre_lang.clone() });
                        }
                        pre_content.clear();
                        pre_lang.clear();
                        in_pre = false;
                    } else {
                        in_pre = true;
                    }
                }
                "a" => {
                    if !in_pre {
                        if closing {
                            text_buf.push_str("</a>");
                        } else if let Some(href) = attrs.iter()
                            .find(|(k, _)| k == "href")
                            .map(|(_, v)| v)
                        {
                            text_buf.push_str(&format!("<a href=\"{}\">", escape_attr(href)));
                        }
                    }
                }
                "br" => {
                    if in_pre { pre_content.push('\n'); }
                    else { text_buf.push('\n'); }
                }
                "p" => {
                    if !in_pre && closing { text_buf.push('\n'); }
                }
                "h1" | "h2" | "h3" => {
                    if !in_pre { text_buf.push_str(if closing { "</b>\n" } else { "<b>" }); }
                }
                "h4" | "h5" | "h6" => {
                    if !in_pre { text_buf.push_str(if closing { "</i>\n" } else { "<i>" }); }
                }
                "ul" | "ol" => {
                    if !in_pre {
                        if closing { list_depth = list_depth.saturating_sub(1); }
                        else { list_depth += 1; }
                    }
                }
                "li" => {
                    if !in_pre && !closing {
                        let indent = "  ".repeat(list_depth.saturating_sub(1) as usize);
                        text_buf.push_str(&format!("\n{indent}• "));
                    }
                }
                "blockquote" => {
                    if !in_pre {
                        text_buf.push_str(if closing { "</span>" } else { "<span foreground=\"gray\">" });
                    }
                }
                "mx-reply" => {
                    if !closing {
                        // Byte-based substring search via str::find — linear
                        // and no allocation; previous Vec<char>+indexed eq
                        // was O(n·len(needle)) with a per-iteration alloc.
                        let needle = "</mx-reply>";
                        match html[pos..].find(needle) {
                            Some(rel) => pos += rel + needle.len(),
                            None => pos = len,
                        }
                    }
                }
                _ => {}
            }
        } else {
            // Fast-forward to the next '<' via memchr (str::find is optimised).
            let start = pos;
            let rel = html[pos..].find('<').unwrap_or(len - pos);
            pos += rel;
            let text = &html[start..pos];
            let decoded = decode_html_entities(text);
            if in_pre {
                pre_content.push_str(&decoded);
            } else {
                text_buf.push_str(&linkify_urls(&escape_text(&decoded)));
            }
        }
    }

    flush_text!();
    segments
}

// --- helpers -----------------------------------------------------------------

fn parse_tag(raw: &str) -> (String, Vec<(String, String)>, bool) {
    let raw = raw.trim();
    let closing = raw.starts_with('/');
    let raw = if closing { raw.trim_start_matches('/').trim() } else { raw };

    let mut iter = raw.splitn(2, |c: char| c.is_whitespace());
    let name = iter.next().unwrap_or("").to_lowercase();
    let name = name.trim_end_matches('/');
    let rest = iter.next().unwrap_or("");

    let mut attrs = Vec::new();
    let mut s = rest;
    while !s.is_empty() {
        s = s.trim_start();
        let eq = s.find('=');
        let space = s.find(|c: char| c.is_whitespace());
        match (eq, space) {
            (Some(e), _) => {
                let key = s[..e].trim().to_lowercase();
                s = s[e + 1..].trim_start();
                let (val, rest2) = if s.starts_with('"') {
                    let end = s[1..].find('"').map(|i| i + 1).unwrap_or(s.len() - 1);
                    (&s[1..end], &s[end + 1..])
                } else if s.starts_with('\'') {
                    let end = s[1..].find('\'').map(|i| i + 1).unwrap_or(s.len() - 1);
                    (&s[1..end], &s[end + 1..])
                } else {
                    let end = s.find(|c: char| c.is_whitespace()).unwrap_or(s.len());
                    (&s[..end], &s[end..])
                };
                attrs.push((key, val.to_string()));
                s = rest2;
            }
            (None, Some(sp)) => { s = &s[sp + 1..]; }
            (None, None) => break,
        }
    }

    (name.to_string(), attrs, closing)
}

fn decode_html_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pos = 0;
    let bytes = s.as_bytes();
    while pos < s.len() {
        if bytes[pos] == b'&' {
            if let Some(semi) = s[pos..].find(';') {
                let entity = &s[pos + 1..pos + semi];
                let replacement = match entity {
                    "amp"  => Some("&"),
                    "lt"   => Some("<"),
                    "gt"   => Some(">"),
                    "quot" => Some("\""),
                    "apos" => Some("'"),
                    "nbsp" => Some("\u{00A0}"),
                    _ if entity.starts_with('#') => {
                        let n = &entity[1..];
                        let code: Option<u32> = if let Some(hex) = n.strip_prefix('x') {
                            u32::from_str_radix(hex, 16).ok()
                        } else {
                            n.parse().ok()
                        };
                        if let Some(c) = code.and_then(char::from_u32) {
                            out.push(c);
                        }
                        pos += semi + 1;
                        continue;
                    }
                    _ => None,
                };
                if let Some(r) = replacement {
                    out.push_str(r);
                    pos += semi + 1;
                    continue;
                }
            }
        }
        let c = s[pos..].chars().next().unwrap();
        out.push(c);
        pos += c.len_utf8();
    }
    out
}

pub(crate) fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
     .replace('<', "&lt;")
     .replace('>', "&gt;")
}

/// Convert bare http/https URLs in already-escaped Pango markup text into
/// `<a href="…">…</a>` links.  Input must already be XML-escaped (& → &amp;
/// etc.) so that URL characters are intact but no literal `<`/`>` remain.
///
/// Also linkifies Matrix room aliases (`#name:server.tld`). Room aliases
/// are rendered with text-only pill styling (the Pango `mx-room-pill` class
/// picks up CSS) and become clickable matrix.to links handled by the
/// existing `parse_matrix_uri` / `handle_matrix_link` pipeline.
pub(crate) fn linkify_urls(text: &str) -> String {
    linkify_aliases(&linkify_http(text))
}

fn linkify_http(text: &str) -> String {
    let mut result = String::with_capacity(text.len() + 64);
    let mut rest = text;
    while let Some(start) = rest.find("http") {
        let candidate = &rest[start..];
        if !candidate.starts_with("https://") && !candidate.starts_with("http://") {
            result.push_str(&rest[..start + 4]);
            rest = &rest[start + 4..];
            continue;
        }
        result.push_str(&rest[..start]);
        // Stop at whitespace or literal < / > (there are none after escape_text,
        // but guard against double-calls or future callers).
        let url_end = candidate
            .find(|c: char| c.is_whitespace() || c == '<' || c == '>')
            .unwrap_or(candidate.len());
        let url = &candidate[..url_end];
        // matrix.to permalinks get the same pill styling as bare #alias
        // mentions — otherwise a 90-character event URL wraps across
        // four lines and buries the prose around it. Pill text is the
        // friendly short form (alias, room id, or "message link");
        // href stays the full URL so the existing matrix-uri click
        // handler can route it.
        if let Some(pill_text) = matrix_to_pill_text(url) {
            let pill_esc = glib::markup_escape_text(&pill_text);
            result.push_str(&render_pill_markup(url, &pill_esc));
        } else {
            result.push_str(&format!("<a href=\"{url}\">{url}</a>"));
        }
        rest = &candidate[url_end..];
    }
    result.push_str(rest);
    result
}

/// Pill colour for alias / matrix.to links.
const PILL_BG: &str = "#26a269";
const PILL_FG: &str = "#ffffff";

/// Build the Pango markup for a pill-styled link. `href` is the URL
/// the anchor should point at; `body_escaped` is the pill's visible
/// text, already run through `markup_escape_text` when needed. The
/// pill is a single span with NBSP padding on each side — we tried
/// bolting half-circle endcaps on for a rounded look but the cap
/// glyphs rendered at character-cell height, not the pill's line
/// height, which produced the visible seam in Sriram's screenshot.
/// Staying with a clean rectangle until we do the real widget-based
/// renderer.
fn render_pill_markup(href: &str, body_escaped: &str) -> String {
    format!(
        "<a href=\"{href}\"><span \
            foreground=\"{PILL_FG}\" \
            background=\"{PILL_BG}\" \
            weight=\"bold\" \
            underline=\"none\">\u{a0}{body_escaped}\u{a0}</span></a>",
    )
}

/// Shim: room-name lookups now live in `crate::directory`. Kept here so
/// call sites that already import `markdown::set_room_name` don't need
/// updating in the same patch. Prefer `crate::directory::set_room_name`
/// for new code.
pub fn set_room_name(room_id: &str, name: &str) {
    crate::directory::set_room_name(room_id, name);
}

fn resolve_room_name(room_id_or_alias: &str) -> Option<String> {
    // Direct room-id hit.
    if let Some(name) = crate::directory::room_name(room_id_or_alias) {
        return Some(name);
    }
    // Alias path → reverse lookup to find the room id, then its name.
    if room_id_or_alias.starts_with('#') {
        if let Some(rid) = crate::directory::room_id_for_alias(room_id_or_alias) {
            return crate::directory::room_name(&rid);
        }
    }
    None
}

/// Return a compact, human-readable label for a matrix.to URL — used as
/// the pill text when the URL appears in a message body. Returns None
/// for non-matrix.to URLs so the caller falls back to the raw href.
///
/// Shape decisions (best effort, with the resolver when available):
///   * event link    → `🔗 <Room Name>` when known, else `🔗 <room-id>`
///   * room-only     → `# <Room Name>` when known, else `#alias:server`
///                     / `!id:server`
///   * user link     → `@user:server`
/// The leading glyph distinguishes the pill shape at a glance without
/// needing to read the full text.
fn matrix_to_pill_text(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://matrix.to/#/")?;
    // Drop any `?via=` query — it never affects the display form.
    let path = rest.split('?').next().unwrap_or(rest);
    // Split on first `/` to separate room from optional event id.
    let (room_enc, event) = match path.split_once('/') {
        Some((r, e)) => (r, Some(e)),
        None => (path, None),
    };
    // Decode percent-encoded `!` and `#` (other bytes are unlikely in a
    // room id / alias, so a minimal decoder is enough).
    let room = room_enc
        .replace("%21", "!")
        .replace("%23", "#");
    // User link: the whole URL is `@user:server`, no event suffix.
    if room.starts_with('@') && event.is_none() {
        return Some(room);
    }
    // Reject URLs whose room part isn't a room id or alias — we don't
    // want to pill arbitrary matrix.to paths we don't recognise.
    if !(room.starts_with('#') || room.starts_with('!')) {
        return None;
    }
    let resolved = resolve_room_name(&room);
    // Room-only link → resolved name if we have it. If we can't
    // resolve, only fall back to the raw id for an alias (`#name`) —
    // those are human-readable on their own. For a raw room id
    // (`!abc:server`) we'd rather not paste an opaque identifier, so
    // show a generic "# external room" that still signals the shape.
    if event.is_none() || event == Some("") {
        return Some(match resolved {
            Some(name) => format!("# {name}"),
            None if room.starts_with('#') => room,
            None => "# external room".to_string(),
        });
    }
    // Event link → point at a specific message. Plain English reads
    // clearer than an emoji prefix (and avoids the emoji-font fallback
    // that coloured our 🔗 inconsistently inside the pill body). When
    // the room isn't in our cache (user isn't a member), fall back to
    // a generic label — the full URL is still in the href for power
    // users who want to copy it.
    Some(match resolved {
        Some(name) => format!("Comment linked to {name}"),
        None => "Linked comment".to_string(),
    })
}

/// Detect Matrix room aliases (#name:server.tld) in Pango-escaped text and
/// wrap them in anchor tags. Linkification pass runs AFTER `linkify_http`
/// so aliases already inside `<a>` tags (web links happening to contain a
/// `#`) are skipped — we scan outside-of-tag text by tracking depth.
///
/// Match rules:
///   * `#` not preceded by an alphanumeric (avoids `bug#123` style IDs)
///   * followed by one or more `[A-Za-z0-9_.-]` (the localpart)
///   * a `:` separator
///   * a server name with at least one `.` (guards against `#foo:bar`)
///   * server name stops at whitespace, `/`, `<`, `>`, or end of string
fn linkify_aliases(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut result = String::with_capacity(text.len() + 32);
    let mut i = 0;
    let mut in_tag = false;
    // Depth counter for `<a>...</a>` nesting. linkify_http runs before us
    // and may have produced an anchor whose *display* text contains a
    // literal `#alias:server` (e.g. a matrix.to pill decoded from `%23`).
    // We must not re-wrap that text in another anchor, so skip alias
    // matching while anchor_depth > 0.
    let mut anchor_depth: i32 = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'<' {
            in_tag = true;
            // Opening anchor: `<a ` or `<a>`.
            if bytes.get(i + 1) == Some(&b'a')
                && matches!(bytes.get(i + 2), Some(b' ') | Some(b'>'))
            {
                anchor_depth += 1;
            }
            // Closing anchor: `</a>`.
            if bytes.get(i + 1) == Some(&b'/')
                && bytes.get(i + 2) == Some(&b'a')
                && bytes.get(i + 3) == Some(&b'>')
            {
                anchor_depth -= 1;
            }
            result.push(b as char);
            i += 1;
            continue;
        }
        if b == b'>' {
            in_tag = false;
            result.push(b as char);
            i += 1;
            continue;
        }
        if in_tag || anchor_depth > 0 || b != b'#' {
            // push this byte verbatim; we're copying ASCII directly and
            // any multi-byte UTF-8 content flows through unmodified via
            // pushing raw bytes … but push_str from the slice is simpler
            // and handles multi-byte correctly.
            let start = i;
            while i < bytes.len() {
                let c = bytes[i];
                if c == b'<' || c == b'>' { break; }
                // Only break on `#` when we'd actually try to alias-match
                // it — i.e. outside both raw tags and anchor contents.
                // Breaking inside an anchor would stall the outer loop
                // (zero-byte copy, same byte considered again → hang).
                if !in_tag && anchor_depth == 0 && c == b'#' { break; }
                i += 1;
            }
            result.push_str(&text[start..i]);
            continue;
        }
        // b == b'#' and not in a tag. Check preceding char isn't alnum.
        let prev_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        if !prev_ok {
            result.push('#');
            i += 1;
            continue;
        }
        // Scan localpart.
        let local_start = i + 1;
        let mut j = local_start;
        while j < bytes.len() {
            let c = bytes[j];
            if c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || c == b'-' {
                j += 1;
            } else {
                break;
            }
        }
        if j == local_start || j >= bytes.len() || bytes[j] != b':' {
            result.push('#');
            i += 1;
            continue;
        }
        // Scan server name (must contain a dot; stops at whitespace/end).
        let server_start = j + 1;
        let mut k = server_start;
        let mut saw_dot = false;
        while k < bytes.len() {
            let c = bytes[k];
            if c.is_ascii_whitespace() || c == b'<' || c == b'>' || c == b'/' { break; }
            if c == b'.' { saw_dot = true; }
            k += 1;
        }
        if !saw_dot || k == server_start {
            result.push('#');
            i += 1;
            continue;
        }
        let alias = &text[i..k]; // #local:server
        // Percent-encode the '#' for the matrix.to fragment.
        let href = format!("https://matrix.to/#/%23{}", &text[local_start..k]);
        result.push_str(&render_pill_markup(&href, alias));
        i = k;
    }
    result
}

/// Scan a message body for every `#localpart:server.tld` alias and return
/// the unique ones in order of first appearance. Rules match
/// `linkify_aliases` so the two passes agree on what counts as an alias:
/// `#` not preceded by alphanumeric, a non-empty localpart of `[A-Za-z0-9_.-]`,
/// a `:`, a server name containing at least one `.`, terminated by
/// whitespace / `<` / `>` / `/` / end.
///
/// Pure — no directory lookup, no side effects. Caller filters against
/// `crate::directory::room_id_for_alias` to decide which to resolve.
/// Extract the first `https://matrix.to/#/<room>/<event>` link in `text`,
/// returning `(room_part, event_id)` where `room_part` is the
/// percent-decoded room id or alias (`!id:server` or `#alias:server`).
/// Returns `None` for any URL shape that isn't a matrix.to *event*
/// link — room-only matrix.to URLs, user links, and bare URLs are all
/// rejected here so the caller doesn't need to second-guess.
///
/// Used by the link-preview card path: when the message body cites a
/// specific Matrix event, the bound row tries to look the event up
/// in the local cache and renders a Discord-style preview under the
/// body. The pill itself (`matrix_to_pill_text`) still handles the
/// inline rendering — this helper just teases out the structured pair.
pub(crate) fn extract_first_matrix_to_event_link(text: &str) -> Option<(String, String)> {
    for word in text.split_whitespace() {
        let url = word.trim_end_matches(|c: char| matches!(c, '.' | ',' | ')' | '!' | '?' | ';' | ':'));
        let Some(rest) = url.strip_prefix("https://matrix.to/#/") else { continue };
        let path = rest.split('?').next().unwrap_or(rest);
        let Some((room_enc, event)) = path.split_once('/') else { continue };
        if event.is_empty() || !event.starts_with('$') { continue; }
        let room = room_enc.replace("%21", "!").replace("%23", "#");
        if room.starts_with('!') || room.starts_with('#') {
            return Some((room, event.to_string()));
        }
    }
    None
}

pub(crate) fn extract_aliases(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'#' {
            i += 1;
            continue;
        }
        let prev_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        if !prev_ok {
            i += 1;
            continue;
        }
        let local_start = i + 1;
        let mut j = local_start;
        while j < bytes.len() {
            let c = bytes[j];
            if c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || c == b'-' {
                j += 1;
            } else {
                break;
            }
        }
        if j == local_start || j >= bytes.len() || bytes[j] != b':' {
            i += 1;
            continue;
        }
        let server_start = j + 1;
        let mut k = server_start;
        let mut saw_dot = false;
        while k < bytes.len() {
            let c = bytes[k];
            if c.is_ascii_whitespace() || c == b'<' || c == b'>' || c == b'/' { break; }
            if c == b'.' { saw_dot = true; }
            k += 1;
        }
        if !saw_dot || k == server_start {
            i += 1;
            continue;
        }
        let alias = text[i..k].to_string();
        if seen.insert(alias.clone()) {
            out.push(alias);
        }
        i = k;
    }
    out
}

fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
     .replace('<', "&lt;")
     .replace('>', "&gt;")
     .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bold_italic_segment() {
        let segs = html_to_segments("<b>hello</b>");
        assert!(matches!(&segs[0], Segment::Text(t) if t.contains("<b>hello</b>")));
    }

    #[test]
    fn test_code_block_becomes_code_segment() {
        let segs = html_to_segments("<p>Look:</p><pre><code class=\"language-rust\">let x = 1;</code></pre>");
        assert!(segs.iter().any(|s| matches!(s, Segment::Code { lang, .. } if lang == "rust")));
    }

    #[test]
    fn test_inline_code_stays_text() {
        let segs = html_to_segments("<code>x</code>");
        assert!(matches!(&segs[0], Segment::Text(t) if t.contains("<tt>x</tt>")));
    }

    #[test]
    fn test_entities() {
        let segs = html_to_segments("a &amp; b");
        assert!(matches!(&segs[0], Segment::Text(t) if t.contains("a &amp; b")));
    }

    #[test]
    fn test_link() {
        let segs = html_to_segments("<a href=\"https://example.com\">click</a>");
        assert!(matches!(&segs[0], Segment::Text(t) if t.contains("href=\"https://example.com\"")));
    }

    #[test]
    fn test_bare_url_in_html_text_is_linkified() {
        // A bare URL in the text content of HTML (no <a> tag) should be linkified.
        let segs = html_to_segments("Check out https://example.com for details");
        assert!(matches!(&segs[0], Segment::Text(t) if t.contains("href=\"https://example.com\"")),
            "bare URL should be wrapped in <a href>");
    }

    #[test]
    fn test_bare_url_in_paragraph_is_linkified() {
        // pulldown_cmark wraps plain text in <p>; the stripped result is bare text.
        let segs = html_to_segments("why no link? - https://github.com/foo/bar#section");
        assert!(matches!(&segs[0], Segment::Text(t) if t.contains("href=\"https://github.com/foo/bar#section\"")),
            "URL with fragment should be linkified");
    }

    #[test]
    fn test_linkify_urls_basic() {
        let out = linkify_urls("see https://example.com here");
        assert!(out.contains("<a href=\"https://example.com\">https://example.com</a>"));
    }

    #[test]
    fn test_linkify_matrix_to_event_link_becomes_pill() {
        let url = "https://matrix.to/#/!DFxCKzUpzBjtSORjyb:matrix.org/$abc";
        let out = linkify_urls(&format!("see {url} please"));
        // href stays intact so the existing matrix-uri router fires on click.
        assert!(out.contains(&format!("<a href=\"{url}\">")), "href missing: {out}");
        // Unknown room → generic "Linked comment" label, no raw id.
        assert!(out.contains("Linked comment"), "fallback label missing: {out}");
        // Pill styling applied (green fill, white bold text).
        assert!(out.contains("background=\"#26a269\""), "pill style missing: {out}");
        // Raw URL does not appear in the rendered body.
        assert!(!out.contains(">https://matrix.to"), "raw URL leaked: {out}");
        // Opaque room id must not appear in the pill text.
        assert!(!out.contains(">!DFxCKzUpzBjtSORjyb"), "room id leaked: {out}");
    }

    #[test]
    fn test_linkify_matrix_to_event_link_uses_cached_name() {
        // With a cached room name, the pill reads "Comment linked to <Name>".
        super::set_room_name("!cached:example.org", "My Room");
        let url = "https://matrix.to/#/!cached:example.org/$abc";
        let out = linkify_urls(&format!("see {url}"));
        assert!(out.contains("Comment linked to My Room"),
            "cached room name missing: {out}");
    }

    #[test]
    fn test_linkify_matrix_to_room_alias_becomes_pill() {
        let url = "https://matrix.to/#/%23room:example.org";
        let out = linkify_urls(&format!("go to {url}"));
        assert!(out.contains("#room:example.org\u{a0}</span>"),
            "alias pill body missing: {out}");
    }

    #[test]
    fn test_linkify_matrix_to_user_link_becomes_pill() {
        let url = "https://matrix.to/#/@alice:example.org";
        let out = linkify_urls(&format!("ping {url}"));
        assert!(out.contains("@alice:example.org\u{a0}</span>"),
            "user pill body missing: {out}");
    }

    #[test]
    fn test_linkify_plain_url_stays_plain() {
        // Non-matrix.to URLs should still render as normal inline links.
        let out = linkify_urls("see https://example.com here");
        assert!(out.contains("<a href=\"https://example.com\">https://example.com</a>"));
        assert!(!out.contains("background=\"#26a269\""), "plain URL got pill: {out}");
    }

    #[test]
    fn test_linkify_urls_no_false_positive() {
        // "http" that is not a proper URL prefix should not be linkified.
        let out = linkify_urls("not a link: httpx://foo");
        assert!(!out.contains("<a"), "httpx:// should not be linkified");
    }

    #[test]
    fn test_linkify_room_alias() {
        let out = linkify_urls("see #outreachy:gnome.org for info");
        assert!(out.contains("<a href=\"https://matrix.to/#/%23outreachy:gnome.org\">"),
            "anchor href missing: {out}");
        // Alias text survives inside the pill body span, padded with NBSPs.
        assert!(out.contains("#outreachy:gnome.org\u{a0}</span></a>"),
            "pill body missing: {out}");
    }

    #[test]
    fn test_linkify_room_alias_no_bare_hashes() {
        // Bug tags (#123) and word-suffix hashes (foo#bar) must NOT linkify.
        let out = linkify_urls("see bug#123 and foo#bar for details");
        assert!(!out.contains("<a"), "bug#123 should not match: {out}");
    }

    #[test]
    fn test_linkify_room_alias_requires_dotted_server() {
        // A server name without a dot is not a real alias.
        let out = linkify_urls("meet at #lunch:today please");
        assert!(!out.contains("<a"), "single-label server should not match: {out}");
    }

    #[test]
    fn test_linkify_room_alias_and_url_together() {
        let out = linkify_urls("see #room:example.org or https://example.org/doc");
        assert!(out.contains("<a href=\"https://matrix.to/#/%23room:example.org\">"));
        assert!(out.contains("<a href=\"https://example.org/doc\">https://example.org/doc</a>"));
    }

    // ── extract_aliases (used by #1 async resolve) ──────────────────────

    #[test]
    fn extract_aliases_plain_text() {
        let got = extract_aliases("come to #rust:example.org or #fedi.social:mastodon.social");
        assert_eq!(got, vec!["#rust:example.org", "#fedi.social:mastodon.social"]);
    }

    #[test]
    fn extract_aliases_dedupes_repeats() {
        let got = extract_aliases("#r:example.org then later #r:example.org again");
        assert_eq!(got, vec!["#r:example.org"]);
    }

    #[test]
    fn extract_aliases_rejects_bare_hash_counters() {
        // A `#123` counter or a `bug#42` issue reference is not an alias.
        let got = extract_aliases("filed bug#42 against #123 tracker");
        assert!(got.is_empty(), "got {got:?}");
    }

    #[test]
    fn extract_aliases_requires_dotted_server() {
        // `#a:b` has no `.` in the server — not a valid alias.
        let got = extract_aliases("try #a:b and #real:server.tld");
        assert_eq!(got, vec!["#real:server.tld"]);
    }

    #[test]
    fn extract_aliases_empty_on_no_match() {
        let got = extract_aliases("just plain text with no aliases");
        assert!(got.is_empty());
    }

    #[test]
    fn extract_aliases_stops_at_whitespace_and_tag_edges() {
        // Trailing slash, whitespace, and angle brackets all terminate the server name.
        let got = extract_aliases("goto #foo:server.tld/path next");
        assert_eq!(got, vec!["#foo:server.tld"]);
    }

    // ── extract_first_matrix_to_event_link (link-preview card) ────────

    #[test]
    fn extract_event_link_basic_room_id() {
        let body = "see https://matrix.to/#/%21abc:example.org/$evt123 for context";
        let got = extract_first_matrix_to_event_link(body);
        assert_eq!(got, Some(("!abc:example.org".to_string(), "$evt123".to_string())));
    }

    #[test]
    fn extract_event_link_basic_alias() {
        let body = "https://matrix.to/#/%23room:example.org/$evt456";
        let got = extract_first_matrix_to_event_link(body);
        assert_eq!(got, Some(("#room:example.org".to_string(), "$evt456".to_string())));
    }

    #[test]
    fn extract_event_link_strips_via_query() {
        let body = "https://matrix.to/#/%21abc:example.org/$evt789?via=other.org";
        let got = extract_first_matrix_to_event_link(body);
        assert_eq!(got, Some(("!abc:example.org".to_string(), "$evt789".to_string())));
    }

    #[test]
    fn extract_event_link_rejects_room_only() {
        // No /$event suffix → room-only link, not an event reference.
        let body = "join https://matrix.to/#/%23room:example.org";
        assert_eq!(extract_first_matrix_to_event_link(body), None);
    }

    #[test]
    fn extract_event_link_rejects_user_link() {
        let body = "ping https://matrix.to/#/@alice:example.org";
        assert_eq!(extract_first_matrix_to_event_link(body), None);
    }

    #[test]
    fn extract_event_link_returns_first() {
        let body = "https://matrix.to/#/%21a:s.tld/$one and https://matrix.to/#/%21b:s.tld/$two";
        let got = extract_first_matrix_to_event_link(body);
        assert_eq!(got, Some(("!a:s.tld".to_string(), "$one".to_string())));
    }

    #[test]
    fn extract_event_link_handles_trailing_punctuation() {
        let body = "see https://matrix.to/#/%21abc:example.org/$evt123, please";
        let got = extract_first_matrix_to_event_link(body);
        assert_eq!(got, Some(("!abc:example.org".to_string(), "$evt123".to_string())));
    }
}
