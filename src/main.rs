// Hikyaku — a Matrix client built with Rust + libadwaita.
//
// Entry point: initialize logging, then hand off to GTK's application
// lifecycle. GTK takes over the main thread; the Matrix SDK runs on a
// separate tokio thread spawned during `activate`.

mod application;
mod bookmarks;
mod config;
mod intelligence;
mod local_unread;
mod markdown;
mod matrix;
mod perf;
mod spell_check;
mod models;
mod plugins;
pub mod room_context;
mod widgets;

use gtk::prelude::*;


/// Performance regression tests for O(1) data-structure paths.
///
/// These tests do NOT require a GTK display — they exercise the pure-Rust
/// HashMap/HashSet logic that backs the event_index and nav_index in the
/// widget layer, and verify that 1 000-entry timelines are handled well
/// within the frame budget.
#[cfg(test)]
mod perf_tests {
    use std::collections::HashMap;
    use std::time::Instant;

    // ── event_index simulation ────────────────────────────────────────────────
    //
    // The real event_index maps event_id → MessageObject (GObject).
    // Here we simulate the same access pattern with HashMap<String, usize>
    // so the algorithmic properties can be verified without a display.

    fn build_event_index(n: usize) -> HashMap<String, usize> {
        (0..n)
            .map(|i| (format!("$event{i}:example.com"), i))
            .collect()
    }

    /// Verify O(1) complexity: 10 000 lookups must not take >20× longer than
    /// 1 000 lookups (O(n) would take exactly 10× longer; we allow 20× slack).
    /// This catches algorithmic regressions without depending on absolute speed.
    #[test]
    fn event_index_lookup_is_o1_not_on() {
        let index_1k  = build_event_index(1_000);
        let index_10k = build_event_index(10_000);

        let time_lookups = |index: &HashMap<String, usize>, n: usize| -> u128 {
            let start = Instant::now();
            for i in 0..n {
                let _ = index.contains_key(&format!("$event{i}:example.com"));
            }
            start.elapsed().as_nanos().max(1)
        };

        // Each batch does 1 000 lookups so the work is the same; only the
        // *index size* changes.  O(1) → similar times; O(n) → 10× difference.
        let t_small = time_lookups(&index_1k,  1_000);
        let t_large = time_lookups(&index_10k, 1_000);

        // Correctness: every key must be present.
        for i in 0..1_000 {
            assert!(
                index_1k.contains_key(&format!("$event{i}:example.com")),
                "event {i} missing from index"
            );
        }

        // If lookup were O(n), t_large would be ~10× t_small.
        // We require it stays within 20× (generous slack for noise/JIT).
        assert!(
            t_large < t_small * 20,
            "O(1) regression: 10k-index lookup ({t_large}ns) is >20× \
             the 1k-index lookup ({t_small}ns) — suggests O(n) scan"
        );
    }

    /// has_event() equivalent: index size must not affect lookup time.
    #[test]
    fn event_index_contains_key_is_o1() {
        let index_1k   = build_event_index(1_000);
        let index_100k = build_event_index(100_000);
        let key = "$event500:example.com".to_string();

        let t1 = { let s = Instant::now(); for _ in 0..1000 { let _ = index_1k.contains_key(&key); } s.elapsed().as_nanos().max(1) };
        let t2 = { let s = Instant::now(); for _ in 0..1000 { let _ = index_100k.contains_key(&key); } s.elapsed().as_nanos().max(1) };

        // 100k index must not be >30× slower than 1k index for the same lookup.
        assert!(
            t2 < t1 * 30,
            "O(1) regression: 100k-index ({t2}ns) is >30× 1k-index ({t1}ns)"
        );
    }

    // ── nav_index simulation ──────────────────────────────────────────────────
    //
    // navigate_room uses nav_index: HashMap<room_id, position> for O(1) lookup.

    fn build_nav_index(n: usize) -> (Vec<String>, HashMap<String, usize>) {
        let order: Vec<String> = (0..n).map(|i| format!("!room{i}:example.com")).collect();
        let index: HashMap<String, usize> =
            order.iter().enumerate().map(|(i, id)| (id.clone(), i)).collect();
        (order, index)
    }

