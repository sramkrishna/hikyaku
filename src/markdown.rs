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
        result.push_str(&format!("<a href=\"{url}\">{url}</a>"));
        rest = &candidate[url_end..];
    }
    result.push_str(rest);
    result
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
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'<' {
            in_tag = true;
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
        if in_tag || b != b'#' {
            // push this byte verbatim; we're copying ASCII directly and
            // any multi-byte UTF-8 content flows through unmodified via
            // pushing raw bytes … but push_str from the slice is simpler
            // and handles multi-byte correctly.
            let start = i;
            while i < bytes.len() {
                let c = bytes[i];
                if c == b'<' || c == b'>' { break; }
                if !in_tag && c == b'#' { break; }
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
        // Pango can't apply widget CSS to a markup span (no class selectors),
        // so style the alias pill via Pango span attributes directly: drop
        // the default link underline, use an accent foreground, and give it
        // a subtle translucent background so it reads as a pill instead of
        // a plain underlined URL. Colours picked to track the libadwaita
        // accent tones; the background alpha (~0.14) keeps contrast in
        // both light and dark themes.
        result.push_str(&format!(
            "<a href=\"{href}\"><span \
                foreground=\"#62a0ea\" \
                background=\"#3584e424\" \
                underline=\"none\">{alias}</span></a>",
        ));
        i = k;
    }
    result
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
        assert!(out.contains(">#outreachy:gnome.org</span></a>"),
            "pill span missing: {out}");
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

}
