# Hikyaku — Development Guide

## Project Overview

Hikyaku is a GNOME Matrix chat client written in Rust + GTK4/libadwaita. It uses the matrix-rust-sdk for Matrix protocol support and GObject/Blueprint for the widget layer.

---

## GTK4 / Rust Performance Rules

These were learned from profiling real bugs with sysprof. Violating them causes visible UI lag.

### 1. Never block the GTK main thread

Any blocking call in a GTK signal handler (click, selection-changed, notify) freezes the UI for its entire duration. The threshold is **<1ms**; anything longer is perceptible.

**Confirmed expensive operations on the GTK thread:**
- `std::fs::read` / any synchronous file I/O
- SQLite writes with `PRAGMA synchronous=FULL` (default) — triggers `fdatasync` via `sqlite3VdbeHalt → unixSync`
- Constructing `gtk::Label` with `selectable: true` (~10–20ms each — backed by an internal GtkTextView)
- `gio::ListStore::splice(0, n, &objs)` replacing n items — **300–1000ms regardless of what your bind callback does** (see GtkListView section below)

**Fixes:**
- Defer non-urgent work to `glib::idle_add_local_once(|| ...)` so it runs after the current frame renders
- Use `PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;` for any SQLite DB written from the GTK thread — eliminates per-write fdatasync, fsync only at checkpoint
- Pre-allocate heavy widgets in the template (Blueprint); update their content in-place during bind instead of creating new widgets

### 2. GtkListView full-replace splice is inherently slow for variable-height rows

**Confirmed via profiling**: `list_store.splice(0, n_old, &new_items)` replacing all items takes **300–1000ms on the GTK main thread**, even when your `connect_bind` callback completes in under 2ms. The cost is inside GTK's `items_changed` processing — item position tracking, height estimation for the scrollbar, and potential measurement of rows.

This is a GTK4 `GtkListView` infrastructure cost that cannot be avoided with the full-replace splice pattern.

**What we measured:**
- `info_to_obj` (Rust): 0.4–4ms (trivial)
- `bind_message_object` callbacks: <2ms each, ~30ms total per room switch
- `list_store.splice(0, n, &objs)`: 300–1000ms — **all GTK infrastructure overhead**

**Current architecture (implemented in `src/widgets/message_view.rs`):**

The message view uses **per-room ListStore + GtkStack** so room switches don't splice:

- `list_store_cache: HashMap<RoomId, gio::ListStore>` — one store per room, cached for the session
- `room_view_cache: HashMap<RoomId, (ScrolledWindow, ListView)>` — one per-room ListView, created once
- All per-room ScrolledWindows live in `room_view_stack: gtk::Stack`
- Room switch = `room_view_stack.set_visible_child_name(room_id)` — O(1), no splice, no rebind
- **First visit** to a room does one unavoidable `splice(0, n, &objs)` in `set_messages` — detaches the model before the splice (`set_model(None)`) and re-attaches in `idle_add_local_once` so it doesn't block the frame
- **Incremental updates** (new messages, echoes): end-appends are batched into a single `splice(orig_n, 0, &batch)`; gap-fills (older timestamps) splice high-to-low. Never full-replace after first_load
- **Property updates** (edits, reactions, UTD heals): mutate GObject properties in place via `update_message_body` — no splice at all

Profile evidence (2026-04-21, 287s capture, 468k samples): splice totals ~0.41% of process time, concentrated in first-visit + pagination paths. Room switching itself is effectively free.

**Rules that follow from this architecture:**

- Never introduce a new `splice(0, n, ...)` full-replace — use incremental append, gap-fill, or in-place property mutation
- Never create widgets during `set_messages`/bind — widgets live in the Stack and are reused
- If a new code path needs to replace a room's content (e.g., space switch, clearing encrypted messages), reuse the existing first-load detach/re-attach pattern; don't invent a parallel one

**Do not try to fix splice cost by optimizing bind callbacks** — we confirmed bind is <10% of splice time. The bottleneck is GTK's `items_changed` infrastructure.

### 3. GtkListView factory: pre-allocate widgets in the template

The `SignalListItemFactory` calls `connect_setup` once per row widget (pool allocation) and `connect_bind` every time a row is recycled for a new item. **Do not construct expensive widgets in `connect_bind`.**

```
setup  → create the row widget tree (once per pool slot)
bind   → update widget content for the current item (called on every room switch)
unbind → disconnect signal handlers from the old item
```

**Pattern: pre-allocated body label**

For a message list, put the body label in the Blueprint template as `visible: false`:

```blueprint
Gtk.Label body_label {
  selectable: true;
  wrap: true;
  wrap-mode: word_char;
  xalign: 0;
  hexpand: true;
  visible: false;
  styles ["mx-message-body"]
}
Gtk.Box body_box {
  orientation: vertical;
  visible: false;   // used only for HTML/code-block messages
}
```

