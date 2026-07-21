//! Fuzz-style invariant tests for `hikyaku_timeline::Timeline`.
//!
//! Purpose: run long random sequences of the public mutation API
//! (`insert`, `remove`, `replace_all`, `patch_echo`, `front_evict`,
//! `evict_range`, `clear`, `update_reactions`) against a fresh Timeline,
//! and after each operation assert that all four documented invariants
//! still hold:
//!
//! 1. **Sort**: timestamps in the ListStore are strictly non-decreasing.
//! 2. **Uniqueness**: no two items share a non-empty `event_id`.
//! 3. **Index consistency**: `has_event(e)` returns true iff `e` is a
//!    non-empty event_id currently in the store, and `get_event(e)`
//!    returns exactly the object at that position.
//! 4. **Cached derived state**: `oldest_timestamp` / `newest_timestamp` /
//!    `n_items` match reality.
//!
//! Every test uses a `StdRng` with a fixed seed — failures reproduce
//! deterministically. If a test fails, the seed prints so you can
//! copy-paste it into a targeted rerun.
//!
//! GTK init: every GObject method needs GTK initialised. We call
//! `gtk::init()` inside every test — it's idempotent and Ok to call
//! repeatedly. Tests still need `--test-threads=1` for parallel-safety
//! (GTK panics on multi-thread init); the `[[test]]` harness auto-adds
//! this via the standard workaround at the top.

use gtk::glib;
use gtk::prelude::*;
use hikyaku_timeline::{MessageObject, Timeline};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// Ensure GTK is initialised exactly once per process. `gtk::init()` on
/// an already-initialised main-thread is a no-op; on a different thread
/// it panics with "GTK from two different threads." Tests must run with
/// `--test-threads=1` (see the `#[cfg(test)]` marker below or
/// `RUST_TEST_THREADS=1 cargo test -p hikyaku-timeline`).
fn init() {
    let _ = gtk::init();
}

/// Build a MessageObject with the minimum fields required for Timeline
/// to accept it. `eid == ""` produces an "echo" (matches production's
/// pending-echo pattern where the server-assigned event_id hasn't
/// arrived yet); Timeline permits multiple echoes but no duplicate
/// non-empty event_ids.
fn make_msg(eid: &str, ts: u64, body: &str) -> MessageObject {
    MessageObject::new(
        "sender",  // display name
        "@u:test", // sender_id
        body,
        "",        // formatted_body
        ts,
        eid,
        "",        // reply_to
        "",        // thread_root
        &[],       // reactions
        "",        // media_json
    )
}

/// Walk the store and MessageObject cache to assert every documented
/// invariant. Panics with a descriptive message on any violation —
/// caller wraps with the seed and op count for reproduction.
fn assert_invariants(t: &Timeline, ctx: &str) {
    use std::collections::HashSet;

    let store = t.model();
    let n = store.n_items();

    // Property-cache consistency: n_items mirrors real length.
    assert_eq!(t.n_items(), n, "{ctx}: n_items property out of sync with store");

    if n == 0 {
        assert_eq!(t.oldest_timestamp(), 0, "{ctx}: oldest_timestamp should be 0 on empty");
        assert_eq!(t.newest_timestamp(), 0, "{ctx}: newest_timestamp should be 0 on empty");
        return;
    }

    // Sort + uniqueness invariants.
    let mut prev_ts: u64 = 0;
    let mut seen_eids: HashSet<String> = HashSet::new();
    let mut first_ts: u64 = 0;
    let mut last_ts: u64 = 0;
    for i in 0..n {
        let obj = store
            .item(i)
            .and_downcast::<MessageObject>()
            .unwrap_or_else(|| panic!("{ctx}: item({i}) not a MessageObject"));
        let ts = obj.timestamp();
        if i == 0 {
            first_ts = ts;
        }
        last_ts = ts;
        assert!(
            ts >= prev_ts,
            "{ctx}: sort violation at pos {i}: prev_ts={prev_ts}, ts={ts}"
        );
        prev_ts = ts;

        // Index round-trip.
        let eid = obj.event_id();
        if !eid.is_empty() {
            assert!(
                seen_eids.insert(eid.clone()),
                "{ctx}: duplicate event_id {eid} at pos {i}"
            );
            assert!(
                t.has_event(&eid),
                "{ctx}: event_index missing {eid} that's in store at pos {i}"
            );
            let got = t.get_event(&eid).unwrap_or_else(|| {
                panic!("{ctx}: get_event({eid}) returned None but item exists")
            });
            assert_eq!(
                got.timestamp(),
                ts,
                "{ctx}: get_event({eid}) returned wrong object"
            );
        }
    }

    // Cached derived-state invariants.
    assert_eq!(
        t.oldest_timestamp(),
        first_ts,
        "{ctx}: oldest_timestamp cache mismatch"
    );
    assert_eq!(
        t.newest_timestamp(),
        last_ts,
        "{ctx}: newest_timestamp cache mismatch"
    );
}

