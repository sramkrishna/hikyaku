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

**Architectural alternatives that avoid full-replace splice:**

1. **Per-room ListStore + GtkStack** (recommended for chat apps):
   - Keep a `HashMap<RoomId, gio::ListStore>` plus one `gtk::ListView` per room
   - Wrap all ListViews in a `gtk::Stack`; on room switch call `stack.set_visible_child(room_list_view)`
   - First visit to a room still does an initial splice (one-time cost)
   - Returning to a room is O(1) — no splice, no rebind, GTK just shows the existing widget tree
   - Memory cost: ~15 pool widgets per room × n_rooms (acceptable for desktop apps with <100 rooms)

2. **Incremental append** (for the common case of new messages at end):
   - When new messages arrive for the currently visible room, `list_store.append(&obj)` per message
   - Only use full-replace splice for room switches
   - Amortizes the cost: only visible when first entering a room

3. **GtkListBox instead of GtkListView** (for small item counts):
   - No virtual scrolling, all items always in DOM
   - Simpler model-change handling, potentially faster for n<50
   - Loses factory pattern; use `bind_model` with a widget-creation callback

**Do not try to fix the splice by optimizing bind callbacks** — we confirmed our bind contributes <10% of the total splice time. The bottleneck is GTK.

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