In bind, for plain text: call `body_label.set_markup(markup)` and `body_label.set_visible(true)`. Never call `gtk::Label::builder()...build()` inside bind — constructing a selectable label costs 10–20ms each, which multiplies across all visible rows on every room switch.

Connect one-shot signal handlers (like `connect_activate_link`) in `ObjectImpl::constructed()`, not in bind. In `mod imp {}`, reference parent-module functions as `super::fn_name()`.

### 3. GObject property notifications, not items_changed/splice

Use `connect_notify_local` for reactive UI updates. Avoid `ListStore::splice` for in-place item updates.

```rust
// Good — reactive update, no list churn
msg_obj.connect_notify_local(Some("is-new-message"), |obj, _| { ... });

// Bad — replaces all items, triggers full rebind of visible rows
list_store.splice(0, n, &all_new_objs);
```

`splice` is acceptable when the room changes entirely (different message set). It is not acceptable for updating properties of existing messages (edits, reactions, read status).

### 4. All mutable UI state in GObject properties

Use GObject properties (with `#[property]` via `glib::Properties`) for any state that drives widget appearance. Never store UI state in a side-channel `Rc<RefCell<...>>` that widgets read directly. The GObject property system is the reactive layer — properties fire `notify` signals automatically, which is what `connect_notify_local` listens for.

### 5. Background work: dedicated thread + bounded channel

For CPU-heavy background work (ML inference, image processing, etc.), use a single dedicated `std::thread` with a bounded `std::sync::mpsc::sync_channel`. Use `try_send` (drops if full) to apply backpressure without blocking the caller.

```rust
// Good: one scorer thread, bounded queue, try_send drops under load
let (tx, rx) = std::sync::mpsc::sync_channel::<WorkItem>(32);
std::thread::Builder::new()
    .name("scorer".into())
    .spawn(move || {
        let model = Model::new();          // expensive init, happens once
        while let Ok(item) = rx.recv() {   // blocks thread, not GTK
            let result = model.score(&item);
            event_tx.send_blocking(MatrixEvent::Result(result)).ok();
        }
    }).ok();

// Caller: fire-and-forget, never blocks GTK thread
let _ = tx.try_send(work_item);

// Bad: spawns unlimited OS threads, starves GTK for CPU
tokio::task::spawn_blocking(|| { Model::new(); ... });  // called per message
```

### 6. Lookups: HashMaps, not linear scans

Any O(n) scan called from a GTK signal handler is a latency bomb as rooms grow.

```rust
// Good
let obj = imp.event_index.borrow().get(&event_id).cloned();

// Bad — O(n) on every room click
let obj = imp.list_store.iter::<MessageObject>()
    .find(|o| o.event_id() == event_id);
```

---

## Profiling with sysprof

Build with frame pointers (required for callgraph unwinding):

```toml
# .cargo/config.toml
[build]
rustflags = ["-Cforce-frame-pointers=yes", "-Csymbol-mangling-version=v0"]
```

Capture a profile:

```sh
sysprof-cli --gtk --speedtrack hikyaku.syscap -- ./target/debug/hikyaku
# interact with the app, then Ctrl-C
sysprof-cat hikyaku.syscap > output.txt
```

`sysprof-cat` output is large and takes time to appear. Search for your function names in `output.txt` to find hot call chains. GTK symbols appear unresolved without debug packages — focus on your own Rust frames.

**Redact before sharing.** `sysprof` captures the process environment including `ANTHROPIC_API_KEY` and similar secrets. Scrub before attaching to issues or pastes:

```sh
sed -i "s/ANTHROPIC_API_KEY=sk-ant-[A-Za-z0-9_-]*/ANTHROPIC_API_KEY=REDACTED/g" output.txt
```

## Rust-side scoped timing

`src/perf.rs` provides a scope-guard timer that logs to `tracing` on drop if the elapsed time exceeds a threshold. Every line is tagged `perf=<name>` so a single grep pulls the full heat map out of a log.

```rust
pub fn bind_message_object(&self, ...) {
    let _g = crate::perf::scope("bind_message_object");   // default threshold 500µs
    let _g = crate::perf::scope_gt("info_to_obj", 200);   // custom threshold
    let _g = crate::perf::scope_with("splice", room_id);  // add a context tag
    // ...work...
}
```

Run with `RUST_LOG=hikyaku=info` to see the logs. Capture and inspect:

```sh
RUST_LOG=hikyaku=info ./target/debug/hikyaku 2>&1 | tee ~/hikyaku.log
grep 'perf=' ~/hikyaku.log | sort | uniq -c | sort -rn | head -30
```

Thresholds are chosen to suppress noise; set to `0` temporarily to force every scope to log. Instrument new functions when a flow fails to explain a user-visible pause — if a 6-second pause has zero `perf=` lines inside it, the slow function is still uninstrumented.

## GSK renderer choice

