// media-rewrite plugin — host-swap rewriter for image/GIF URLs.
//
// Some homeservers block or rate-limit popular media hosts (imgur is
// the canonical case — some EU/self-hosted servers drop it entirely,
// others are just slow to proxy it). This plugin lets the user
// supply a map of source hostname → replacement hostname that is
// applied to every image URL `extract_image_url` detects in a
// message body. If no config file is present the plugin is a no-op.
//
// Config file: $XDG_CONFIG_HOME/hikyaku/media_rewrite.json
// Format:
//   { "imgur.com": "rimgo.pussthecat.org",
//     "i.imgur.com": "rimgo.pussthecat.org" }
//
// The match is on the URL host (not a substring) so "imgur.com"
// rewrites `https://imgur.com/abc.png` but does NOT rewrite a
// `https://example.com/?ref=imgur.com` that happens to mention
// imgur in its query string.
//
// Feature: "media-rewrite"
//
// Storage rationale: config, not data. This is a user preference
// that belongs in XDG_CONFIG_HOME alongside settings, not in the
// data dir with caches and session state.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

/// Process-wide rewrite map. Loaded lazily on first `rewrite_url` call.
/// Reloadable via `reload_from_disk` when the user edits the config.
static REWRITE_MAP: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();

fn config_path() -> std::path::PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("hikyaku").join("media_rewrite.json")
}

fn load_map_from_disk() -> HashMap<String, String> {
    let path = config_path();
    let Ok(data) = std::fs::read_to_string(&path) else {
        return HashMap::new();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

fn map() -> &'static RwLock<HashMap<String, String>> {
    REWRITE_MAP.get_or_init(|| RwLock::new(load_map_from_disk()))
}

/// Re-read the config file from disk. Call after the user edits the
/// file so the rewrite takes effect without a restart.
pub fn reload_from_disk() {
    let fresh = load_map_from_disk();
    if let Ok(mut guard) = map().write() {
        *guard = fresh;
    }
}

/// Apply the rewrite map to a URL. Returns the URL unchanged when the
/// map is empty, the URL can't be parsed, or the host has no mapping.
///
/// Matching is exact-on-host: `imgur.com` in the map will rewrite
/// `https://imgur.com/x.png` but NOT `https://i.imgur.com/x.png`
/// (the user must map that subdomain separately). This is
/// intentional — subdomain wildcarding silently reroutes traffic the
/// user may not have intended to redirect.
pub fn rewrite_url(url: &str) -> String {
    let Ok(guard) = map().read() else { return url.to_string() };
    if guard.is_empty() { return url.to_string(); }

    let (scheme, rest) = match url.split_once("://") {
        Some(pair) => pair,
        None => return url.to_string(),
    };
    // host ends at the first '/', '?', or '#'.
    let host_end = rest.find(|c: char| c == '/' || c == '?' || c == '#')
        .unwrap_or(rest.len());
    let host = &rest[..host_end];
    let tail = &rest[host_end..];

    match guard.get(host) {
        Some(replacement) if !replacement.is_empty() => {
            format!("{scheme}://{replacement}{tail}")
        }
        _ => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_map<F: FnOnce()>(pairs: &[(&str, &str)], body: F) {
        let mut fresh: HashMap<String, String> = HashMap::new();
        for (k, v) in pairs {
            fresh.insert((*k).to_string(), (*v).to_string());
        }
        {
            let cell = map();
            let mut w = cell.write().unwrap();
            *w = fresh;
        }
        body();
        // Restore empty so later tests see a clean state.
        let cell = map();
        let mut w = cell.write().unwrap();
        w.clear();
    }

    #[test]
    fn no_map_passes_through() {
        with_map(&[], || {
            assert_eq!(rewrite_url("https://imgur.com/x.png"), "https://imgur.com/x.png");
        });
    }

    #[test]
    fn host_match_rewrites() {
        with_map(&[("imgur.com", "rimgo.pussthecat.org")], || {
            assert_eq!(
                rewrite_url("https://imgur.com/x.png"),
                "https://rimgo.pussthecat.org/x.png"
            );
        });
    }

    #[test]
    fn path_and_query_preserved() {
        with_map(&[("imgur.com", "rimgo.pussthecat.org")], || {
            assert_eq!(
                rewrite_url("https://imgur.com/a/bcd?x=1#frag"),
                "https://rimgo.pussthecat.org/a/bcd?x=1#frag"
            );
        });
    }

    #[test]
    fn subdomain_is_not_matched_implicitly() {
        // Mapping only imgur.com must NOT rewrite i.imgur.com — the
        // user has to opt each subdomain in so they can't accidentally
        // redirect traffic they didn't expect.
        with_map(&[("imgur.com", "rimgo.pussthecat.org")], || {
            assert_eq!(
                rewrite_url("https://i.imgur.com/x.png"),
                "https://i.imgur.com/x.png"
            );
        });
    }

    #[test]
    fn unknown_host_passes_through() {
        with_map(&[("imgur.com", "rimgo.pussthecat.org")], || {
            assert_eq!(
                rewrite_url("https://example.org/pic.png"),
                "https://example.org/pic.png"
            );
        });
    }

    #[test]
    fn malformed_url_passes_through() {
        with_map(&[("imgur.com", "rimgo.pussthecat.org")], || {
            assert_eq!(rewrite_url("not a url"), "not a url");
        });
    }
}
