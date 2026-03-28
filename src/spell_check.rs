// spell_check.rs — lightweight spell-checking for the compose area.
//
// Uses enchant-2 (libenchant) via the `enchant` crate. The library's
// `Broker` and `Dict` types are not `Send`, so we keep them in a
// `thread_local!` that is only accessed from the GTK main thread.

use enchant::Broker;
use gtk::prelude::*;

thread_local! {
    static BROKER: std::cell::RefCell<Option<Broker>> = const { std::cell::RefCell::new(None) };
}

/// Return the LANG tag to use (e.g. "en_US"). Falls back to "en_US".
fn spell_lang() -> String {
    let lang = std::env::var("LANG").unwrap_or_default();
    // "en_US.UTF-8" → "en_US"; "C" → fallback
    let tag = lang.split('.').next().unwrap_or("").replace('-', "_");
    if tag.len() >= 2 && tag != "C" && tag != "POSIX" {
        tag
    } else {
        "en_US".to_string()
    }
}

/// Call `f` with a live dictionary, initialising the broker on first use.
/// Returns `None` if enchant is unavailable. GTK main thread only.
fn with_dict<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&enchant::Dict) -> R,
{
    BROKER.with(|cell| {
        {
            let mut opt = cell.borrow_mut();
            if opt.is_none() {
                *opt = Some(Broker::new());
            }
        }
        let mut opt = cell.borrow_mut();
        let broker = opt.as_mut().unwrap();
        let lang = spell_lang();
        let dict = broker
            .request_dict(&lang)
            .or_else(|_| broker.request_dict("en_US"))
            .ok()?;
        Some(f(&dict))
    })
}

/// Returns `true` if `word` is correctly spelled (or if enchant is unavailable).
/// GTK main thread only.
pub fn is_correct(word: &str) -> bool {
    with_dict(|dict| dict.check(word).unwrap_or(true)).unwrap_or(true)
}

/// Returns up to 8 spelling suggestions for `word`.
/// GTK main thread only.
pub fn suggestions(word: &str) -> Vec<String> {
    with_dict(|dict| dict.suggest(word)).unwrap_or_default()
}

/// Adds `word` to the user's personal enchant dictionary (persists across runs).
/// GTK main thread only.
pub fn add_to_dictionary(word: &str) {
    with_dict(|dict| dict.add(word));
}

/// Apply spell-check underlines to `buf`.
///
/// Clears the "misspelled" tag across the whole buffer, then re-applies it
/// to every misspelled word found by `extract_words`. Called from both the
/// buffer's `changed` signal and the "Add to dictionary" handler.
///
/// Requires the "misspelled" TextTag to already exist in the buffer's tag table.
/// GTK main thread only.
pub fn check_buffer(buf: &gtk::TextBuffer) {
    let text = {
        let start = buf.start_iter();
        let end = buf.end_iter();
        buf.text(&start, &end, false).to_string()
    };
    buf.remove_tag_by_name("misspelled", &buf.start_iter(), &buf.end_iter());
    for (byte_start, byte_end) in extract_words(&text) {
        let word = &text[byte_start..byte_end];
        if !is_correct(word) {
            let char_start = text[..byte_start].chars().count() as i32;
            let char_end = text[..byte_end].chars().count() as i32;
            let iter_start = buf.iter_at_offset(char_start);
            let iter_end = buf.iter_at_offset(char_end);
            buf.apply_tag_by_name("misspelled", &iter_start, &iter_end);
        }
    }
}

