//! Per-room timeline GObject.
//!
//! All mutations to a room's message list flow through this object. It owns
//! the underlying `gio::ListStore` and the `event_id → MessageObject` index,
//! and enforces these invariants on every mutation:
//!
//! 1. The list is sorted ascending by `timestamp`.
//! 2. No two items share a non-empty `event_id`.
//! 3. `event_index` is in sync with the list — every present non-empty
//!    `event_id` maps to exactly one MessageObject, and that object is
//!    actually in the list.
//! 4. `oldest_timestamp()` / `newest_timestamp()` reflect real list state.
//!
//! No code outside this module may call `list_store.splice()` or mutate
//! `event_index` directly. See `feedback_timeline_chokepoint` in memory for
//! the reasoning — past bugs (out-of-order rows compounding across
//! pagination, vanishing reactions on duplicated echoes) all came from
//! multiple uncoordinated paths each implementing their own sort/dedup
//! logic slightly differently.

use crate::models::MessageObject;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

mod imp {
    use super::*;
    use glib::Properties;
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    #[derive(Properties)]
    #[properties(wrapper_type = super::Timeline)]
    pub struct Timeline {
        /// Matrix room ID (construct-only).
        #[property(get, set, construct_only)]
        pub room_id: RefCell<String>,

        /// True when a backward pagination request is in flight or its
        /// cooldown is still active. UI binds a spinner to this.
        #[property(get, set)]
        pub fetching_older: Cell<bool>,

        /// Cached oldest timestamp (0 if the list is empty). Mirrors
        /// `list_store.item(0).timestamp()` because the list is always
        /// sorted; updated synchronously by every mutation that touches
        /// position 0.
        #[property(get)]
        pub oldest_timestamp: Cell<u64>,

        /// Cached newest timestamp (0 if the list is empty). Mirrors the
        /// last item's timestamp.
        #[property(get)]
        pub newest_timestamp: Cell<u64>,

        /// Pagination token for the next backward fetch. Empty string when
        /// None (GObject string properties can't be Option). Use
        /// `has_prev_batch` to test for presence.
        #[property(get)]
        pub prev_batch_token: RefCell<String>,

        /// Convenience derived flag for binding to "Load more" UI.
        #[property(get)]
        pub has_prev_batch: Cell<bool>,

        /// Live item count. Mirrors `list_store.n_items()`.
        #[property(get)]
        pub n_items: Cell<u32>,

        // ── private state (no direct external access) ───────────────────
        /// The list itself. Returned via `model()` for ListView::set_model
        /// but never exposed for direct mutation.
        pub list_store: gio::ListStore,
        /// event_id → MessageObject. Always in sync with list_store.
        pub event_index: RefCell<HashMap<String, MessageObject>>,
        /// Approximate count of unpatched local echoes (empty event_id).
        /// Used to short-circuit `patch_echo` when there's nothing to find.
        pub pending_echo_count: Cell<u32>,
    }