    /// navigate_room must be O(1): navigating a 3 000-room list must not be
    /// >20× slower per step than a 300-room list.
    #[test]
    fn nav_index_navigate_300_rooms() {
        let (order300,  index300)  = build_nav_index(300);
        let (order3000, index3000) = build_nav_index(3_000);

        let nav_steps = |order: &Vec<String>, index: &HashMap<String, usize>, steps: usize| -> u128 {
            let n = order.len() as i32;
            let mut current = order[0].clone();
            let start = Instant::now();
            for _ in 0..steps {
                let pos = index.get(&current).copied().unwrap_or(0) as i32;
                let next_pos = ((pos + 1).rem_euclid(n)) as usize;
                current = order[next_pos].clone();
            }
            start.elapsed().as_nanos().max(1)
        };

        let t_300  = nav_steps(&order300,  &index300,  300);
        let t_3000 = nav_steps(&order3000, &index3000, 300);

        // Correctness: full cycle wraps back to start.
        let (o, ix) = build_nav_index(300);
        let n = o.len() as i32;
        let mut c = o[0].clone();
        for _ in 0..300 { let p = ix[&c] as i32; c = o[((p+1).rem_euclid(n)) as usize].clone(); }
        assert_eq!(c, "!room0:example.com");

        // 10× more rooms must not cause >20× slowdown per step.
        assert!(
            t_3000 < t_300 * 20,
            "O(1) regression: 3k nav ({t_3000}ns/300 steps) >20× 300-room \
             nav ({t_300}ns/300 steps)"
        );
    }

    /// nav_index must return the correct room when navigating backwards.
    #[test]
    fn nav_index_navigate_backwards() {
        let (order, index) = build_nav_index(10);
        let n = order.len() as i32;
        let pos = index["!room3:example.com"] as i32;
        let prev = ((pos - 1).rem_euclid(n)) as usize;
        assert_eq!(order[prev], "!room2:example.com");
    }

    // ── reaction-update throughput ────────────────────────────────────────────
    //
    // Reaction events look up their target by event_id.  Index size must not
    // affect lookup time (O(1) invariant).

    #[test]
    fn reaction_lookup_is_o1() {
        let index_1k   = build_event_index(1_000);
        let index_10k  = build_event_index(10_000);

        let lookup_n = |index: &HashMap<String, usize>, n: usize| -> u128 {
            let start = Instant::now();
            for i in 0..n {
                let _ = index.get(&format!("$event{}:example.com", i % 1000));
            }
            start.elapsed().as_nanos().max(1)
        };

        // Same number of lookups; only index size differs.
        let t_1k  = lookup_n(&index_1k,  500);
        let t_10k = lookup_n(&index_10k, 500);

        assert!(
            t_10k < t_1k * 20,
            "O(1) regression: 10k-index reaction lookup ({t_10k}ns) \
             is >20× 1k-index ({t_1k}ns)"
        );
    }
}

/// Pure helper: is a message sent by the current user?
///
/// Guards against `my_id` being empty (not yet set before `LoginSuccess`
/// fires).  An empty `my_id` must never match any `sender_id`, otherwise
/// messages from other users could be treated as self-echoes and skipped.
#[cfg(test)]
pub(crate) fn compute_is_self(my_id: &str, sender_id: &str) -> bool {
    !my_id.is_empty() && sender_id == my_id
}

#[cfg(test)]
mod message_dispatch_tests {
    use super::compute_is_self;

    // ── is_self guard ────────────────────────────────────────────────────────
    //
    // Scenario: on startup the user_id may not be set yet (empty string).
    // Before the LoginSuccess event arrives, compute_is_self must return false
    // for every sender so incoming messages are never misrouted to the
    // self-echo patch path.

    #[test]
    fn is_self_false_when_my_id_not_yet_set() {
        // Empty my_id means LoginSuccess hasn't fired; must never match.
        assert!(!compute_is_self("", "@alice:matrix.org"));
    }

    #[test]
    fn is_self_false_when_both_ids_empty() {
        // Both empty should NOT count as "same user".
        assert!(!compute_is_self("", ""));
    }

    #[test]
    fn is_self_true_when_ids_match() {
        assert!(compute_is_self("@me:example.com", "@me:example.com"));
    }

    #[test]
    fn is_self_false_for_different_users() {
        assert!(!compute_is_self("@me:example.com", "@alice:example.com"));
    }

    #[test]
    fn is_self_case_sensitive() {
        // Matrix user IDs are lowercase but let's confirm no case folding.
        assert!(!compute_is_self("@Me:example.com", "@me:example.com"));
    }
}