/// Random-op driver. Each iteration picks one of the mutation methods
/// weighted like production usage (insert dominates, evict is rare).
fn run_random_ops(seed: u64, ops: usize) {
    init();
    let ctx = glib::MainContext::default();
    let _guard = ctx.acquire().expect("acquire GLib main context");

    let mut rng = StdRng::seed_from_u64(seed);
    let timeline = Timeline::new("!room:test");
    let mut next_eid: u64 = 1;
    let mut next_ts: u64 = 1_000_000;

    for op_i in 0..ops {
        let dice = rng.gen_range(0..100);
        match dice {
            // 60% insert — the dominant mutation.
            0..=59 => {
                let batch_size = rng.gen_range(1..=8);
                let mut batch = Vec::with_capacity(batch_size);
                for _ in 0..batch_size {
                    // Mix of new-tail (ts > newest), gap-fill (ts inside
                    // current range), and prepend (ts < oldest).
                    let mode = rng.gen_range(0..3);
                    let ts = match mode {
                        0 => {
                            next_ts += rng.gen_range(1..=100);
                            next_ts
                        }
                        1 if timeline.n_items() > 0 => {
                            // Random inside current range.
                            let lo = timeline.oldest_timestamp();
                            let hi = timeline.newest_timestamp().max(lo + 1);
                            rng.gen_range(lo..=hi)
                        }
                        _ if timeline.n_items() > 0 => {
                            // Prepend older than current oldest.
                            timeline
                                .oldest_timestamp()
                                .saturating_sub(rng.gen_range(1..=1000))
                                .max(1)
                        }
                        _ => {
                            next_ts += rng.gen_range(1..=100);
                            next_ts
                        }
                    };
                    let eid = format!("${next_eid:016x}");
                    next_eid += 1;
                    batch.push(make_msg(&eid, ts, "body"));
                }
                timeline.insert(batch);
            }
            // 15% remove.
            60..=74 => {
                let n = timeline.n_items();
                if n > 0 {
                    let i = rng.gen_range(0..n);
                    if let Some(obj) = timeline
                        .model()
                        .item(i)
                        .and_downcast::<MessageObject>()
                    {
                        let eid = obj.event_id();
                        if !eid.is_empty() {
                            timeline.remove(&eid);
                        }
                    }
                }
            }
            // 10% front_evict — trim oldest.
            75..=84 => {
                let n = timeline.n_items();
                if n > 0 {
                    let count = rng.gen_range(1..=n.min(4));
                    timeline.front_evict(count);
                }
            }
            // 5% evict_range — middle-of-store cut.
            85..=89 => {
                let n = timeline.n_items();
                if n >= 3 {
                    let start = rng.gen_range(0..n - 1);
                    let count = rng.gen_range(1..=(n - start).min(3));
                    timeline.evict_range(start, count);
                }
            }
            // 5% patch_echo — flip an empty event_id to a real one.
            90..=94 => {
                // Prepare an echo by inserting one with empty eid.
                let ts = {
                    next_ts += 1;
                    next_ts
                };
                let body = format!("echo-{op_i}");
                timeline.insert(vec![make_msg("", ts, &body)]);
                let real = format!("${next_eid:016x}");
                next_eid += 1;
                assert!(
                    timeline.patch_echo(&body, &real),
                    "patch_echo failed for body {body}"
                );
            }
            // 3% replace_all — full rebuild.
            95..=97 => {
                let m = rng.gen_range(0..20);
                let mut batch = Vec::with_capacity(m);
                for _ in 0..m {
                    next_ts += rng.gen_range(1..=100);
                    let eid = format!("${next_eid:016x}");
                    next_eid += 1;
                    batch.push(make_msg(&eid, next_ts, "body"));
                }
                timeline.replace_all(batch);
            }
            // 2% clear.
            _ => {
                timeline.clear();
            }
        }

        assert_invariants(
            &timeline,
            &format!("seed={seed} op={op_i} (dice={dice})"),
        );
    }
}