    impl Default for Timeline {
        fn default() -> Self {
            Self {
                room_id: RefCell::new(String::new()),
                fetching_older: Cell::new(false),
                oldest_timestamp: Cell::new(0),
                newest_timestamp: Cell::new(0),
                prev_batch_token: RefCell::new(String::new()),
                has_prev_batch: Cell::new(false),
                n_items: Cell::new(0),
                list_store: gio::ListStore::new::<MessageObject>(),
                event_index: RefCell::new(HashMap::new()),
                pending_echo_count: Cell::new(0),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Timeline {
        const NAME: &'static str = "HikyakuTimeline";
        type Type = super::Timeline;
    }

    #[glib::derived_properties]
    impl ObjectImpl for Timeline {}
}

glib::wrapper! {
    pub struct Timeline(ObjectSubclass<imp::Timeline>);
}

impl Timeline {
    pub fn new(room_id: &str) -> Self {
        glib::Object::builder().property("room-id", room_id).build()
    }

    /// The underlying ListModel — pass this to `ListView::set_model` (via a
    /// `gtk::NoSelection`). Read-only externally; all mutation goes through
    /// the methods on this object.
    pub fn model(&self) -> &gio::ListStore {
        &self.imp().list_store
    }

    // ── invariant-preserving mutations ──────────────────────────────────

    /// Insert messages into the timeline. Handles every mix: pure-append
    /// (newest live message), pure-prepend (pagination, all-older), echo
    /// patch (empty event_id), and gap-fill (event older than newest but
    /// newer than oldest — landing in the middle).
    ///
    /// Steps performed:
    /// 1. Drop incoming events whose event_id is already in `event_index`.
    /// 2. Sort the survivors by timestamp ascending.
    /// 3. Walk each event, locating the correct sorted position. Coalesce
    ///    runs of consecutive end-appends and consecutive head-prepends
    ///    into single splices; gap-fills go in one-by-one (rare).
    /// 4. Update `event_index` and cached property fields.
    /// 5. Emit `notify::*` for every property that changed.
    pub fn insert(&self, objs: Vec<MessageObject>) {
        if objs.is_empty() {
            return;
        }
        let _g = crate::perf::scope_gt("Timeline::insert", 500);
        let imp = self.imp();
        let store = &imp.list_store;

        // Dedup against event_index.
        let mut filtered: Vec<MessageObject> = {
            let idx = imp.event_index.borrow();
            objs.into_iter()
                .filter(|o| {
                    let eid = o.event_id();
                    eid.is_empty() || !idx.contains_key(&eid)
                })
                .collect()
        };
        if filtered.is_empty() {
            return;
        }

        // Sort ascending by timestamp. Stable sort: equal timestamps keep
        // the input order, which keeps replies adjacent to their roots in
        // the common case.
        filtered.sort_by(|a, b| a.timestamp().cmp(&b.timestamp()));

        // Determine the current range — empty store is the easy path.
        let mut n = gio::prelude::ListModelExt::n_items(store);
        let cur_oldest = imp.oldest_timestamp.get();
        let cur_newest = imp.newest_timestamp.get();

        let mut added_echoes: u32 = 0;
        let mut idx_updates: Vec<(String, MessageObject)> = Vec::new();

        if n == 0 {
            // Empty store: one bulk splice is correct.
            for obj in &filtered {
                let eid = obj.event_id();
                if eid.is_empty() {
                    added_echoes += 1;
                } else {
                    idx_updates.push((eid, obj.clone()));
                }
            }
            store.splice(0, 0, &filtered);
            n = filtered.len() as u32;
        } else {
            // Three buckets, processed in order so position bookkeeping
            // stays simple. Echoes (empty event_id) always go to the end:
            // they're local sends about to be ack'd, and the user has just
            // typed them now, so "now" is their effective timestamp.
            let mut prepend_batch: Vec<MessageObject> = Vec::new();
            let mut append_batch: Vec<MessageObject> = Vec::new();
            let mut gap_fills: Vec<MessageObject> = Vec::new();
            for obj in filtered.into_iter() {
                let eid = obj.event_id();
                if eid.is_empty() {
                    added_echoes += 1;
                    append_batch.push(obj);
                    continue;
                }
                let ts = obj.timestamp();
                if ts < cur_oldest || cur_oldest == 0 {
                    prepend_batch.push(obj);
                } else if ts >= cur_newest {
                    append_batch.push(obj);
                } else {
                    gap_fills.push(obj);
                }
            }

            // Head: bulk splice, single items_changed.
            if !prepend_batch.is_empty() {
                for obj in &prepend_batch {
                    let eid = obj.event_id();
                    if !eid.is_empty() {
                        idx_updates.push((eid, obj.clone()));
                    }
                }
                store.splice(0, 0, &prepend_batch);
                n += prepend_batch.len() as u32;
            }

            // Tail: bulk splice at end.
            if !append_batch.is_empty() {
                for obj in &append_batch {
                    let eid = obj.event_id();
                    if !eid.is_empty() {
                        idx_updates.push((eid, obj.clone()));
                    }
                }
                store.splice(n, 0, &append_batch);
                n += append_batch.len() as u32;
            }

            // Gap-fills: one splice each. Order by (position DESC, timestamp DESC).
            //
            // Position DESC so earlier-position splices don't have their
            // positions invalidated by later ones (splice at pos P shifts
            // items pos>=P to pos+1).
            //
            // TIMESTAMP DESC WITHIN THE SAME POSITION is critical: multiple
            // gap-fills can compute the same sorted_insert_pos (both want to
            // slot into the same gap in the store). Processing lower-ts first
            // would splice it at pos P; the higher-ts then splices at the
            // same pos P and pushes the lower-ts to P+1 — swapping their
            // order and violating the sort invariant.
            //
            // Repro that used to fail: insert [150, 175] into [100, 200,
            // 300, 400]. Both compute pos=1. Old code processed 150 first
            // (stable sort tie), giving [100, 175, 150, 200, 300, 400] —
            // 175 before 150. Processing 175 first gives [100, 175, ...]
            // then 150 splice at pos 1 gives [100, 150, 175, ...]. Sorted.
            //
            // This bug caused historical-timeline pagination to produce
            // scrambled order (today, may 20, yesterday, may 24, ...) once
            // multiple new msgs landed in the same gap.
            if !gap_fills.is_empty() {
                // Compute each gap-fill's insert position against the ORIGINAL
                // store (pre-splice). Group by position — items that all want
                // to slot into the same gap share a splice.
                //
                // Why this matters: one splice per gap-fill fires one
                // items_changed. During pagination that delivers 200
                // interior msgs, 200 individual splices drive GtkListView
                // to recycle/remeasure every visible row 200 times — the
                // row-oscillation loop observed as bind counts of 50+ per
                // msg. Batching same-position gap-fills into a single splice
                // collapses that to one items_changed per unique position.
                //
                // Ordering:
                //   - Process position groups from HIGHEST to LOWEST so
                //     earlier splices don't shift later positions.
                //   - Within a group, sort ASC by timestamp so the batch is
                //     already chronologically ordered when spliced in place
                //     (splice inserts the slice contiguously; ASC sort within
                //     the batch preserves the Timeline's overall ASC-by-ts
                //     invariant).
                let mut by_pos: std::collections::BTreeMap<u32, Vec<MessageObject>> =
                    std::collections::BTreeMap::new();
                for obj in gap_fills.into_iter() {
                    let pos = sorted_insert_pos(store, obj.timestamp());
                    by_pos.entry(pos).or_default().push(obj);
                }
                // BTreeMap iterates ASC; iterate reverse to get DESC.
                for (pos, mut group) in by_pos.into_iter().rev() {
                    group.sort_by(|a, b| a.timestamp().cmp(&b.timestamp()));
                    for obj in &group {
                        let eid = obj.event_id();
                        if !eid.is_empty() {
                            idx_updates.push((eid, obj.clone()));
                        }
                    }
                    let added = group.len() as u32;
                    store.splice(pos, 0, &group);
                    n += added;
                }
            }
        }

        {
            let mut idx = imp.event_index.borrow_mut();
            for (eid, obj) in idx_updates {
                idx.insert(eid, obj);
            }
        }
        imp.pending_echo_count
            .set(imp.pending_echo_count.get().saturating_add(added_echoes));

        self.refresh_derived_state(n);
    }

    /// Replace the entire timeline with a new set of messages — used when
    /// switching rooms or recovering from a corrupted cache.
    pub fn replace_all(&self, objs: Vec<MessageObject>) {
        let imp = self.imp();
        let store = &imp.list_store;
        let n_old = gio::prelude::ListModelExt::n_items(store);
        // Sort + dedup the input BEFORE the splice so we can do the whole
        // replace in one items_changed signal. Two-splice (clear then insert)
        // caused an extra ListView measurement pass on room-switch.
        let mut filtered: Vec<MessageObject> = {
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            objs.into_iter()
                .filter(|o| {
                    let eid = o.event_id();
                    eid.is_empty() || seen.insert(eid)
                })
                .collect()
        };
        filtered.sort_by(|a, b| a.timestamp().cmp(&b.timestamp()));

        // Rebuild event_index in one shot.
        let mut new_idx: std::collections::HashMap<String, MessageObject> =
            std::collections::HashMap::with_capacity(filtered.len());
        let mut echo_count: u32 = 0;
        for obj in &filtered {
            let eid = obj.event_id();
            if eid.is_empty() {
                echo_count = echo_count.saturating_add(1);
            } else {
                new_idx.insert(eid, obj.clone());
            }
        }

        // Single splice: remove all old, insert all new. One items_changed.
        store.splice(0, n_old, &filtered);
        *imp.event_index.borrow_mut() = new_idx;
        imp.pending_echo_count.set(echo_count);
        let new_n = filtered.len() as u32;
        self.refresh_derived_state(new_n);
    }

    /// Update the body (and optional formatted body) of an existing message
    /// in place. Returns true if the event was found.
    pub fn update_body(&self, event_id: &str, body: &str, formatted: Option<&str>) -> bool {
        if event_id.is_empty() {
            return false;
        }
        let imp = self.imp();
        let Some(msg) = imp.event_index.borrow().get(event_id).cloned() else {
            return false;
        };
        msg.set_body(body.to_string());
        if let Some(f) = formatted {
            msg.set_formatted_body(f.to_string());
        }
        true
    }

    /// Overwrite the reactions JSON for an existing message. Returns true
    /// if the event was found.
    pub fn update_reactions(&self, event_id: &str, reactions_json: &str) -> bool {
        if event_id.is_empty() {
            return false;
        }
        let imp = self.imp();
        let Some(msg) = imp.event_index.borrow().get(event_id).cloned() else {
            return false;
        };
        msg.update_reactions_json(reactions_json.to_string());
        true
    }

    /// Patch a local echo's empty event_id with the server-assigned id.
    /// Searches the tail of the list (echoes are appended, so they're
    /// always near the end) for a row with empty `event_id` and matching
    /// `body`. Returns true when found and patched.
    pub fn patch_echo(&self, echo_body: &str, real_event_id: &str) -> bool {
        if real_event_id.is_empty() {
            return false;
        }
        let imp = self.imp();
        if imp.pending_echo_count.get() == 0 {
            // No outstanding echoes — short-circuit.
            return false;
        }
        let store = &imp.list_store;
        let n = gio::prelude::ListModelExt::n_items(store);
        for i in (0..n).rev() {
            let Some(msg) = store.item(i).and_downcast::<MessageObject>() else {
                continue;
            };
            if !msg.event_id().is_empty() {
                continue;
            }
            if msg.body() == echo_body {
                msg.set_event_id(real_event_id.to_string());
                imp.event_index
                    .borrow_mut()
                    .insert(real_event_id.to_string(), msg);
                imp.pending_echo_count
                    .set(imp.pending_echo_count.get().saturating_sub(1));
                return true;
            }
        }
        false
    }

    /// Remove a message by event_id (redaction). Returns true if found.
    pub fn remove(&self, event_id: &str) -> bool {
        if event_id.is_empty() {
            return false;
        }
        let imp = self.imp();
        let Some(_msg) = imp.event_index.borrow_mut().remove(event_id) else {
            return false;
        };
        let store = &imp.list_store;
        let n = gio::prelude::ListModelExt::n_items(store);
        for i in 0..n {
            let Some(o) = store.item(i).and_downcast::<MessageObject>() else {
                continue;
            };
            if o.event_id() == event_id {
                store.splice(i, 1, &[] as &[MessageObject]);
                self.refresh_derived_state(n - 1);
                return true;
            }
        }
        false
    }

    /// Remove the first `count` items from the front. Used by the incoming-
    /// message cap enforcement in MessageView — when we've grown past
    /// MAX_STORE_SIZE and need to trim the oldest tail so the ListView
    /// doesn't unbounded-grow. Returns the number of items actually removed
    /// (may be less than requested if the store is smaller). Updates
    /// event_index and cached derived state.
    pub fn front_evict(&self, count: u32) -> u32 {
        if count == 0 {
            return 0;
        }
        let imp = self.imp();
        let store = &imp.list_store;
        let n = gio::prelude::ListModelExt::n_items(store);
        let actual = count.min(n);
        if actual == 0 {
            return 0;
        }
        // Collect eids to drop from the index before the splice moves items.
        let mut drop_eids: Vec<String> = Vec::with_capacity(actual as usize);
        for i in 0..actual {
            if let Some(msg) = store.item(i).and_downcast::<MessageObject>() {
                let eid = msg.event_id();
                if !eid.is_empty() {
                    drop_eids.push(eid);
                }
            }
        }
        store.splice(0, actual, &[] as &[MessageObject]);
        {
            let mut idx = imp.event_index.borrow_mut();
            for eid in drop_eids {
                idx.remove(&eid);
            }
        }
        self.refresh_derived_state(n - actual);
        actual
    }

    /// Remove `count` consecutive items starting at `start`. Single splice
    /// (one items_changed signal) — the bulk counterpart of `front_evict`
    /// for tail / mid-list eviction. Used by `prepend_messages` to trim
    /// the newest tail when a pagination pushes the store past its cap.
    /// Returns the number of items actually removed (clamped to the store's
    /// current length).
    pub fn evict_range(&self, start: u32, count: u32) -> u32 {
        if count == 0 {
            return 0;
        }
        let imp = self.imp();
        let store = &imp.list_store;
        let n = gio::prelude::ListModelExt::n_items(store);
        if start >= n {
            return 0;
        }
        let actual = count.min(n - start);
        if actual == 0 {
            return 0;
        }
        let mut drop_eids: Vec<String> = Vec::with_capacity(actual as usize);
        for i in start..start + actual {
            if let Some(msg) = store.item(i).and_downcast::<MessageObject>() {
                let eid = msg.event_id();
                if !eid.is_empty() {
                    drop_eids.push(eid);
                }
            }
        }
        store.splice(start, actual, &[] as &[MessageObject]);
        {
            let mut idx = imp.event_index.borrow_mut();
            for eid in drop_eids {
                idx.remove(&eid);
            }
        }
        self.refresh_derived_state(n - actual);
        actual
    }

    /// Drop all messages and clear the index. Used on room cache reset.
    pub fn clear(&self) {
        let imp = self.imp();
        let n = gio::prelude::ListModelExt::n_items(&imp.list_store);
        imp.list_store.splice(0, n, &[] as &[MessageObject]);
        imp.event_index.borrow_mut().clear();
        imp.pending_echo_count.set(0);
        self.refresh_derived_state(0);
    }

    /// Set the prev-batch token (used by the matrix client after a fetch).
    /// Empty/None clears it. Updates the derived `has-prev-batch` flag and
    /// fires notify so any bound UI updates.
    pub fn set_prev_batch_token(&self, token: Option<String>) {
        let imp = self.imp();
        let new_val = token.unwrap_or_default();
        let prev = imp.prev_batch_token.borrow().clone();
        if prev == new_val {
            return;
        }
        *imp.prev_batch_token.borrow_mut() = new_val.clone();
        let prev_has = imp.has_prev_batch.get();
        let new_has = !new_val.is_empty();
        imp.has_prev_batch.set(new_has);
        self.notify_prev_batch_token();
        if prev_has != new_has {
            self.notify_has_prev_batch();
        }
    }

    // ── queries (no mutation, no invariant change) ──────────────────────

    pub fn has_event(&self, event_id: &str) -> bool {
        if event_id.is_empty() {
            return false;
        }
        self.imp().event_index.borrow().contains_key(event_id)
    }

    pub fn get_event(&self, event_id: &str) -> Option<MessageObject> {
        if event_id.is_empty() {
            return None;
        }
        self.imp().event_index.borrow().get(event_id).cloned()
    }

    // ── internal helpers ────────────────────────────────────────────────

    fn refresh_derived_state(&self, new_n: u32) {
        let imp = self.imp();
        let store = &imp.list_store;
        let prev_n = imp.n_items.get();
        imp.n_items.set(new_n);
        if prev_n != new_n {
            self.notify_n_items();
        }
        let prev_oldest = imp.oldest_timestamp.get();
        let prev_newest = imp.newest_timestamp.get();
        let (new_oldest, new_newest) = if new_n == 0 {
            (0, 0)
        } else {
            let oldest = store
                .item(0)
                .and_downcast::<MessageObject>()
                .map(|o| o.timestamp())
                .unwrap_or(0);
            let newest = store
                .item(new_n - 1)
                .and_downcast::<MessageObject>()
                .map(|o| o.timestamp())
                .unwrap_or(0);
            (oldest, newest)
        };
        imp.oldest_timestamp.set(new_oldest);
        imp.newest_timestamp.set(new_newest);
        if prev_oldest != new_oldest {
            self.notify_oldest_timestamp();
        }
        if prev_newest != new_newest {
            self.notify_newest_timestamp();
        }

        // Invariant check: walk the store and error-log any place where
        // ts decreases (sort violation). This is O(n) per mutation but
        // Sort invariant check — WARN level so it fires with RUST_LOG=info
        // (which is what `just debug-render` uses). O(n) per mutation but
        // only VIOLATIONS log; the walk itself is silent on healthy stores.
        if new_n > 1 {
            let mut prev_ts: u64 = 0;
            let mut violations: u32 = 0;
            let mut first_violation_pos: Option<u32> = None;
            let mut first_violation_ts: (u64, u64) = (0, 0);
            for i in 0..new_n {
                if let Some(msg) = store.item(i).and_downcast::<MessageObject>() {
                    let ts = msg.timestamp();
                    if i > 0 && ts < prev_ts {
                        violations += 1;
                        if first_violation_pos.is_none() {
                            first_violation_pos = Some(i);
                            first_violation_ts = (prev_ts, ts);
                        }
                    }
                    prev_ts = ts;
                }
            }
            if violations > 0 {
                tracing::warn!(
                    "timeline-invariant: {} sort violations in store (n={}), first at pos={:?} prev_ts={} < this_ts={} broken",
                    violations, new_n, first_violation_pos,
                    first_violation_ts.0, first_violation_ts.1
                );
            }
        }
    }
}

/// Binary search for the insertion position that keeps the store sorted
/// ascending by timestamp. Returns the count of items whose timestamp is
/// `<=` the target, i.e. the index where the new item should be inserted.
fn sorted_insert_pos(store: &gtk::gio::ListStore, ts: u64) -> u32 {
    let n = gio::prelude::ListModelExt::n_items(store);
    let mut lo = 0u32;
    let mut hi = n;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let mid_ts = store
            .item(mid)
            .and_downcast::<MessageObject>()
            .map(|o| o.timestamp())
            .unwrap_or(0);
        if mid_ts <= ts {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::MessageObject;

    fn init_gtk() {
        let _ = gtk::init();
    }

    fn msg(eid: &str, body: &str, ts: u64) -> MessageObject {
        MessageObject::new(
            "Alice", "@alice:test", body, "", ts, eid, "", "", &[], "",
        )
    }

    fn timestamps(t: &Timeline) -> Vec<u64> {
        let n = gtk::prelude::ListModelExt::n_items(t.model());
        (0..n)
            .filter_map(|i| t.model().item(i).and_downcast::<MessageObject>())
            .map(|o| o.timestamp())
            .collect()
    }

    #[test]
    fn insert_preserves_sort_for_mixed_input() {
        init_gtk();
        let t = Timeline::new("!room:test");
        t.insert(vec![
            msg("$a", "first", 100),
            msg("$b", "second", 50),
            msg("$c", "third", 200),
            msg("$d", "fourth", 150),
        ]);
        assert_eq!(timestamps(&t), vec![50, 100, 150, 200]);
        assert_eq!(t.n_items(), 4);
        assert_eq!(t.oldest_timestamp(), 50);
        assert_eq!(t.newest_timestamp(), 200);
    }

    #[test]
    fn second_insert_with_gap_fill_lands_in_middle() {
        init_gtk();
        let t = Timeline::new("!room:test");
        t.insert(vec![msg("$a", "", 100), msg("$b", "", 200)]);
        // Gap-fill at ts=150 should land between them.
        t.insert(vec![msg("$c", "", 150)]);
        assert_eq!(timestamps(&t), vec![100, 150, 200]);
    }

    #[test]
    fn insert_dedupes_by_event_id() {
        init_gtk();
        let t = Timeline::new("!room:test");
        t.insert(vec![msg("$a", "first", 100)]);
        t.insert(vec![msg("$a", "duplicate", 999)]);
        assert_eq!(t.n_items(), 1);
        // Original kept; the would-be-newer copy at ts=999 is dropped.
        assert_eq!(timestamps(&t), vec![100]);
    }

    #[test]
    fn patch_echo_finds_empty_event_id() {
        init_gtk();
        let t = Timeline::new("!room:test");
        t.insert(vec![msg("$a", "first", 100)]);
        t.insert(vec![msg("", "my echo", 150)]); // local echo
        assert!(t.patch_echo("my echo", "$real"));
        assert!(t.has_event("$real"));
        let real = t.get_event("$real").unwrap();
        assert_eq!(real.body(), "my echo");
    }

    #[test]
    fn patch_echo_with_no_pending_short_circuits() {
        init_gtk();
        let t = Timeline::new("!room:test");
        t.insert(vec![msg("$a", "first", 100)]);
        // No echo in the list — patch must return false.
        assert!(!t.patch_echo("nonexistent", "$real"));
    }

    #[test]
    fn remove_drops_event_from_list_and_index() {
        init_gtk();
        let t = Timeline::new("!room:test");
        t.insert(vec![msg("$a", "", 100), msg("$b", "", 200)]);
        assert!(t.remove("$a"));
        assert_eq!(t.n_items(), 1);
        assert!(!t.has_event("$a"));
        assert!(t.has_event("$b"));
        assert_eq!(t.oldest_timestamp(), 200);
        assert_eq!(t.newest_timestamp(), 200);
    }

    #[test]
    fn replace_all_resets_state() {
        init_gtk();
        let t = Timeline::new("!room:test");
        t.insert(vec![msg("$a", "", 100)]);
        t.replace_all(vec![msg("$x", "", 500), msg("$y", "", 600)]);
        assert_eq!(t.n_items(), 2);
        assert!(!t.has_event("$a"));
        assert!(t.has_event("$x"));
        assert_eq!(timestamps(&t), vec![500, 600]);
    }

    #[test]
    fn gap_fills_at_same_position_preserve_sort() {
        // Regression: two gap-fills whose sorted_insert_pos computed the
        // same slot used to end up reversed because the second splice at
        // the same pos would push the first one down. Repro'd as scrambled
        // history ordering after backpagination.
        init_gtk();
        let t = Timeline::new("!room:test");
        t.insert(vec![
            msg("$a", "", 100),
            msg("$b", "", 200),
            msg("$c", "", 300),
            msg("$d", "", 400),
        ]);
        // Both slot between 100 and 200 (pos=1 initially).
        t.insert(vec![msg("$x", "", 150), msg("$y", "", 175)]);
        assert_eq!(timestamps(&t), vec![100, 150, 175, 200, 300, 400]);
    }

    #[test]
    fn many_gap_fills_at_same_position_preserve_sort() {
        // Same shape at scale — a burst of pagination msgs all landing in
        // the same slot.
        init_gtk();
        let t = Timeline::new("!room:test");
        t.insert(vec![msg("$a", "", 100), msg("$b", "", 1000)]);
        t.insert(vec![
            msg("$1", "", 500),
            msg("$2", "", 200),
            msg("$3", "", 800),
            msg("$4", "", 300),
            msg("$5", "", 700),
        ]);
        assert_eq!(timestamps(&t), vec![100, 200, 300, 500, 700, 800, 1000]);
    }

    #[test]
    fn evict_range_removes_middle_slice_in_single_splice() {
        init_gtk();
        let t = Timeline::new("!room:test");
        t.insert(vec![
            msg("$a", "", 100),
            msg("$b", "", 200),
            msg("$c", "", 300),
            msg("$d", "", 400),
            msg("$e", "", 500),
        ]);
        // Fire an items_changed counter to prove it's a single signal.
        let count = std::rc::Rc::new(std::cell::Cell::new(0u32));
        {
            let c = count.clone();
            t.model().connect_items_changed(move |_, _, _, _| c.set(c.get() + 1));
        }
        // Evict $c and $d (positions 2 and 3) in one call.
        let removed = t.evict_range(2, 2);
        assert_eq!(removed, 2);
        assert_eq!(count.get(), 1, "evict_range must fire exactly one items_changed");
        assert_eq!(t.n_items(), 3);
        assert!(!t.has_event("$c"));
        assert!(!t.has_event("$d"));
        assert!(t.has_event("$a"));
        assert!(t.has_event("$e"));
        assert_eq!(t.newest_timestamp(), 500);
        assert_eq!(t.oldest_timestamp(), 100);
    }

    #[test]
    fn evict_range_clamps_to_store_length() {
        init_gtk();
        let t = Timeline::new("!room:test");
        t.insert(vec![msg("$a", "", 100), msg("$b", "", 200)]);
        assert_eq!(t.evict_range(1, 999), 1);
        assert_eq!(t.n_items(), 1);
        assert_eq!(t.evict_range(50, 1), 0);
    }

    #[test]
    fn prev_batch_token_notifies() {
        init_gtk();
        let t = Timeline::new("!room:test");
        assert_eq!(t.prev_batch_token(), "");
        assert!(!t.has_prev_batch());
        t.set_prev_batch_token(Some("tok1".into()));
        assert_eq!(t.prev_batch_token(), "tok1");
        assert!(t.has_prev_batch());
        t.set_prev_batch_token(None);
        assert!(!t.has_prev_batch());
    }
}