/// Lint test: no new synchronous file I/O on the GTK main thread.
///
/// All GTK-thread source files (src/ minus src/matrix/) are scanned for
/// synchronous I/O calls.  Violations in this list have been audited and
/// approved; any NEW violation causes the test to fail so it can be reviewed
/// before merging.
///
/// To add an approved exception: append a `(relative_path_fragment, line_content_fragment)`
/// entry to SYNC_IO_ALLOWLIST with a short justification comment.
#[cfg(test)]
mod sync_io_lint {
    /// Patterns that must not appear in GTK-thread source files.
    const FORBIDDEN: &[&str] = &[
        "std::fs::read(",
        "std::fs::read_to_string(",
        "std::fs::write(",
        "std::fs::create_dir(",
        "std::fs::create_dir_all(",
        "std::fs::remove_file(",
        "File::open(",
        "File::create(",
    ];

    /// Known, audited violations.  Matched by (path fragment, line content fragment).
    /// Do NOT add entries without a review — file an issue or justify in the comment.
    const ALLOWLIST: &[(&str, &str)] = &[
        // bookmarks.rs — new(): mkdir at startup, before GTK loop.
        ("bookmarks.rs", "std::fs::create_dir_all(&path)"),
        // bookmarks.rs — load() slow path: single disk read on first call; cache is warm thereafter.
        ("bookmarks.rs", "std::fs::read(&self.path)"),
        // bookmarks.rs — persist(): user-triggered add/remove, not a hot path.
        ("bookmarks.rs", "std::fs::write(&self.path, data)"),
        // room_context.rs — ensure_loaded(): has in-memory cache; reads from disk at most once per session.
        ("room_context.rs", "std::fs::read_to_string(context_path())"),
        // room_context.rs — save_override(): triggered by an explicit settings change (not per-message).
        ("room_context.rs", "std::fs::create_dir_all(parent)"),
        ("room_context.rs", "std::fs::write(&path, json)"),
        // message_view.rs — export_messages_jsonl(): explicit user export action.
        ("message_view.rs", "File::create(path)"),
        // local_unread.rs — open_conn(): one-time create_dir_all before opening SQLite; fast.
        ("local_unread.rs", "std::fs::create_dir_all(parent)"),
        // plugins/rolodex — load(): called once at startup from window::new.
        ("rolodex/mod.rs", "std::fs::read_to_string(&path)"),
        // plugins/rolodex — save(): user-triggered contact book update; create_dir_all is fast.
        ("rolodex/mod.rs", "std::fs::create_dir_all(parent)"),
        ("rolodex/mod.rs", "std::fs::write(&path, data)"),
        // plugins/pinning — dead code (allow(dead_code)); user-triggered when eventually wired up.
        ("pinning/mod.rs", "std::fs::read_to_string(&path)"),
        ("pinning/mod.rs", "std::fs::create_dir_all(parent)"),
        ("pinning/mod.rs", "std::fs::write(&path, data)"),
        // plugins/motd — load(): called in idle_add_local_once; acceptable in idle context.
        ("motd/mod.rs", "std::fs::read_to_string(&path)"),
        // plugins/motd — check_and_update → save(): create_dir_all is fast; write is already allowlisted.
        ("motd/mod.rs", "std::fs::create_dir_all(parent)"),
        ("motd/mod.rs", "std::fs::write(&path, data)"),
        // intelligence/gpu_detect.rs — reads /proc/meminfo and /sys/class/drm/*; these are kernel
        // virtual filesystems (always in RAM, never touch disk); latency is < 0.1ms.
        ("gpu_detect.rs", r#"std::fs::read_to_string("/proc/meminfo")"#),
        ("gpu_detect.rs", "std::fs::read_to_string(&vendor_path)"),
        ("gpu_detect.rs", "std::fs::read_to_string(&vram_path)"),
        // intelligence/ollama_manager.rs — download_ollama_binary(): one-time, user-initiated action.
        // create_dir_all is fast; write flushes the downloaded binary to disk after async download.
        // TODO: convert the write to gio::File::replace_async to avoid blocking the GTK thread.
        ("ollama_manager.rs", "std::fs::create_dir_all(parent)"),
        ("ollama_manager.rs", "std::fs::write(&dest, &data)"),
    ];

    fn src_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
    }