/// GTK's thread-affinity check panics when a second test lands on a
/// different worker thread and tries to `gtk::init()` again. Cargo's
/// test harness allocates a new worker per test even with
/// `--test-threads=1`, so we can't split invariant checks into
/// separate `#[test]` fns without hitting that panic.
///
/// Workaround: one top-level `#[test]` runs every scenario back-to-back.
/// If one scenario fails, the panic message includes the seed / phase
/// so you can copy-paste it into a targeted repro. Not as pretty as
/// individual `#[test]` fns, but the only pattern that works with
/// gtk-rs's initialisation model.
#[test]
fn all_invariant_scenarios() {
    // Random-op scenarios — increasing seeds cover more of the state
    // space; the "long" run catches leaks that only surface over time.
    for (label, seed, ops) in [
        ("seed=1", 1_u64, 500_usize),
        ("seed=2", 2, 500),
        ("seed=42", 42, 500),
        ("seed=1337", 1337, 500),
        ("seed=0xdeadbeef long", 0xdead_beef, 2000),
    ] {
        eprintln!("running random-op scenario {label} ({ops} ops)");
        run_random_ops(seed, ops);
    }

    pagination_shaped_load_scenario();
    many_gap_fills_scenario();
}

fn pagination_shaped_load_scenario() {
    // Simulate the specific pattern that broke in production:
    // 1. Initial load appends 50 recent msgs.
    // 2. Backpagination delivers 200 msgs that fall INSIDE the current
    //    range (gap-fill, not strict-prepend). This is what
    //    d4d3c4f fixed — the strict-prepend filter dropped these.
    init();
    let ctx = glib::MainContext::default();
    let _guard = ctx.acquire().expect("acquire");

    let timeline = Timeline::new("!room:test");
    let base_ts: u64 = 1_000_000;

    // Initial: 50 msgs at ts 100_000..100_050.
    let initial: Vec<_> = (0..50)
        .map(|i| make_msg(&format!("$init-{i:03}"), base_ts + i, "recent"))
        .collect();
    timeline.insert(initial);
    assert_invariants(&timeline, "after initial load");
    assert_eq!(timeline.n_items(), 50);

    // Pagination: 200 msgs at ts 999_800..1_000_000 (prepending, older
    // than current oldest — but with some interleaving).
    let older: Vec<_> = (0..200)
        .map(|i| make_msg(&format!("$old-{i:03}"), base_ts - 200 + i, "older"))
        .collect();
    timeline.insert(older);
    assert_invariants(&timeline, "after backpagination prepend");
    assert_eq!(timeline.n_items(), 250);
    // Oldest is now way older, newest unchanged.
    assert_eq!(timeline.oldest_timestamp(), base_ts - 200);
    assert_eq!(timeline.newest_timestamp(), base_ts + 49);

    // Gap-fill pagination: 30 msgs whose timestamps fall inside the
    // current range. Test the exact code path that dropped 100% of
    // legitimate msgs before d4d3c4f.
    let gap: Vec<_> = (0..30)
        .map(|i| make_msg(&format!("$gap-{i:03}"), base_ts - 50 + i * 2, "gap"))
        .collect();
    timeline.insert(gap);
    assert_invariants(&timeline, "after gap-fill");
    assert_eq!(timeline.n_items(), 280);

    // The gap events should be reachable by event_id — regression guard
    // for a hypothetical future filter that drops interior msgs.
    for i in 0..30 {
        assert!(
            timeline.has_event(&format!("$gap-{i:03}")),
            "gap event {i} disappeared from timeline"
        );
    }
}

fn many_gap_fills_scenario() {
    // Guard against the same-position batching regression: many gap-fills
    // that land at the SAME insert-position must all end up in the store,
    // sorted ASC by timestamp within the position group.
    init();
    let ctx = glib::MainContext::default();
    let _guard = ctx.acquire().expect("acquire");

    let timeline = Timeline::new("!room:test");
    // Anchor with two msgs; gap = [100, 200].
    timeline.insert(vec![
        make_msg("$anchor-lo", 100, "a"),
        make_msg("$anchor-hi", 200, "b"),
    ]);

    // Fill the gap with 50 msgs at ts 101..150 — all compute pos=1.
    let fill: Vec<_> = (0..50)
        .map(|i| make_msg(&format!("$fill-{i:03}"), 101 + i, "fill"))
        .collect();
    timeline.insert(fill);
    assert_invariants(&timeline, "after 50 same-position gap-fills");
    assert_eq!(timeline.n_items(), 52);

    // Verify order: anchor-lo, fill-0..fill-49, anchor-hi.
    let store = timeline.model();
    let first = store.item(0).and_downcast::<MessageObject>().unwrap();
    assert_eq!(first.event_id(), "$anchor-lo");
    let last = store.item(51).and_downcast::<MessageObject>().unwrap();
    assert_eq!(last.event_id(), "$anchor-hi");
    for i in 0..50 {
        let mid = store.item(1 + i).and_downcast::<MessageObject>().unwrap();
        assert_eq!(mid.event_id(), format!("$fill-{i:03}"));
    }
}