`main.rs` pins `GSK_RENDERER=vulkan` when the user hasn't set it. The default selection (observed as `ngl` on Fedora + Mesa Intel) reported `Unsupported node 'GskTransformNode' / Offscreening node ...` for `GskTransformNode`, `GskMaskNode`, and `GskContainerNode` — every frame fell back to per-node offscreen texture compositing, which capped the 120 Hz display at ~60 Hz effective and produced 125 ms stalls. Vulkan handles those nodes natively. Validate with `GDK_DEBUG=frames` — the `Unsupported node` spam should be gone. User override via `GSK_RENDERER=...` in the environment always wins.

## GTK-side instrumentation (the other half of the stack)

Rust `perf=` timing covers everything above the GTK FFI boundary. For layout / render / paint pauses, use GTK's built-in debug flags:

- `GTK_DEBUG=layout` — logs every allocation / reposition. Heavy output but pinpoints layout storms.
- `GTK_DEBUG=tree` — widget tree dump; useful after a memory-bloat suspicion to see how many widgets are alive.
- `GTK_DEBUG=interactive` — opens the GtkInspector. Its *Recorder* tab captures GskRenderNode trees per frame; its *Statistics* tab shows widget counts and CSS timings.
- `GSK_DEBUG=full-redraw` — flashes the full window red whenever the renderer decides to repaint everything (not just damage). Expected on focus-in after long idle; unexpected anywhere else.
- `GSK_DEBUG=fallback` — logs every time GSK falls back from GPU to CPU rendering. Indicates a driver / node compatibility problem eating frames.
- `GDK_DEBUG=frames` — per-frame timing from GDK, including compositor round-trip latency.

Combine with the Rust logs to correlate: a 6-second pause with zero `perf=` lines AND a `GSK_DEBUG=full-redraw` flash means the pause is in GTK's render path, not our code.

```sh
GTK_DEBUG=interactive GSK_DEBUG=full-redraw,fallback \
  GDK_DEBUG=frames RUST_LOG=hikyaku=info \
  ./target/debug/hikyaku 2>&1 | tee ~/hikyaku-full.log
```

## Coverage-map discipline

Every new hot path should be instrumented at introduction. When a regression shows a pause that no `perf=` line can account for, the gap is the bug — find the uninstrumented function on the path and wrap it before hypothesising. "The logs say it's fast" is only meaningful for paths that are actually instrumented.

## Other profilers — when to reach for them

sysprof + `perf=` logs cover most CPU questions. Reach for these when they don't:

| Tool | Answers | When to use |
|------|---------|-------------|
| `cargo install cargo-valgrind` → `cargo valgrind run` | Exact CPU cycles, cache misses, uninitialised reads | A reproducible hot loop where sysprof samples aren't granular enough. Valgrind is ~20× slower so reproduce on a small case. |
| `valgrind --tool=callgrind` + `kcachegrind` | Call graph with exact inclusive/exclusive counts | Same as cargo-valgrind but with a visual call-graph explorer. |
| `valgrind --tool=massif` | Peak heap usage over time, per-function | Confirming the "per-room MessageRow bloat" hypothesis — expect large allocations under `MessageView::ensure_room_view`. |
| `heaptrack ./target/debug/hikyaku` + `heaptrack_gui` | Every allocation, leak candidates, top allocators, peak usage | The one to reach for on "6-second focus pause after idle" — if we're holding 500 MB of widgets, that's swap pressure. |
| `hyperfine './target/release/hikyaku --one-shot X'` | Wall-clock comparison across N runs with warmup | Before/after benchmarks of startup or a scripted operation. Not useful for interactive perf. |
| `tokio-console` (requires tokio_unstable) | Pending tasks, blocked tasks, long-running futures | If a matrix tokio future is blocking a send-side channel — suspected on long `bg_refresh` tails. |

Run with `--release` when the question is "how fast for the user". Run debug with frame pointers when the question is "where in the code does the time go". Don't mix the two.

## Process-isolation option (heavy tool)

For CPU-heavy parsing (HTML→Pango conversion, markdown rendering, regex-heavy linkification), moving work to a sidecar via D-Bus is a known GTK pattern. We don't need this today — the per-scope timings are sub-millisecond after the recent fixes — but if a future profile shows a parser exceeding 10ms on the GTK thread despite idle-deferral, the sidecar pattern (spawn once, request/response over D-Bus) is the standard escape hatch. Don't reach for it before proving the parser is genuinely the bottleneck.

---

## No-dialog policy

No dialog windows except the Settings window. User feedback goes via:
- `adw::Toast` for transient confirmations
- Inline bars / banners for persistent state

---

## Code style

- No `if`/`else` chains for dispatch — use `match`, `HashMap`, or trait objects
- No O(n) loops for lookups — `HashMap` keyed by ID
- No speculative abstractions — build what the current feature needs
- Extract repeated patterns into GObject-encapsulated APIs
- Add unit tests for any non-trivial logic; all tests pass before merging