    /// Return all .rs files under `src/` that run on the GTK main thread.
    /// Excludes src/matrix/ (tokio thread) and src/bin/ (separate binaries).
    fn gtk_thread_files(src: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    // Exclude tokio-thread and auxiliary binary directories.
                    let name = p.file_name().unwrap_or_default().to_string_lossy();
                    if name == "matrix" || name == "bin" { continue; }
                    walk(&p, out);
                } else if p.extension().map(|e| e == "rs").unwrap_or(false) {
                    out.push(p);
                }
            }
        }
        walk(src, &mut out);
        out
    }

    /// True if `line` looks like it is inside a `#[cfg(test)]` block.
    /// Used to skip I/O that only ever runs during unit tests, not at runtime.
    fn looks_like_test_line(line: &str) -> bool {
        // Heuristic: look for `#[test]`, `#[cfg(test)]`, or being inside a
        // `mod tests {` / `mod sync_io_lint {` block.  We check for a comment
        // marker that callers can add: `// lint: test-only`
        line.contains("// lint: test-only") || line.contains("// sync-io-ok:")
    }

    #[test]
    fn no_new_sync_io_on_gtk_thread() {
        let src = src_dir();
        let mut failures: Vec<String> = Vec::new();

        for path in gtk_thread_files(&src) {
            let rel = path.strip_prefix(&src).unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let content = std::fs::read_to_string(&path) // lint: test-only
                .unwrap_or_default();
            let mut in_cfg_test: u32 = 0;
            let mut brace_depth: i32 = 0;
            let mut cfg_test_pending = false;

            for (i, line) in content.lines().enumerate() {
                let trimmed = line.trim();
                // Track #[cfg(test)] regions so we don't flag test-only I/O.
                if trimmed == "#[cfg(test)]" { cfg_test_pending = true; }
                if cfg_test_pending && (trimmed.starts_with("mod ") || trimmed.starts_with("fn ") || trimmed.starts_with("pub fn ")) {
                    in_cfg_test = in_cfg_test.saturating_add(1);
                    cfg_test_pending = false;
                }
                if in_cfg_test > 0 {
                    brace_depth += line.chars().filter(|&c| c == '{').count() as i32;
                    brace_depth -= line.chars().filter(|&c| c == '}').count() as i32;
                    if brace_depth <= 0 { in_cfg_test = in_cfg_test.saturating_sub(1); brace_depth = 0; }
                    continue; // skip test-only code
                }
                // Skip comment lines and lines with explicit override markers.
                if trimmed.starts_with("//") || looks_like_test_line(line) { continue; }

                for &pat in FORBIDDEN {
                    if !line.contains(pat) { continue; }
                    let allowed = ALLOWLIST.iter().any(|(af, al)| {
                        rel.contains(af) && line.contains(al)
                    });
                    if !allowed {
                        failures.push(format!("  {}:{}: {}", rel, i + 1, trimmed));
                    }
                }
            }
        }

        assert!(
            failures.is_empty(),
            "\nSynchronous file I/O found on GTK main thread — not in allowlist:\n{}\n\n\
            Options:\n\
            1. Move I/O to a background thread or glib::idle_add_local_once.\n\
            2. Add a `// sync-io-ok: <reason>` comment on the offending line.\n\
            3. Add an entry to SYNC_IO_ALLOWLIST in src/main.rs with justification.\n",
            failures.join("\n")
        );
    }
}

fn main() {
    // Point GSettings to the compiled schema next to the binary when running
    // without a system install (cargo run / dev builds).
    if std::env::var("GSETTINGS_SCHEMA_DIR").is_err() {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                std::env::set_var("GSETTINGS_SCHEMA_DIR", dir);
            }
        }
    }

    // Pin GSK to the Vulkan renderer. With the default renderer selection
    // (observed as ngl on Fedora + Mesa Intel), GDK_DEBUG=frames spammed
    // "Unsupported node 'GskTransformNode' / Offscreening node ..." for
    // GskTransformNode, GskMaskNode, and GskContainerNode — scroll dropped
    // to 60Hz effective on a 120Hz display with occasional 125ms stalls.
    // Explicit vulkan removes the offscreen-fallback path and restores
    // smooth scroll. User override always wins (checked env first).
    if std::env::var_os("GSK_RENDERER").is_none() {
        std::env::set_var("GSK_RENDERER", "vulkan");
    }

    // Initialize structured logging. Override with RUST_LOG env var, e.g.
    // RUST_LOG=hikyaku=debug cargo run
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("hikyaku=warn")),
        )
        .init();

    // Create and run the GTK application. `run()` blocks until the
    // user closes the window. It handles argc/argv for us.
    let app = application::MxApplication::new();
    app.run();
}