/// Extract byte ranges `(start, end)` for each spell-checkable word in `text`.
///
/// Skips:
/// - URLs (tokens containing `://` or preceded by `/`)
/// - @-mentions and #-hashtags
/// - Numeric tokens and single-character tokens
/// - Markdown fenced code spans (backtick-delimited regions)
///
/// This function is pure (no I/O, no GTK) so it can be unit-tested.
pub fn extract_words(text: &str) -> Vec<(usize, usize)> {
    let mut words = Vec::new();
    // Collect (byte_offset, char) so we can compute byte ranges.
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let n = chars.len();
    let mut i = 0;
    let mut in_backtick = false;

    while i < n {
        let (byte_i, c) = chars[i];

        // Toggle in/out of backtick code spans.
        if c == '`' {
            in_backtick = !in_backtick;
            i += 1;
            continue;
        }
        if in_backtick {
            i += 1;
            continue;
        }

        // Skip @-mentions and #-hashtags — consume until next whitespace.
        if c == '@' || c == '#' {
            while i < n && !chars[i].1.is_whitespace() {
                i += 1;
            }
            continue;
        }

        // Skip http/https URLs — consume until whitespace.
        if c == 'h' && text[byte_i..].starts_with("http") {
            while i < n && !chars[i].1.is_whitespace() {
                i += 1;
            }
            continue;
        }

        // Not the start of a word?
        if !c.is_alphabetic() {
            i += 1;
            continue;
        }

        // Collect alphabetic run (allow apostrophe and hyphen mid-word).
        let word_start = byte_i;
        let mut j = i;
        while j < n {
            let (_, wc) = chars[j];
            if wc.is_alphanumeric() || wc == '\'' || wc == '-' {
                j += 1;
            } else {
                break;
            }
        }
        let word_byte_end = if j < n { chars[j].0 } else { text.len() };
        let word = &text[word_start..word_byte_end];
        i = j;

        // Skip tokens with digits.
        if word.chars().any(|c| c.is_ascii_digit()) {
            continue;
        }

        // Skip tokens that look like URL fragments (contain `/` or `:`).
        if word.contains('/') || word.contains(':') {
            continue;
        }

        // Strip trailing punctuation characters.
        let trimmed = word.trim_end_matches(|c: char| c == '\'' || c == '-' || c == '.');
        if trimmed.len() <= 1 {
            continue;
        }

        words.push((word_start, word_start + trimmed.len()));
    }

    words
}

// ── unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::extract_words;

    fn words_in(text: &str) -> Vec<&str> {
        extract_words(text)
            .into_iter()
            .map(|(s, e)| &text[s..e])
            .collect()
    }

    #[test]
    fn plain_sentence() {
        let w = words_in("Hello world");
        assert_eq!(w, vec!["Hello", "world"]);
    }

    #[test]
    fn contraction_kept() {
        let w = words_in("don't stop");
        assert_eq!(w, vec!["don't", "stop"]);
    }

    #[test]
    fn url_skipped() {
        let w = words_in("visit https://example.com for details");
        assert_eq!(w, vec!["visit", "for", "details"]);
    }

    #[test]
    fn mention_skipped() {
        let w = words_in("hey @alice how are you");
        assert_eq!(w, vec!["hey", "how", "are", "you"]);
    }

    #[test]
    fn hashtag_skipped() {
        let w = words_in("check #gnome channel");
        assert_eq!(w, vec!["check", "channel"]);
    }

    #[test]
    fn backtick_code_skipped() {
        let w = words_in("use `someFn` to do it");
        assert_eq!(w, vec!["use", "to", "do", "it"]);
    }

    #[test]
    fn number_skipped() {
        let w = words_in("version 3.14 released");
        assert_eq!(w, vec!["version", "released"]);
    }

    #[test]
    fn empty_string() {
        assert!(extract_words("").is_empty());
    }

    #[test]
    fn single_letter_skipped() {
        let w = words_in("I am a test");
        // "I" and "a" are single-char → skipped
        assert_eq!(w, vec!["am", "test"]);
    }

    #[test]
    fn unicode_word() {
        // Non-ASCII alphabetic chars are accepted.
        let w = words_in("café au lait");
        assert!(w.contains(&"café"));
        assert!(w.contains(&"lait"));
    }

    #[test]
    fn hyphenated_word() {
        let w = words_in("well-known issue");
        assert_eq!(w, vec!["well-known", "issue"]);
    }

    #[test]
    fn byte_offsets_correct() {
        let text = "bad gooood";
        let ranges = extract_words(text);
        assert_eq!(ranges.len(), 2);
        assert_eq!(&text[ranges[0].0..ranges[0].1], "bad");
        assert_eq!(&text[ranges[1].0..ranges[1].1], "gooood");
    }
}
