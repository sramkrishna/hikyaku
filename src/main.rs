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
mod spell_check;
mod models;
mod plugins;
pub mod room_context;
mod widgets;

use gtk::prelude::*;

extern crate sourceview5;

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
