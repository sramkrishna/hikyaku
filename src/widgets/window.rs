// MxWindow — the main application window.
//
// Starts with a login page. After successful login, swaps to an
// AdwNavigationSplitView with the room list sidebar and message view.

mod imp {
    use adw::prelude::*;
    use adw::subclass::prelude::*;
    use gtk::glib;

    use async_channel::{Receiver, Sender};
    use std::cell::{Cell, OnceCell, RefCell};

    use crate::matrix::{MatrixCommand, MatrixEvent};
    use crate::widgets::BookmarksOverview;
    use crate::widgets::LoginPage;
    use crate::widgets::MessageView;
    use crate::widgets::OnboardingPage;
    use crate::widgets::RoomListView;

    pub struct MxWindow {
        pub event_rx: OnceCell<Receiver<MatrixEvent>>,
        pub command_tx: OnceCell<Sender<MatrixCommand>>,
        /// Shared timeline cache — written by the Matrix thread, read here for
        /// instant synchronous cache hits on room selection.
        pub timeline_cache: OnceCell<crate::matrix::room_cache::RoomCache>,
        pub onboarding_page: OnboardingPage,
        pub login_page: LoginPage,
        pub room_list_view: RoomListView,
        pub message_view: MessageView,
        pub toast_overlay: adw::ToastOverlay,
        pub toolbar: adw::ToolbarView,
        pub loading_spinner: gtk::Spinner,
        pub verify_banner: adw::Banner,
        /// Transient banner shown at the top for @mention notifications
        /// in rooms the user is not currently viewing.
        pub notify_banner: adw::Banner,
        /// Centralises in-app + desktop notification logic.
        pub notification_manager: crate::widgets::NotificationManager,
        /// Track which room is currently selected so we know where to
        /// route incoming messages and where to send outgoing ones.
        pub current_room_id: RefCell<Option<String>>,
        /// The content navigation page — title is updated to the selected room name.
        pub content_page: OnceCell<adw::NavigationPage>,
        /// The currently open waiting/emoji verification dialog, so we can
        /// dismiss it when the next state arrives.
        pub verify_dialog: RefCell<Option<adw::AlertDialog>>,
        /// Current room metadata for the details panel.
        pub current_room_meta: RefCell<Option<crate::matrix::RoomMeta>>,
        /// Right sidebar for room details.
        pub details_revealer: gtk::Revealer,
        pub details_content: gtk::Box,
        /// Separator between message view and details sidebar.
        pub details_separator: OnceCell<gtk::Separator>,
        /// Info button in the content header (shown only when a room is selected).
        pub info_button: OnceCell<gtk::Button>,
        /// Bookmark toggle button.
        pub bookmark_button: OnceCell<gtk::Button>,
        /// Export metrics button.
        pub export_button: OnceCell<gtk::Button>,
        /// Export messages button — dumps visible room messages to a JSONL file.
        pub export_messages_button: OnceCell<gtk::Button>,
        /// Current user ID for deduplicating local echo.
        pub user_id: RefCell<String>,
        /// Media cache: mxc_url → local file path.
        pub media_cache: RefCell<std::collections::HashMap<String, String>>,
        /// Avatar cache: user_id → local file path of the downloaded thumbnail.
        pub avatar_cache: RefCell<std::collections::HashMap<String, String>>,
        /// Widget to anchor the next media preview popover to.
        pub media_preview_anchor: RefCell<Option<gtk::Widget>>,
        /// Shared media preview popover — reused across hovers.
        pub media_popover: gtk::Popover,
        /// Timer for delayed read receipt + badge clear (15s after entering room).
        /// Stores (SourceId, fired flag, room_id). The flag is set by the callback
        /// so we know not to call .remove() on an already-fired one-shot timer.
        /// room_id lets us flush the receipt immediately on room-leave or app-close.
        pub read_timer: RefCell<Option<(glib::SourceId, std::rc::Rc<std::cell::Cell<bool>>, String)>>,
        /// Count of messages received in the current room while window is unfocused.
        pub unseen_while_unfocused: Cell<u32>,
        /// Popover used for AI hover summaries on the room list.
        pub hover_popover: gtk::Popover,
        /// Room ID for which a hover summary is in flight (to match the response).
        pub hover_room_id: RefCell<Option<String>>,
        /// Incremented on every new hover — lets in-flight futures detect they're stale.
        pub hover_gen: Cell<u32>,
        /// Accumulation buffer for streaming Ollama metrics summary chunks.
        pub metrics_summary_buf: RefCell<String>,
        /// Set of room IDs for which FetchRoomAvatar has already been sent this session.
        pub requested_room_avatars: RefCell<std::collections::HashSet<String>>,
        /// Active GLib timer that pulses the loading progress bar in the hover popover.
        pub hover_pulse_timer: RefCell<Option<glib::SourceId>>,
        /// Latest RoomListUpdated snapshot — written by the event loop, drained
        /// by the rooms idle.  Always holds the most recent state; older
        /// snapshots are silently discarded.
        pub pending_rooms: RefCell<Option<Vec<crate::matrix::RoomInfo>>>,
        /// Coalesced bg_refresh message batches — maps room_id → (messages,
        /// prev_batch_token).  When multiple RoomMessages { is_background: true }
        /// events arrive before the idle fires (e.g. 10 sync cycles while the
        /// window was unfocused), only one set_messages() call fires using the
        /// latest batch.  Without this, N events × 300–1000 ms = multi-second freeze.
        pub pending_bg_refresh: RefCell<std::collections::HashMap<String, (Vec<crate::matrix::MessageInfo>, Option<String>)>>,
        /// True while an idle callback for update_rooms() is already queued.
        /// Checked before scheduling a new idle — at most one update in-flight.
        pub rooms_idle_pending: Cell<bool>,
        /// True until the first RoomListUpdated after SyncStarted arrives.
        /// NewMessage notifications are suppressed during this window to prevent
        /// spam from historical catchup messages on first login.
        pub initial_sync_done: Cell<bool>,
        /// Full-window bookmarks overview (Ptyxis-style card grid).
        pub bookmarks_overview: BookmarksOverview,
        /// Bottom sheet that slides the bookmarks overview over the main view.
        pub bookmarks_sheet: OnceCell<adw::BottomSheet>,
        /// Event ID to flash after the next RoomMessages load (set by bookmark navigate).
        pub pending_flash_event_id: RefCell<Option<String>>,
        /// Whether the inline join/DM bar is currently visible (prevents stacking).
        pub inline_bar_active: Cell<bool>,
        /// Currently open public room directory dialog (if any).
        pub directory_dialog: RefCell<Option<adw::Dialog>>,
        /// The ListBox inside the open directory dialog — updated in-place on global search.
        pub directory_list_box: RefCell<Option<gtk::ListBox>>,
        /// Join buttons currently in "Joining…" state, keyed by room_id.
        /// Cleared and updated when RoomJoined / JoinFailed arrives.
        pub directory_join_buttons: RefCell<std::collections::HashMap<String, gtk::Button>>,
        /// The gtk::Stack inside the open directory dialog — flipped from "empty"
        /// to "results" when the first search response arrives.
        pub directory_stack: RefCell<Option<gtk::Stack>>,
        /// ListBox + optional space join context per space.
        /// The Option is Some((id_or_alias, via_servers)) when the space is NOT yet joined.
        pub directory_space_expanders: RefCell<std::collections::HashMap<String, (gtk::ListBox, Option<(String, Vec<String>)>)>>,
        /// PreferencesGroup per server — used to append new space ExpanderRows when
        /// PublicSpacesForServer arrives.
        pub directory_server_groups: RefCell<std::collections::HashMap<String, adw::PreferencesGroup>>,
        /// Outer box of the hierarchical directory dialog — used to append new server groups.
        pub directory_outer_box: RefCell<Option<gtk::Box>>,
        /// GObject list model for the personal contact book (Rolodex).
        /// Shared between the prefs page and the add/remove callbacks.
        pub rolodex_store: gio::ListStore,
        /// Local unread-message broker — counts every new message per room and
        /// persists across restarts so badges survive quit/reopen.
        pub local_unread: crate::local_unread::LocalUnreadStore,
        /// Callback registered by the active invite dialog.  Called with
        /// `UserSearchResults` so the dialog can update its results list without
        /// going through the full event channel.  Cleared when the dialog closes.
        pub user_search_cb: RefCell<Option<Box<dyn Fn(Vec<(String, String)>) + 'static>>>,
        /// In-session notification log (@mentions and DMs).  Max 50 entries;
        /// oldest are dropped when the list is full.
        pub notif_store: gio::ListStore,
        /// Revealer for the notifications right sidebar.
        pub notif_revealer: gtk::Revealer,
        /// Bell toggle button in the sidebar header — shows/hides the notif panel.
        pub notif_bell_button: OnceCell<gtk::ToggleButton>,
        /// Number of unread notifications — drives the bell badge label.
        pub notif_unread_count: Cell<u32>,
        /// The badge label overlaid on the bell button (shows unread count).
        pub notif_badge: OnceCell<gtk::Label>,
        /// ListBox inside the notification sidebar — rebuilt on each push.
        pub notif_list_box: OnceCell<gtk::ListBox>,
    }

    impl Default for MxWindow {
        fn default() -> Self {
            let verify_banner = adw::Banner::builder()
                .title("This device is not verified. Verify to decrypt messages in encrypted rooms.")
                .button_label("Verify")
                .revealed(false)
                .build();

            Self {
                event_rx: OnceCell::new(),
                command_tx: OnceCell::new(),
                timeline_cache: OnceCell::new(),
                onboarding_page: OnboardingPage::new(),
                login_page: LoginPage::new(),
                room_list_view: RoomListView::new(),
                message_view: MessageView::new(),
                toast_overlay: adw::ToastOverlay::new(),
                toolbar: adw::ToolbarView::new(),
                loading_spinner: gtk::Spinner::new(),
                verify_banner,
                notify_banner: adw::Banner::builder()
                    .revealed(false)
                    .button_label("Jump")
                    .css_classes(["accent"])
                    .build(),
                notification_manager: crate::widgets::NotificationManager::new(),
                current_room_id: RefCell::new(None),
                content_page: OnceCell::new(),
                verify_dialog: RefCell::new(None),
                current_room_meta: RefCell::new(None),
                details_revealer: gtk::Revealer::builder()
                    .transition_type(gtk::RevealerTransitionType::None)
                    .reveal_child(false)
                    .visible(false)
                    .build(),
                details_content: gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .width_request(200)
                    .build(),
                details_separator: OnceCell::new(),
                info_button: OnceCell::new(),
                bookmark_button: OnceCell::new(),
                export_button: OnceCell::new(),
                export_messages_button: OnceCell::new(),
                user_id: RefCell::new(String::new()),
                media_cache: RefCell::new(std::collections::HashMap::new()),
                avatar_cache: RefCell::new(std::collections::HashMap::new()),
                media_preview_anchor: RefCell::new(None),
                media_popover: {
                    let p = gtk::Popover::new();
                    p.set_autohide(true);
                    p.set_has_arrow(true);
                    p
                },
                read_timer: RefCell::new(None),
                unseen_while_unfocused: Cell::new(0),
                hover_popover: {
                    let p = gtk::Popover::new();
                    // autohide(false) so switching to another app doesn't
                    // dismiss the summary mid-generation. The user can still
                    // close it with Escape or by Ctrl+clicking another room.
                    p.set_autohide(false);
                    p.set_has_arrow(true);
                    p
                },
                hover_room_id: RefCell::new(None),
                hover_gen: Cell::new(0),
                metrics_summary_buf: RefCell::new(String::new()),
                requested_room_avatars: RefCell::new(std::collections::HashSet::new()),
                hover_pulse_timer: RefCell::new(None),
                pending_rooms: RefCell::new(None),
                pending_bg_refresh: RefCell::new(std::collections::HashMap::new()),
                rooms_idle_pending: Cell::new(false),
                initial_sync_done: Cell::new(false),
                bookmarks_overview: BookmarksOverview::new(),
                bookmarks_sheet: OnceCell::new(),
                pending_flash_event_id: RefCell::new(None),
                inline_bar_active: Cell::new(false),
                directory_dialog: RefCell::new(None),
                directory_list_box: RefCell::new(None),
                directory_join_buttons: RefCell::new(std::collections::HashMap::new()),
                directory_stack: RefCell::new(None),
                directory_space_expanders: RefCell::new(std::collections::HashMap::new()),
                directory_server_groups: RefCell::new(std::collections::HashMap::new()),
                directory_outer_box: RefCell::new(None),
                rolodex_store: {
                    let store = gio::ListStore::new::<crate::models::RolodexEntryObject>();
                    // Populate from JSON on startup.
                    for e in crate::plugins::rolodex::load() {
                        store.append(&crate::models::RolodexEntryObject::new(
                            &e.user_id, &e.display_name, &e.notes, e.added_at,
                        ));
                    }
                    store
                },
                local_unread: crate::local_unread::LocalUnreadStore::new(),
                user_search_cb: RefCell::new(None),
                notif_store: gio::ListStore::new::<crate::models::NotificationObject>(),
                notif_revealer: gtk::Revealer::builder()
                    .transition_type(gtk::RevealerTransitionType::SlideLeft)
                    .reveal_child(false)
                    .visible(false)
                    .build(),
                notif_bell_button: OnceCell::new(),
                notif_unread_count: Cell::new(0),
                notif_badge: OnceCell::new(),
                notif_list_box: OnceCell::new(),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MxWindow {
        const NAME: &'static str = "MxWindow";
        type Type = super::MxWindow;
        type ParentType = adw::ApplicationWindow;
    }

    impl ObjectImpl for MxWindow {
        fn constructed(&self) {
            self.parent_constructed();

            #[cfg(feature = "devel")]
            self.obj().add_css_class("devel");

            // Start with a loading spinner — if we have a saved session the
            // Matrix thread will restore it and jump straight to the main
            // view, so the user never sees the login page.
            self.loading_spinner.set_spinning(true);
            self.loading_spinner.set_halign(gtk::Align::Center);
            self.loading_spinner.set_valign(gtk::Align::Center);
            self.loading_spinner.set_vexpand(true);
            self.loading_spinner.set_width_request(32);
            self.loading_spinner.set_height_request(32);

            self.toolbar.add_top_bar(&adw::HeaderBar::new());
            self.toolbar.set_content(Some(&self.loading_spinner));

            self.toast_overlay.set_child(Some(&self.toolbar));
            self.obj().set_content(Some(&self.toast_overlay));
        }
    }

    impl WidgetImpl for MxWindow {}
    impl WindowImpl for MxWindow {}
    impl ApplicationWindowImpl for MxWindow {}
    impl AdwApplicationWindowImpl for MxWindow {}
}

use adw::prelude::*;
use gtk::glib;
use gtk::subclass::prelude::*;

use async_channel::{Receiver, Sender};

use crate::config::AppearanceSettings;
use crate::matrix::{MatrixCommand, MatrixEvent};

/// Build the Ollama prompt for the metrics summary feature.
fn build_metrics_prompt(metrics_text: &str, detect_conflict: bool, detect_coc: bool) -> String {
    let mut sections = vec![
        "1. Interesting conversations: identify threads or exchanges that show \
         genuine knowledge-sharing, creative problem-solving, or topics of broad \
         community interest. Note who contributed and what made it notable.".to_string(),
    ];
    if detect_conflict {
        sections.push(
            "2. Conflict and spam signals: flag any patterns of escalating \
             disagreement, unusually high message frequency from single users, \
             or repetitive content that may indicate spam.".to_string(),
        );
    }
    if detect_coc {
        sections.push(
            "3. Code-of-conduct signals: note ban/kick events, users actioned \
             more than once, or unusual moderation activity.".to_string(),
        );
    }
    format!(
        "You are a community manager assistant analyzing Matrix room activity. \
         Review the metrics below and provide a concise report (3-5 bullet points \
         per section) covering:\n{}\n\nBe specific about numbers. \
         If data is insufficient for a section, say so briefly.\n\nMetrics:\n{metrics_text}",
        sections.join("\n")
    )
}

/// Detect MIME type for a local file path using GIO content-type guessing.
fn mime_for_path(path: &str) -> String {
    // Ask GIO to detect the content type from the file itself (magic bytes +
    // extension). standard::content-type causes GIO to read the file header,
    // so PNG, WebP, MP4, WebM etc. are identified correctly even when the
    // cached filename has no extension.
    let file = gio::File::for_path(path);
    if let Ok(info) = file.query_info(
        "standard::content-type",
        gio::FileQueryInfoFlags::NONE,
        gio::Cancellable::NONE,
    ) {
        if let Some(ct) = info.content_type() {
            if let Some(mime) = gio::functions::content_type_get_mime_type(&ct) {
                return mime.to_string();
            }
        }
    }
    "application/octet-stream".to_string()
}

/// Show a media preview in-app for images/video; launch system default app for everything else.
fn show_media_preview(window: &MxWindow, _anchor: &gtk::Widget, path: &str) {
    let mime = mime_for_path(path);

    // Images and video: show in-app dialog.
    if mime.starts_with("image/") || mime.starts_with("video/") {
        let dialog = adw::Dialog::builder()
            .content_width(600)
            .content_height(500)
            .title("Media Preview")
            .build();

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());

        if mime.starts_with("video/") || mime == "image/gif" {
            let media_file = gtk::MediaFile::for_filename(path);
            if mime == "image/gif" {
                media_file.set_loop(true);
            }
            media_file.play();
            let video = gtk::Video::new();
            video.set_media_stream(Some(&media_file));
            video.set_vexpand(true);
            video.set_hexpand(true);
            toolbar.set_content(Some(&video));
        } else {
            let file = gio::File::for_path(path);
            let picture = gtk::Picture::for_file(&file);
            picture.set_can_shrink(true);
            picture.set_content_fit(gtk::ContentFit::Contain);
            picture.set_vexpand(true);
            picture.set_hexpand(true);
            toolbar.set_content(Some(&picture));
        }

        dialog.set_child(Some(&toolbar));
        dialog.present(Some(window));
        return;
    }

    // Everything else (PDF, audio, documents, …) — hand off to the system default app.
    let uri = format!("file://{}", path);
    if let Err(e) = gio::AppInfo::launch_default_for_uri(&uri, gio::AppLaunchContext::NONE) {
        tracing::warn!("Failed to open {uri} with default app: {e}");
        // Fall back to xdg-open.
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

fn toast(overlay: &adw::ToastOverlay, msg: &str) {
    overlay.add_toast(adw::Toast::new(msg));
}

/// Show a toast when the window is focused, or a D-Bus desktop notification
/// when the window is unfocused.  Use for notification-worthy events (mentions,
/// alerts, DMs) so the user is always informed regardless of app focus.
fn toast_or_notify(
    window: &MxWindow,
    overlay: &adw::ToastOverlay,
    notif_id: &str,
    title: &str,
    body: &str,
) {
    use gtk::prelude::GtkWindowExt;
    use gio::prelude::ApplicationExt;
    if window.is_active() {
        let t = adw::Toast::builder()
            .title(body)
            .timeout(8)
            .build();
        overlay.add_toast(t);
    } else {
        if let Some(app) = GtkWindowExt::application(window) {
            let notif = gio::Notification::new(title);
            notif.set_body(Some(body));
            notif.set_priority(gio::NotificationPriority::High);
            notif.set_default_action("app.activate");
            app.send_notification(Some(notif_id), &notif);
        }
    }
}

/// Show a toast with a formatted error message.
fn toast_error(overlay: &adw::ToastOverlay, prefix: &str, error: &str) {
    overlay.add_toast(
        adw::Toast::builder()
            .title(&format!("{prefix}: {error}"))
            .timeout(30)
            .build()
    );
}

thread_local! {
    static BOOKMARK_CSS_PROVIDER: gtk::CssProvider = {
        let p = gtk::CssProvider::new();
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &p,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
            );
        }
        p
    };

    static NEW_MESSAGE_CSS_PROVIDER: gtk::CssProvider = {
        let p = gtk::CssProvider::new();
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &p,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
            );
        }
        p
    };

    static TINT_PROVIDER: gtk::CssProvider = {
        let p = gtk::CssProvider::new();
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &p,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
            );
        }
        p
    };

    /// Static app CSS: active room highlight, etc.
    static APP_CSS_PROVIDER: gtk::CssProvider = {
        let p = gtk::CssProvider::new();
        p.load_from_string(
            ".active-room-row { \
               background-color: alpha(@accent_bg_color, 0.15); \
               border-radius: 6px; \
             } \
             .active-room-row label { color: @accent_color; font-weight: bold; } \
             @keyframes message-flash { \
               0%   { background-color: alpha(@accent_bg_color, 0.45); } \
               100% { background-color: transparent; } \
             } \
             .message-flash { animation: message-flash 0.9s ease-out; } \
             .bookmark-card, .bookmark-room-card { border-radius: 12px; } \
             .bookmark-delete-btn { opacity: 0; transition: opacity 200ms; } \
             flowboxchild:hover .bookmark-delete-btn { opacity: 1; } \
             .rolodex-contact { \
               color: @accent_color; \
               font-weight: bold; \
               text-decoration: underline; \
               filter: drop-shadow(0 0 4px alpha(@accent_color, 0.5)); \
             } \
             .health-dot { border-radius: 50%; } \
             .health-dot.health-none    { background-color: #26a269; } \
             .health-dot.health-watch   { background-color: #e5a50a; } \
             .health-dot.health-warning { background-color: #e01b24; } \
             .notif-unread { background-color: alpha(@accent_bg_color, 0.12); } \
             .notif-unread:hover { background-color: alpha(@accent_bg_color, 0.22); } \
             .notif-badge { \
               background-color: @destructive_color; \
               color: white; \
               border-radius: 99px; \
               font-size: 8px; \
               font-weight: bold; \
               padding: 1px 4px; \
               min-width: 8px; \
               margin-top: 2px; \
               margin-end: 2px; \
             } \
             .notif-bell-unread image { color: @accent_color; } \
             .notif-sidebar { \
               background-color: @sidebar_bg_color; \
               border-left: 1px solid @borders; \
             }"
        );
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &p,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        p
    };
}

fn tint_rule(class: &str, color: &str, color2: &str, opacity: f64) -> String {
    let transparent = format!(
        "{class}, {class} > listview, {class} > listview > row, {class} > listview > listitem \
         {{ background-color: transparent; }}"
    );
    if color.is_empty() {
        return transparent;
    }
    let rgba = gtk::gdk::RGBA::parse(color).unwrap_or(gtk::gdk::RGBA::BLACK);
    let r = (rgba.red() * 255.0) as u8;
    let g = (rgba.green() * 255.0) as u8;
    let b = (rgba.blue() * 255.0) as u8;
    let op = opacity.clamp(0.0, 0.5);

    if color2.is_empty() {
        // Solid tint — same color on container and all list children.
        format!(
            "{class}, {class} > listview, {class} > listview > row, \
             {class} > listview > listitem {{ background-color: rgba({r},{g},{b},{op}); }}"
        )
    } else {
        // Gradient — apply to container only, children are transparent so the
        // gradient shows through the scrollable list.
        let rgba2 = gtk::gdk::RGBA::parse(color2).unwrap_or(gtk::gdk::RGBA::BLACK);
        let r2 = (rgba2.red() * 255.0) as u8;
        let g2 = (rgba2.green() * 255.0) as u8;
        let b2 = (rgba2.blue() * 255.0) as u8;
        format!(
            "{class} {{ background: linear-gradient(to bottom, \
               rgba({r},{g},{b},{op}), rgba({r2},{g2},{b2},{op})); }}\n\
             {class} > listview, {class} > listview > row, \
             {class} > listview > listitem {{ background-color: transparent; }}"
        )
    }
}

/// Apply background tints for message area and sidebar via a single CSS provider.
fn apply_tint_css(settings: &AppearanceSettings) {
    let css = format!(
        "{}\n{}",
        tint_rule(".mx-tinted-bg", &settings.tint_color, &settings.tint_color2, settings.tint_opacity),
        tint_rule(".mx-tinted-sidebar", &settings.sidebar_tint_color, &settings.sidebar_tint_color2, settings.sidebar_tint_opacity),
    );
    TINT_PROVIDER.with(|p| p.load_from_string(&css));
}

/// Apply bookmark highlight color via a thread-local CSS provider.
fn apply_bookmark_css(color: &str) {
    let color = if color.is_empty() { "#f5c542" } else { color };
    let css = if let Ok(rgba) = gtk::gdk::RGBA::parse(color) {
        let r = (rgba.red() * 255.0) as u8;
        let g = (rgba.green() * 255.0) as u8;
        let b = (rgba.blue() * 255.0) as u8;
        format!(
            ".bookmarked-message {{ \
               background-color: rgba({r},{g},{b},0.15); \
               border-radius: 8px; \
               border-left: 3px solid rgba({r},{g},{b},0.7); \
               padding: 6px 10px; \
               margin: 2px 4px; \
             }}"
        )
    } else {
        String::new()
    };
    BOOKMARK_CSS_PROVIDER.with(|p| p.load_from_string(&css));
}

fn apply_new_message_css(color: &str) {
    let color = if color.is_empty() { "#5B9BD5" } else { color };
    let css = if let Ok(rgba) = gtk::gdk::RGBA::parse(color) {
        let r = (rgba.red() * 255.0) as u8;
        let g = (rgba.green() * 255.0) as u8;
        let b = (rgba.blue() * 255.0) as u8;
        format!(
            ".new-message {{ \
               background-color: rgba({r},{g},{b},0.12); \
               border-radius: 4px; \
             }}"
        )
    } else {
        String::new()
    };
    NEW_MESSAGE_CSS_PROVIDER.with(|p| p.load_from_string(&css));
}

/// Apply font settings via a CSS provider on the default display.
/// Targets both `.mx-message-body` (the message text) and
/// `.mx-message-sender` (the nick above it) so the Appearance preference
/// scales the whole message block together rather than just the body.
fn apply_font_css(settings: &AppearanceSettings) {
    let selectors = ".mx-message-body, .mx-message-sender";
    let css = if settings.font_family.is_empty() {
        format!("{selectors} {{ font-size: {}pt; }}", settings.font_size)
    } else {
        format!(
            "{selectors} {{ font-family: \"{}\"; font-size: {}pt; }}",
            settings.font_family, settings.font_size
        )
    };

    let provider = gtk::CssProvider::new();
    provider.load_from_string(&css);

    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

glib::wrapper! {
    pub struct MxWindow(ObjectSubclass<imp::MxWindow>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl MxWindow {
    pub fn new(
        app: &crate::application::MxApplication,
        event_rx: Receiver<MatrixEvent>,
        command_tx: Sender<MatrixCommand>,
        timeline_cache: crate::matrix::room_cache::RoomCache,
    ) -> Self {
        let window: Self = glib::Object::builder()
            .property("application", app)
            .build();

        let imp = window.imp();
        let _ = imp.event_rx.set(event_rx.clone());
        let _ = imp.command_tx.set(command_tx.clone());
        let _ = imp.timeline_cache.set(timeline_cache);

        // Load persisted local unread counts, then connect broker → room list.
        imp.local_unread.load();
        {
            let rlv = imp.room_list_view.clone();
            imp.local_unread.connect_room_unread_changed(move |_, room_id, unread, highlights| {
                rlv.set_room_unread_counts(&room_id, unread, highlights);
            });
        }

        // Wire up login button.
        let cmd_tx = command_tx.clone();
        imp.login_page.connect_login_requested(move |homeserver, username, password| {
            let tx = cmd_tx.clone();
            glib::spawn_future_local(async move {
                let _ = tx.send(MatrixCommand::Login {
                    homeserver,
                    username,
                    password,
                }).await;
            });
        });

        // Wire up register button.
        let cmd_tx = command_tx.clone();
        imp.login_page.connect_register_requested(move |hs, user, pass, display, email| {
            let tx = cmd_tx.clone();
            glib::spawn_future_local(async move {
                let _ = tx.send(MatrixCommand::Register {
                    homeserver: hs,
                    username: user,
                    password: pass,
                    display_name: display,
                    email,
                }).await;
            });
        });

        // Wire up recovery key save (best-effort — no save_recovery_key in session.rs yet).
        imp.login_page.connect_save_recovery_key(|_key| {
            // TODO: save recovery key to GNOME Keyring via session::save_recovery_key
            tracing::info!("Recovery key save requested (not yet implemented)");
        });

        // Recovery key confirmed → fetch public rooms for the local-spaces wizard page.
        {
            let cmd_tx = command_tx.clone();
            imp.login_page.connect_recovery_key_confirmed(move || {
                let tx = cmd_tx.clone();
                glib::spawn_future_local(async move {
                    let _ = tx.send(MatrixCommand::BrowsePublicRooms {
                        search_term: None,
                        spaces_only: true,
                        server: None,
                    }).await;
                });
            });
        }

        // Join rooms confirmed or skipped → send join commands (navigation continues to resources).
        {
            let cmd_tx = command_tx.clone();
            imp.login_page.connect_join_rooms(move |room_aliases| {
                let tx = cmd_tx.clone();
                glib::spawn_future_local(async move {
                    for alias in room_aliases {
                        let _ = tx.send(MatrixCommand::JoinRoom { room_id_or_alias: alias, via_servers: vec![] }).await;
                    }
                });
            });
        }

        // AI setup page → save settings; trigger model download after wizard if enabled.
        imp.login_page.connect_ai_setup(|enabled, model| {
            let gs = crate::config::gsettings();
            let _ = gs.set_boolean("ollama-enabled", enabled);
            let _ = gs.set_string("ollama-model", &model);
            tracing::info!("Wizard AI setup: enabled={enabled}, model={model}");
        });

        // Get-started "Start Chatting" → enter the app, then start model download if needed.
        {
            let window_ref = window.clone();
            imp.login_page.connect_finish(move || {
                window_ref.show_main_view();
                let cfg = crate::config::settings();
                if cfg.ollama.enabled && !cfg.ollama.setup_done {
                    let w = window_ref.clone();
                    glib::timeout_add_local_once(
                        std::time::Duration::from_millis(800),
                        move || show_ai_setup_dialog(&w),
                    );
                }
            });
        }

        // Verification callbacks.
        {
            let cmd_tx = command_tx.clone();
            let window_ref = window.clone();
            imp.login_page.connect_verify_with_device(move || {
                let w = window_ref.clone();
                // Guard: if a verification dialog is already open, do nothing.
                // Without this, a second tap would cancel the in-progress request.
                if w.imp().verify_dialog.borrow().is_some() {
                    return;
                }
                let tx = cmd_tx.clone();
                let dialog = crate::widgets::verification_dialog::show_waiting_dialog(
                    &w, tx.clone(),
                );
                let window_weak = w.downgrade();
                let tx2 = tx.clone();
                dialog.connect_response(None, move |_, response| {
                    if let Some(win) = window_weak.upgrade() {
                        win.imp().verify_dialog.replace(None);
                        if response == "cancel" {
                            let tx = tx2.clone();
                            glib::spawn_future_local(async move {
                                let _ = tx.send(MatrixCommand::CancelVerification {
                                    flow_id: String::new(),
                                }).await;
                            });
                        }
                    }
                });
                w.imp().verify_dialog.replace(Some(dialog));
                glib::spawn_future_local(async move {
                    let _ = tx.send(MatrixCommand::RequestSelfVerification).await;
                });
            });
        }
        {
            let cmd_tx = command_tx.clone();
            let window_ref = window.clone();
            let toast_ref = imp.toast_overlay.clone();
            imp.login_page.connect_recover_with_key(move |key| {
                toast(&toast_ref, "Recovering keys… this may take a minute.");
                let tx = cmd_tx.clone();
                let w = window_ref.clone();
                glib::spawn_future_local(async move {
                    let _ = tx.send(MatrixCommand::RecoverKeys { recovery_key: key }).await;
                    // After recovery, go to the main view (same as skip).
                    w.show_main_view();
                });
            });
        }
        {
            let window_ref = window.clone();
            let toast_ref = imp.toast_overlay.clone();
            imp.login_page.connect_skip_verification(move || {
                toast(&toast_ref, "Encrypted messages will not be readable until you verify this device");
                window_ref.show_main_view();
            });
        }

        // Wire up room selection → send SelectRoom command.
        let cmd_tx = command_tx.clone();
        let window_weak = window.downgrade();
        let msg_view = imp.message_view.clone();
        let room_list = imp.room_list_view.clone();
        imp.room_list_view.connect_room_selected(move |room_id, room_name| {
            let _t0 = std::time::Instant::now();
            let _t_phase1 = std::time::Instant::now();
            if let Some(window) = window_weak.upgrade() {
                window.imp().current_room_id.replace(Some(room_id.clone()));
                // Drop any pending bg_refresh batches for rooms we're leaving —
                // stale data for the old room must not fire set_messages() later.
                window.imp().pending_bg_refresh.borrow_mut().retain(|rid, _| rid == &room_id);
                window.imp().notification_manager.set_current_room(Some(&room_id));
                room_list.set_active_room(&room_id);
                if let Some(page) = window.imp().content_page.get() {
                    page.set_title(&room_name);
                }
                // Show header buttons now that a room is selected.
                if let Some(btn) = window.imp().info_button.get() {
                    btn.set_visible(true);
                }
                if let Some(btn) = window.imp().bookmark_button.get() {
                    btn.set_visible(true);
                }
                if let Some(btn) = window.imp().export_button.get() {
                    btn.set_visible(true);
                }
                if let Some(btn) = window.imp().export_messages_button.get() {
                    btn.set_visible(true);
                }
                // Hide details sidebar when switching rooms.
                window.imp().details_revealer.set_reveal_child(false);
                window.imp().details_revealer.set_visible(false);
                if let Some(sep) = window.imp().details_separator.get() {
                    sep.set_visible(false);
                }
            }
            // Capture the current unread badge BEFORE clear_unread() zeroes it.
            // Passed to SelectRoom so the Matrix thread can use it as a floor for
            // quick_meta when the SDK store returns 0 pre-sync.
            let known_unread = room_list.imp().room_registry.borrow()
                .get(&room_id)
                .map(|o| o.unread_count())
                .unwrap_or(0);
            // Suppress server unread counts immediately — add to locally_read
            // so sync cycles don't overwrite the badge while we're in the room.
            // The actual read receipt + badge clear happens after 15 seconds.
            room_list.clear_unread(&room_id);
            if let Some(w) = window_weak.upgrade() {
                w.imp().local_unread.mark_read(&room_id);
            }

            tracing::info!("room_selected phase1 (widget updates+unread) took {:?}", _t_phase1.elapsed());
            let _t_phase2 = std::time::Instant::now();
            // Delay sending read receipt by 15 seconds.
            // This ensures the user actually stays in the room before telling the server.
            if let Some(window) = window_weak.upgrade() {
                let imp = window.imp();
                // Flush any pending receipt for the room we're leaving.
                // timeout_add_local_once auto-removes the source after firing,
                // so only call .remove() if the callback hasn't fired yet.
                if let Some((old_id, fired, old_rid)) = imp.read_timer.borrow_mut().take() {
                    if !fired.get() {
                        old_id.remove();
                        // User is leaving the room — mark it as read now.
                        tracing::info!("Room leave: sending MarkRead for {old_rid}");
                        room_list.clear_unread(&old_rid);
                        imp.local_unread.mark_read(&old_rid);
                        let tx = cmd_tx.clone();
                        glib::spawn_future_local(async move {
                            let _ = tx.send(MatrixCommand::MarkRead { room_id: old_rid }).await;
                        });
                    } else {
                        tracing::info!("Room leave: timer already fired for {old_rid}, no MarkRead needed");
                    }
                }
                let room_list_timer = room_list.clone();
                let rid_timer = room_id.clone();
                let cmd_tx_timer = cmd_tx.clone();
                let win_weak_timer = window.downgrade();
                let fired_flag = std::rc::Rc::new(std::cell::Cell::new(false));
                let fired_for_cb = fired_flag.clone();
                let source = glib::timeout_add_local_once(
                    std::time::Duration::from_secs(15),
                    move || {
                        // Mark as fired so cancellation won't call .remove().
                        fired_for_cb.set(true);
                        // Only clear if we're still in the same room.
                        let current = win_weak_timer.upgrade()
                            .and_then(|w| w.imp().current_room_id.borrow().clone());
                        let still_here = current.as_deref() == Some(rid_timer.as_str());
                        tracing::info!(
                            "Read timer fired for {}, still_here={}, current={:?}",
                            rid_timer, still_here, current
                        );
                        if still_here {
                            room_list_timer.clear_unread(&rid_timer);
                            if let Some(w) = win_weak_timer.upgrade() {
                                w.imp().local_unread.mark_read(&rid_timer);
                            }
                            let tx = cmd_tx_timer.clone();
                            let rid = rid_timer.clone();
                            glib::spawn_future_local(async move {
                                let _ = tx.send(MatrixCommand::MarkRead { room_id: rid }).await;
                            });
                        }
                    },
                );
                imp.read_timer.replace(Some((source, fired_flag, room_id.clone())));
            }
            tracing::info!("room_selected phase2 (read timer setup) took {:?}", _t_phase2.elapsed());
            let _t_phase3 = std::time::Instant::now();
            // Instant path: read the cache synchronously on the GTK thread —
            // no async round-trip, no loading flash for warm-cache rooms.
            let cache_hit = window_weak.upgrade()
                .and_then(|w| w.imp().timeline_cache.get().cloned())
                .and_then(|cache| cache.get_memory(&room_id));
            tracing::info!("room_selected phase3a (get_memory) took {:?}", _t_phase3.elapsed());

            if let Some((msgs, prev_batch, mut meta)) = cache_hit {
                meta.unread_count = meta.unread_count.max(known_unread);
                if let Some(window) = window_weak.upgrade() {
                    let wimp = window.imp();
                    wimp.current_room_meta.replace(Some(meta.clone()));
                    let is_dm = {
                        let reg = wimp.room_list_view.imp().room_registry.borrow();
                        reg.get(&room_id)
                            .map(|o| o.kind() == crate::matrix::RoomKind::DirectMessage)
                            .unwrap_or(false)
                    };
                    tracing::info!("room_selected phase3b (is_dm lookup) took {:?}", _t_phase3.elapsed());
                    msg_view.set_is_dm_room(is_dm);
                    msg_view.set_no_media(wimp.room_list_view.resolve_no_media(&room_id));
                    tracing::info!("room_selected phase3c (set_is_dm+no_media) took {:?}", _t_phase3.elapsed());
                    // Switch to the new room's per-room ListView (O(1)).
                    // For return visits existing messages show immediately;
                    // for first visits the spinner shows until set_messages arrives.
                    msg_view.clear(&room_id);
                    tracing::info!("room_selected phase3d (clear) took {:?}", _t_phase3.elapsed());
                    msg_view.set_room_meta(&meta);
                    if let Some(btn) = wimp.bookmark_button.get() {
                        btn.set_icon_name(if meta.is_favourite {
                            "starred-symbolic"
                        } else {
                            "non-starred-symbolic"
                        });
                        btn.set_tooltip_text(Some(if meta.is_favourite {
                            "Remove bookmark"
                        } else {
                            "Bookmark this room"
                        }));
                    }
                    // Call set_messages synchronously.  clear() already swapped
                    // the model; the splice is into an empty store with model
                    // detached (O(N) memory, no GTK layout until re-attach).
                    // Deferring to idle caused vsync starvation on Lunar Lake
                    // (see clear() comment for details).
                    let _t_sm = std::time::Instant::now();
                    msg_view.set_messages(&msgs, prev_batch);
                    tracing::info!("room_selected phase3e (set_messages) took {:?}", _t_sm.elapsed());
                    let _t_bm = std::time::Instant::now();
                    msg_view.load_bookmarks(&room_id);
                    tracing::info!("room_selected phase3f (load_bookmarks) took {:?}", _t_bm.elapsed());
                }
            } else {
                // Cold cache — show loading state and wait for Tokio.
                msg_view.clear(&room_id);
            }

            // Always send SelectRoom so Tokio can dirty-check, run bg_refresh,
            // and deliver an authoritative RoomMessages event.  set_messages
            // handles idempotent updates (no-splice when nothing changed).
            tracing::info!("room_selected GTK handler took {:?} for {room_id}", _t0.elapsed());
            let tx = cmd_tx.clone();
            let rid = room_id.clone();
            glib::spawn_future_local(async move {
                let _ = tx.send(MatrixCommand::SelectRoom { room_id: rid, known_unread }).await;
            });

            // Prefetch sibling rooms for spaces so next selection is instant.
            let parent_space_id = room_list.imp().room_registry.borrow()
                .get(&room_id)
                .map(|o| o.parent_space_id())
                .unwrap_or_default();
            if !parent_space_id.is_empty() {
                let sibling_ids: Vec<String> = room_list.imp().room_registry.borrow()
                    .iter()
                    .filter(|(rid, obj)| {
                        **rid != room_id && obj.parent_space_id() == parent_space_id
                    })
                    .map(|(rid, _)| rid.clone())
                    .collect();
                if !sibling_ids.is_empty() {
                    let tx = cmd_tx.clone();
                    let sid = parent_space_id.clone();
                    glib::spawn_future_local(async move {
                        let _ = tx.send(MatrixCommand::PrefetchSpace {
                            space_id: sid,
                            room_ids: sibling_ids,
                        }).await;
                    });
                }
            }
        });

        // Leave button is wired in setup_ui.

        // Wire Ctrl+click → FetchRoomPreview → show AI summary popover.
        {
            // Cancel in-flight inference whenever the popover is dismissed for any
            // reason (Escape, Cancel button, new request). hover_room_id being set
            // means inference is still running — if it's already cleared (content
            // done) this is a no-op.
            let cancel_tx_close = command_tx.clone();
            let window_weak_close = window.downgrade();
            imp.hover_popover.connect_closed(move |_| {
                let Some(win) = window_weak_close.upgrade() else { return };
                // If hover_room_id is still set, inference hasn't finished yet.
                if win.imp().hover_room_id.borrow().is_some() {
                    win.imp().hover_room_id.borrow_mut().take();
                    if let Some(sid) = win.imp().hover_pulse_timer.borrow_mut().take() {
                        sid.remove();
                    }
                    let tx = cancel_tx_close.clone();
                    glib::spawn_future_local(async move {
                        let _ = tx.send(crate::matrix::MatrixCommand::CancelRoomPreview).await;
                    });
                }
            });

            let cmd_tx_h = command_tx.clone();
            let window_weak_h = window.downgrade();
            imp.room_list_view.connect_room_preview_requested(move |room_id, y| {
                let Some(window) = window_weak_h.upgrade() else { return };
                // Only fetch if Ollama is enabled and configured.
                let ollama_cfg = crate::config::settings().ollama;
                if !ollama_cfg.enabled || ollama_cfg.endpoint.is_empty() { return; }

                // Dismiss any previous summary before showing the new one.
                window.imp().hover_popover.popdown();

                *window.imp().hover_room_id.borrow_mut() = Some(room_id.clone());
                let gen = window.imp().hover_gen.get().wrapping_add(1);
                window.imp().hover_gen.set(gen);

                // Show a pulsing progress bar in the popover while the model loads.
                let loading_box = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .spacing(4)
                    .margin_start(12).margin_end(12)
                    .margin_top(10).margin_bottom(8)
                    .width_request(260)
                    .build();
                let top_row = gtk::Box::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .spacing(8)
                    .build();
                let title_label = gtk::Label::builder()
                    .label("Generating summary…")
                    .hexpand(true)
                    .xalign(0.0)
                    .build();
                let cancel_btn = gtk::Button::builder()
                    .label("Cancel")
                    .css_classes(["flat", "caption"])
                    .build();
                top_row.append(&title_label);
                top_row.append(&cancel_btn);
                let progress_bar = gtk::ProgressBar::new();
                progress_bar.set_pulse_step(0.15);
                let hint_label = gtk::Label::builder()
                    .label("May take 1–5 min if model isn't loaded")
                    .css_classes(["dim-label", "caption"])
                    .xalign(0.0)
                    .wrap(true)
                    .build();
                loading_box.append(&top_row);
                loading_box.append(&progress_bar);
                loading_box.append(&hint_label);

                let popover = &window.imp().hover_popover;
                {
                    let pop = popover.clone();
                    let win_weak = window.downgrade();
                    let cancel_tx = cmd_tx_h.clone();
                    cancel_btn.connect_clicked(move |_| {
                        if let Some(win) = win_weak.upgrade() {
                            if let Some(sid) = win.imp().hover_pulse_timer.borrow_mut().take() {
                                sid.remove();
                            }
                            win.imp().hover_room_id.borrow_mut().take();
                        }
                        // Tell the tokio thread to abort the in-flight Ollama request.
                        let tx = cancel_tx.clone();
                        glib::spawn_future_local(async move {
                            let _ = tx.send(crate::matrix::MatrixCommand::CancelRoomPreview).await;
                        });
                        pop.popdown();
                    });
                }
                // Cancel any previous pulse timer before starting a new one.
                if let Some(sid) = window.imp().hover_pulse_timer.borrow_mut().take() {
                    sid.remove();
                }
                let bar_weak = progress_bar.downgrade();
                let sid = glib::timeout_add_local(
                    std::time::Duration::from_millis(150),
                    move || {
                        if let Some(bar) = bar_weak.upgrade() {
                            bar.pulse();
                            glib::ControlFlow::Continue
                        } else {
                            glib::ControlFlow::Break
                        }
                    },
                );
                window.imp().hover_pulse_timer.replace(Some(sid));
                popover.set_child(Some(&loading_box));

                // Anchor the popover to the room list widget (once only).
                let rlv: &gtk::Widget = window.imp().room_list_view.upcast_ref();
                if popover.parent().is_none() {
                    popover.set_parent(rlv);
                }
                // Point the popover arrow at the clicked row.
                popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
                    0, y as i32, rlv.width(), 1,
                )));
                popover.set_position(gtk::PositionType::Right);
                popover.popup();

                let unread_count = window.imp().room_list_view
                    .imp().room_registry.borrow()
                    .get(&room_id)
                    .map(|o| o.unread_count())
                    .unwrap_or(0);

                let tx = cmd_tx_h.clone();
                let ollama_cfg = crate::config::settings().ollama;
                glib::spawn_future_local(async move {
                    let _ = tx.send(MatrixCommand::FetchRoomPreview {
                        room_id,
                        unread_count,
                        ollama_endpoint: ollama_cfg.endpoint,
                        ollama_model: ollama_cfg.model,
                        extra_instructions: ollama_cfg.room_preview_extra,
                    }).await;
                });
            });
        }


        // Wire up scroll-to-top → fetch older messages.
        let cmd_tx = command_tx.clone();
        let window_weak = window.downgrade();
        let msg_view_for_scroll = imp.message_view.clone();
        imp.message_view.connect_scroll_top(move || {
            let room_id = window_weak
                .upgrade()
                .and_then(|w| w.imp().current_room_id.borrow().clone());
            let token = msg_view_for_scroll.prev_batch_token();
            if let (Some(room_id), Some(from_token)) = (room_id, token) {
                let tx = cmd_tx.clone();
                glib::spawn_future_local(async move {
                    let _ = tx.send(MatrixCommand::FetchOlderMessages { room_id, from_token }).await;
                });
            }
        });

        // Tail-refresh: user scrolled back to bottom after backpagination evicted
        // the newest messages.  Re-select the current room to trigger bg_refresh.
        let cmd_tx = command_tx.clone();
        let window_weak = window.downgrade();
        imp.message_view.connect_scroll_bottom(move || {
            let Some(win) = window_weak.upgrade() else { return };
            let room_id = win.imp().current_room_id.borrow().clone();
            let Some(room_id) = room_id else { return };
            let known_unread = win.imp().room_list_view.imp().room_registry.borrow()
                .get(&room_id).map(|o| o.unread_count()).unwrap_or(0);
            let tx = cmd_tx.clone();
            glib::spawn_future_local(async move {
                let _ = tx.send(MatrixCommand::SelectRoom { room_id, known_unread }).await;
            });
        });

        // Seek-cancelled: the live store was never modified (GtkStack page switch),
        // so no refresh is needed — live messages are already intact.

        // Wire up send message → send SendMessage command.
        let cmd_tx = command_tx.clone();
        let window_weak = window.downgrade();
        let msg_view_for_send = imp.message_view.clone();
        // Wire up delete → redact command.
        let cmd_tx_delete = command_tx.clone();
        let window_weak_del = window.downgrade();
        let toast_del = imp.toast_overlay.clone();
        let msg_view_del = imp.message_view.clone();
        imp.message_view.connect_delete(move |event_id| {
            let room_id = window_weak_del
                .upgrade()
                .and_then(|w| w.imp().current_room_id.borrow().clone());
            if let Some(room_id) = room_id {
                // Remove from timeline immediately.
                msg_view_del.remove_message(&event_id);
                toast(&toast_del, "Message deleted");
                let tx = cmd_tx_delete.clone();
                glib::spawn_future_local(async move {
                    let _ = tx.send(MatrixCommand::RedactMessage { room_id, event_id }).await;
                });
            }
        });

        // Wire up edit → send replacement.
        let cmd_tx_edit = command_tx.clone();
        let window_weak_edit = window.downgrade();
        imp.message_view.connect_edit(move |event_id, new_body| {
            let room_id = window_weak_edit
                .upgrade()
                .and_then(|w| w.imp().current_room_id.borrow().clone());
            if let Some(room_id) = room_id {
                let tx = cmd_tx_edit.clone();
                glib::spawn_future_local(async move {
                    let _ = tx.send(MatrixCommand::EditMessage { room_id, event_id, new_body, new_formatted_body: None }).await;
                });
            }
        });

        let cmd_tx_edit2 = command_tx.clone();
        imp.message_view.connect_send_message(move |body, reply_to, quote_text, formatted_body, mentioned_user_ids| {
            let room_id = window_weak
                .upgrade()
                .and_then(|w| w.imp().current_room_id.borrow().clone());
            if let Some(room_id) = room_id {
                // Check if this is an edit (reply_to starts with "edit:").
                if let Some(ref rt) = reply_to {
                    if let Some(event_id) = rt.strip_prefix("edit:") {
                        // Update locally immediately using the already-processed
                        // formatted_body (which has mention pills from prepare_send).
                        let edit_preview = match body.char_indices().nth(50) {
                            Some((i, _)) => &body[..i],
                            None => &body,
                        };
                        tracing::info!("Editing message {} with new body: {}", event_id, edit_preview);
                        let fmt_ref = formatted_body.as_deref();
                        msg_view_for_send.update_message_body(event_id, &body, fmt_ref);

                        let tx = cmd_tx_edit2.clone();
                        let eid = event_id.to_string();
                        let new_body = body.clone();
                        let new_fmt = formatted_body.clone();
                        let rid = room_id.clone();
                        glib::spawn_future_local(async move {
                            let _ = tx.send(MatrixCommand::EditMessage {
                                room_id: rid, event_id: eid, new_body, new_formatted_body: new_fmt,
                            }).await;
                        });
                        return;
                    }
                }

                // Detect /me and : emote prefixes — strip them before sending.
                let (send_body, is_emote) =
                    if let Some(rest) = body.strip_prefix("/me ").or_else(|| body.strip_prefix(": ")) {
                        (rest.to_string(), true)
                    } else if body.starts_with(':') && body.len() > 1 && !body.starts_with("::") {
                        (body[1..].trim_start().to_string(), true)
                    } else {
                        (body.clone(), false)
                    };
                let echo_body = if is_emote {
                    format!("* {send_body}")
                } else {
                    send_body.clone()
                };

                // Local echo — show the message immediately.
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let my_id = window_weak.upgrade()
                    .map(|w| w.imp().user_id.borrow().clone())
                    .unwrap_or_default();
                let echo = crate::matrix::MessageInfo {
                    sender: "You".to_string(),
                    sender_id: my_id,
                    body: echo_body,
                    formatted_body: if is_emote { None } else { formatted_body.clone() },
                    timestamp: now,
                    event_id: String::new(),
                    reply_to: reply_to.clone(),
                    reply_to_sender: quote_text.as_ref().map(|(sender, _)| sender.clone()),
                    thread_root: None,
                    reactions: Vec::new(),
                    media: None,
                    is_highlight: false,
                    is_system_event: false,
                };
                // Sending a message means the user has read everything — clear
                // the divider so their own echo doesn't appear below it.
                msg_view_for_send.dismiss_unread();
                msg_view_for_send.append_message(&echo, false);

                let tx = cmd_tx.clone();
                let send_formatted = if is_emote { None } else { formatted_body.clone() };
                let send_mentions = if is_emote { Vec::new() } else { mentioned_user_ids.clone() };
                glib::spawn_future_local(async move {
                    let _ = tx.send(MatrixCommand::SendMessage {
                        room_id,
                        body: send_body,
                        formatted_body: send_formatted,
                        reply_to,
                        quote_text,
                        is_emote,
                        mentioned_user_ids: send_mentions,
                    }).await;
                });
            }
        });

        // Wire up react callback → send reaction command + local update.
        let cmd_tx_react = command_tx.clone();
        let window_weak_react = window.downgrade();
        let msg_view_react = imp.message_view.clone();
        imp.message_view.connect_react(move |event_id, emoji| {
            let room_id = window_weak_react
                .upgrade()
                .and_then(|w| w.imp().current_room_id.borrow().clone());
            if let Some(room_id) = room_id {
                // Toggle locally — checks if "You" already reacted.
                msg_view_react.toggle_reaction(&event_id, &emoji);

                let tx = cmd_tx_react.clone();
                glib::spawn_future_local(async move {
                    let _ = tx.send(MatrixCommand::SendReaction { room_id, event_id, emoji }).await;
                });
            }
        });

        // Wire up attach button → upload and send media.
        let cmd_tx_attach = command_tx.clone();
        let window_weak_attach = window.downgrade();
        let toast_attach = imp.toast_overlay.clone();
        let msg_view_attach = imp.message_view.clone();
        imp.message_view.connect_attach(move |file_path| {
            let room_id = window_weak_attach
                .upgrade()
                .and_then(|w| w.imp().current_room_id.borrow().clone());
            if let Some(room_id) = room_id {
                let path = std::path::Path::new(&file_path);
                let filename = path
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default();

                // Detect media kind from extension.
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                let kind = match ext.to_lowercase().as_str() {
                    "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => crate::matrix::MediaKind::Image,
                    "mp4" | "webm" | "mov" => crate::matrix::MediaKind::Video,
                    "mp3" | "ogg" | "wav" | "flac" => crate::matrix::MediaKind::Audio,
                    _ => crate::matrix::MediaKind::File,
                };
                let size = std::fs::metadata(&file_path).ok().map(|m| m.len());

                // Local echo — show the media message immediately.
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let my_id = window_weak_attach.upgrade()
                    .map(|w| w.imp().user_id.borrow().clone())
                    .unwrap_or_default();
                let echo = crate::matrix::MessageInfo {
                    sender: "You".to_string(),
                    sender_id: my_id,
                    body: filename.clone(),
                    formatted_body: None,
                    timestamp: now,
                    event_id: String::new(),
                    reply_to: None,
                    thread_root: None,
                    reply_to_sender: None,
                    reactions: Vec::new(),
                    media: Some(crate::matrix::MediaInfo {
                        kind,
                        filename: filename.clone(),
                        size,
                        url: format!("file://{file_path}"),
                        source_json: String::new(),
                    }),
                    is_highlight: false,
                    is_system_event: false,
                };
                msg_view_attach.dismiss_unread();
                msg_view_attach.append_message(&echo, false);

                toast(&toast_attach, &format!("Sending {filename}…"));
                let tx = cmd_tx_attach.clone();
                glib::spawn_future_local(async move {
                    let _ = tx.send(MatrixCommand::SendMedia { room_id, file_path }).await;
                });
            }
        });

        // Wire up media click → download and open with system viewer.
        let cmd_tx_media = command_tx.clone();
        let window_weak_media = window.downgrade();
        imp.message_view.connect_media_click(move |url, filename, source_json| {
            // Local file:// URLs — open directly.
            if let Some(path) = url.strip_prefix("file://") {
                show_media_preview(
                    &window_weak_media.upgrade().unwrap(),
                    &window_weak_media.upgrade().unwrap().upcast_ref::<gtk::Widget>(),
                    path,
                );
                return;
            }

            // Check cache.
            if let Some(win) = window_weak_media.upgrade() {
                let cached = win.imp().media_cache.borrow().get(&url).cloned();
                if let Some(path) = cached {
                    show_media_preview(&win, win.upcast_ref::<gtk::Widget>(), &path);
                    return;
                }
            }

            let tx = cmd_tx_media.clone();
            glib::spawn_future_local(async move {
                let _ = tx.send(MatrixCommand::DownloadMedia {
                    url,
                    filename,
                    source_json,
                }).await;
            });
        });

        // DM button — confirm before opening or creating a DM.
        let cmd_tx_dm = command_tx.clone();
        let window_weak_dm = window.downgrade();
        imp.message_view.connect_dm(move |user_id| {
            let Some(win) = window_weak_dm.upgrade() else { return };
            // Show the localpart (@alice:server → "alice") in the heading.
            let display = user_id
                .split(':').next().unwrap_or(&user_id)
                .trim_start_matches('@');
            let dialog = adw::AlertDialog::builder()
                .heading(&format!("Start DM with {display}?"))
                .body("This will open or create a direct message conversation.")
                .build();
            dialog.add_response("cancel", "Cancel");
            dialog.add_response("start", "Start DM");
            dialog.set_response_appearance("start", adw::ResponseAppearance::Suggested);
            dialog.set_default_response(Some("cancel"));
            let tx = cmd_tx_dm.clone();
            let uid = user_id.clone();
            dialog.connect_response(None, move |_, response| {
                if response == "start" {
                    let tx2 = tx.clone();
                    let uid2 = uid.clone();
                    glib::spawn_future_local(async move {
                        let _ = tx2.send(MatrixCommand::CreateDm { user_id: uid2 }).await;
                    });
                }
            });
            dialog.present(Some(&win));
        });

        // Typing indicator — send typing notice when user types in the input.
        let cmd_tx_typing = command_tx.clone();
        let window_weak_typing = window.downgrade();
        imp.message_view.connect_typing(move |typing| {
            if let Some(win) = window_weak_typing.upgrade() {
                if let Some(rid) = win.imp().current_room_id.borrow().clone() {
                    let tx = cmd_tx_typing.clone();
                    glib::spawn_future_local(async move {
                        let _ = tx.send(MatrixCommand::TypingNotice {
                            room_id: rid,
                            typing,
                        }).await;
                    });
                }
            }
        });

        // Initialise the NotificationManager now that we have the window reference.
        imp.notification_manager.set_banner(imp.notify_banner.clone());
        imp.notification_manager.set_window(
            glib::object::ObjectExt::downgrade(
                window.upcast_ref::<gtk::Window>()
            )
        );


        // Focus change handler — clear unseen counter when window regains focus
        // and drain any bg_refresh that the idle deferred during hover.
        let _window_weak_focus = window.downgrade();
        let room_list_focus = imp.room_list_view.clone();
        let msg_view_focus = imp.message_view.clone();
        let focus_cmd_tx = command_tx.clone();
        window.connect_is_active_notify(move |win| {
            let _active_t0 = std::time::Instant::now();
            let active = win.is_active();
            if active {
                let mut phase_unread = std::time::Duration::ZERO;
                let mut phase_drain = std::time::Duration::ZERO;
                let mut phase_flash = std::time::Duration::ZERO;
                let mut drain_n: usize = 0;

                let count = win.imp().unseen_while_unfocused.get();
                if count > 0 {
                    let t = std::time::Instant::now();
                    win.imp().unseen_while_unfocused.set(0);
                    // Clear the badge and remove divider lines.
                    if let Some(rid) = win.imp().current_room_id.borrow().clone() {
                        room_list_focus.clear_unread(&rid);
                        win.imp().local_unread.mark_read(&rid);
                    }
                    msg_view_focus.remove_dividers();
                    phase_unread = t.elapsed();
                }
                // Drain any bg_refresh deferred by the idle (hover, not focus).
                let rid = win.imp().current_room_id.borrow().clone();
                if let Some(ref rid) = rid {
                    let pending = win.imp().pending_bg_refresh.borrow_mut().remove(rid);
                    if let Some((msgs, token)) = pending {
                        drain_n = msgs.len();
                        let t = std::time::Instant::now();
                        msg_view_focus.set_messages(&msgs, token);
                        phase_drain = t.elapsed();
                        if let Some(eid) = win.imp().pending_flash_event_id.take() {
                            let tf = std::time::Instant::now();
                            let mv_ref = msg_view_focus.clone();
                            let rid_clone = rid.clone();
                            let tx = focus_cmd_tx.clone();
                            let found = mv_ref.scroll_to_event(&eid);
                            if !found {
                                mv_ref.start_seek_loading();
                                glib::spawn_future_local(async move {
                                    let _ = tx.send(MatrixCommand::SeekToEvent {
                                        room_id: rid_clone,
                                        event_id: eid,
                                    }).await;
                                });
                            }
                            phase_flash = tf.elapsed();
                        }
                    }
                }
                tracing::info!(
                    "is_active_notify(active=true) total={:?} unread={:?} drain={:?} (n={drain_n}) flash={:?}",
                    _active_t0.elapsed(), phase_unread, phase_drain, phase_flash
                );
            } else {
                tracing::info!("is_active_notify(active=false) total={:?}", _active_t0.elapsed());
            }
        });

        // "Jump" button on the mention banner — navigate to the mentioned room.
        {
            let window_weak = window.downgrade();
            imp.notify_banner.connect_button_clicked(move |_| {
                let Some(win) = window_weak.upgrade() else { return };
                let room_id = win.imp().notification_manager.banner_room_id();
                if let Some(rid) = room_id {
                    let reg = win.imp().room_list_view.imp().room_registry.borrow();
                    if let Some(obj) = reg.get(&rid) {
                        let name = obj.name();
                        drop(reg);
                        if let Some(ref cb) = *win.imp().room_list_view.imp().on_room_selected.borrow() {
                            cb(rid, name);
                        }
                    }
                }
                win.imp().notification_manager.dismiss_banner();
            });
        }

        // Thread icon click — open thread in sidebar.
        let cmd_tx_thread = command_tx.clone();
        let window_weak_thread = window.downgrade();
        imp.message_view.connect_open_thread(move |thread_root_id| {
            if let Some(win) = window_weak_thread.upgrade() {
                let room_id = win.imp().current_room_id.borrow().clone();
                if let Some(rid) = room_id {
                    let tx = cmd_tx_thread.clone();
                    let root_id = thread_root_id.clone();
                    glib::spawn_future_local(async move {
                        let _ = tx.send(MatrixCommand::FetchThreadReplies {
                            room_id: rid,
                            thread_root_id: root_id,
                        }).await;
                    });
                }
            }
        });

        // Event loop.
        let toast_overlay = imp.toast_overlay.clone();
        let login_page = imp.login_page.clone();
        let room_list_view = imp.room_list_view.clone();
        let message_view = imp.message_view.clone();

        let window_weak = window.downgrade();
        glib::spawn_future_local(async move {
            // Count events processed since the last explicit GLib yield.
            // When the channel has a backlog (e.g. dozens of sync events queued
            // while the window was unfocused), recv().await returns immediately
            // each time, so this loop runs in a tight async spin without ever
            // letting GLib flush pending GDK input events.
            //
            // yield_now() every N events gives GLib a chance to service input
            // (pointer, keyboard, focus events) before we process the next batch.
            //
            // IMPORTANT: we schedule the waker via glib::idle_add_local_once
            // (DEFAULT_IDLE priority = 200) rather than wake_by_ref() (DEFAULT
            // priority = 0).  glib::spawn_future_local runs at DEFAULT (0),
            // the same priority as GDK's Wayland event source.  If we wake the
            // future immediately via wake_by_ref(), GLib picks between us and GDK
            // at the same priority — and the future often wins, so pointer events
            // are never processed during the drain.
            //
            // By waking at DEFAULT_IDLE we ensure all DEFAULT-priority sources
            // (GDK pointer/keyboard events, timers) run before we resume, making
            // the app fully responsive to hover/click during a backlog drain.
            let mut n_since_yield: u32 = 0;
            while let Ok(event) = event_rx.recv().await {
                let Some(window) = window_weak.upgrade() else {
                    break;
                };
                n_since_yield += 1;
                if n_since_yield >= 4 {
                    n_since_yield = 0;
                    // Yield to GLib at DEFAULT_IDLE priority so GDK events
                    // (pointer, keyboard, focus) run before the next batch.
                    let mut yielded = false;
                    futures_util::future::poll_fn(|cx| {
                        if yielded {
                            std::task::Poll::Ready(())
                        } else {
                            yielded = true;
                            let waker = cx.waker().clone();
                            glib::idle_add_local_once(move || {
                                waker.wake();
                            });
                            std::task::Poll::Pending
                        }
                    })
                    .await;
                    // Re-check window after yield — it could have been destroyed
                    // while GLib was running other callbacks.
                    if window_weak.upgrade().is_none() { break; }
                }
                match event {
                    MatrixEvent::MarkupRendered { id, markup } => {
                        // Background parser delivered Pango markup for a
                        // MessageObject. Apply to the tracked WeakRef;
                        // no-op if the object was dropped (e.g. list_store
                        // swapped away) before we got here.
                        crate::markup_worker::apply_result(id, markup);
                    }
                    MatrixEvent::LoginRequired => {
                        if crate::config::gsettings().boolean("first-start") {
                            window.show_onboarding();
                        } else {
                            window.show_login();
                        }
                    }
                    MatrixEvent::LoginSuccess { display_name, user_id, from_registration, is_fresh_login } => {
                        // Clear first-start flag now that login succeeded.
                        let _ = crate::config::gsettings().set_boolean("first-start", false);
                        login_page.stop_spinner();
                        login_page.stop_register_spinner();
                        tracing::info!("Logged in as {display_name} (from_registration={from_registration}, fresh={is_fresh_login})");
                        window.imp().user_id.replace(user_id.clone());
                        message_view.set_user_id(&user_id);
                        let localpart = user_id
                            .strip_prefix('@')
                            .and_then(|s| s.split(':').next())
                            .unwrap_or("")
                            .to_string();
                        let mut names = vec![display_name.as_str()];
                        if !localpart.is_empty() && localpart != display_name {
                            names.push(&localpart);
                        }
                        message_view.set_highlight_names(&names);

                        if from_registration {
                            // Stay on the login page — the recovery key and
                            // room suggestions wizard pages will follow via
                            // RecoveryKeyGenerated and connect_join_rooms.
                        } else if is_fresh_login {
                            // Fresh interactive login — show verify-device page
                            // so the user knows about encrypted rooms.
                            login_page.show_verify_device();
                        } else {
                            // Restored session — go straight to the app.
                            let msg = format!("Logged in as {display_name}");
                            toast(&toast_overlay, &msg);
                            window.show_main_view();
                            // Show first-run AI setup only if AI is enabled and setup not yet done.
                            let cfg = crate::config::settings();
                            if cfg.ollama.enabled && !cfg.ollama.setup_done {
                                let w = window.clone();
                                glib::timeout_add_local_once(
                                    std::time::Duration::from_millis(800),
                                    move || show_ai_setup_dialog(&w),
                                );
                            }
                        }
                    }
                    MatrixEvent::LoginFailed { error } => {
                        toast_error(&toast_overlay, "Login failed", &error);
                        login_page.stop_spinner();
                        login_page.set_sensitive(true);
                        window.show_login();
                    }
                    MatrixEvent::SyncStarted => {
                        tracing::info!("Initial sync started…");
                        // Reset catchup guard — suppress notifications until first room list arrives.
                        window.imp().initial_sync_done.set(false);
                        // Preload the Ollama model in the background so the
                        // first Ctrl+click doesn't pay the full load penalty.
                        let ollama_cfg = crate::config::settings().ollama;
                        if ollama_cfg.enabled && !ollama_cfg.endpoint.is_empty() && !ollama_cfg.model.is_empty() {
                            let tx = command_tx.clone();
                            glib::spawn_future_local(async move {
                                let _ = tx.send(crate::matrix::MatrixCommand::WarmupOllama {
                                    endpoint: ollama_cfg.endpoint,
                                    model: ollama_cfg.model,
                                }).await;
                            });
                        }
                    }
                    MatrixEvent::SyncError { error } => {
                        tracing::error!("Sync error: {error}");
                        toast_error(&toast_overlay, "Sync error", &error);
                    }
                    MatrixEvent::RoomListUpdated { rooms } => {
                        window.imp().initial_sync_done.set(true);
                        // Immediately patch the current room's GObject so its
                        // sidebar badge reflects the new state before the idle fires.
                        let current_rid = window.imp().current_room_id.borrow().clone();
                        if let Some(ref rid) = current_rid {
                            if let Some(info) = rooms.iter().find(|r| &r.room_id == rid) {
                                room_list_view.patch_room(info);
                            }
                        }
                        // Coalesce: later snapshots replace earlier ones so the idle
                        // always processes the most recent state.
                        window.imp().pending_rooms.replace(Some(rooms));
                        // Schedule exactly one idle per pending batch — the flag
                        // prevents stacking when events arrive faster than the idle runs.
                        if !window.imp().rooms_idle_pending.get() {
                            window.imp().rooms_idle_pending.set(true);
                            let win2 = window.downgrade();
                            let rlv2 = room_list_view.clone();
                            glib::idle_add_local_once(move || {
                                let Some(win) = win2.upgrade() else { return };
                                let imp = win.imp();
                                imp.rooms_idle_pending.set(false);
                                let Some(rooms) = imp.pending_rooms.borrow_mut().take() else { return };
                                tracing::info!("rooms idle fired: {} rooms", rooms.len());
                                // Detect rooms read on another client BEFORE update_rooms
                                // overwrites prev_server_counts.
                                let cross_reads = rlv2.detect_cross_client_reads(&rooms);
                                rlv2.update_rooms(&rooms);
                                for room_id in cross_reads {
                                    imp.local_unread.mark_read_elsewhere(&room_id);
                                }
                                // Re-apply broker counts as a floor — server sync may
                                // have zeroed rooms the user hasn't visited locally.
                                imp.local_unread.for_each_nonzero(|room_id, unread, highlights| {
                                    rlv2.set_room_unread_counts(room_id, unread, highlights);
                                });
                                // Keep favourite rooms in the bookmarks overview in sync.
                                // Tombstoned rooms are excluded: they're archived upgrades,
                                // produce same-name duplicates next to the replacement room,
                                // and some homeservers propagate tag changes across the
                                // upgrade link — the user sees "remove one, both disappear"
                                // which is neither predictable nor useful.
                                {
                                    let registry = rlv2.imp().room_registry.borrow();
                                    let mut favs: Vec<crate::models::RoomObject> = rooms.iter()
                                        .filter(|r| r.is_favourite && !r.is_tombstoned)
                                        .filter_map(|r| registry.get(&r.room_id).cloned())
                                        .collect();
                                    favs.sort_by(|a, b| b.last_activity_ts().cmp(&a.last_activity_ts()));
                                    imp.bookmarks_overview.set_favourite_rooms(&favs);
                                }
                                // Request avatar downloads for any new rooms with avatars.
                                let to_fetch = {
                                    let mut requested = imp.requested_room_avatars.borrow_mut();
                                    rlv2.rooms_needing_avatars()
                                        .into_iter()
                                        .filter(|(id, _)| requested.insert(id.clone()))
                                        .collect::<Vec<_>>()
                                };
                                for (room_id, mxc_url) in to_fetch {
                                    if let Some(tx) = imp.command_tx.get() {
                                        let tx = tx.clone();
                                        glib::spawn_future_local(async move {
                                            let _ = tx.send(crate::matrix::MatrixCommand::FetchRoomAvatar {
                                                room_id, mxc_url,
                                            }).await;
                                        });
                                    }
                                }
                            });
                        }
                    }
                    MatrixEvent::BgRefreshStarted { room_id } => {
                        let current = window.imp().current_room_id.borrow().clone();
                        if current.as_deref() == Some(&room_id) {
                            message_view.set_refreshing(true);
                        }
                    }
                    MatrixEvent::RoomMessages { room_id, messages, prev_batch_token, room_meta, is_background } => {
                        let current = window.imp().current_room_id.borrow().clone();
                        if current.as_deref() == Some(&room_id) {
                            // Fresh data arrived — hide the loading bar (if shown for stale cache).
                            message_view.set_refreshing(false);
                            window.imp().current_room_meta.replace(Some(room_meta.clone()));
                            // MOTD: clear changed icon and record the current topic as seen.
                            // Offloaded to idle so the synchronous disk I/O (load+write)
                            // doesn't block the GTK main loop during room switches.
                            #[cfg(feature = "motd")]
                            {
                                room_list_view.set_topic_changed(&room_id, false);
                                if !room_meta.topic.is_empty() {
                                    let motd_rid = room_id.clone();
                                    let motd_topic = room_meta.topic.clone();
                                    glib::idle_add_local_once(move || {
                                        let mut cache = crate::plugins::motd::load();
                                        crate::plugins::motd::mark_seen(&motd_rid, &motd_topic, &mut cache);
                                    });
                                }
                            }
                            // DM status and no-media flag don't change during a bg_refresh —
                            // skip the DConf reads rebuild_row_context_cache() triggers for
                            // these setters.  Only re-check on explicit room selection.
                            if !is_background {
                                let is_dm = {
                                    let reg = room_list_view.imp().room_registry.borrow();
                                    reg.get(&room_id).map(|o| o.kind() == crate::matrix::RoomKind::DirectMessage).unwrap_or(false)
                                };
                                message_view.set_is_dm_room(is_dm);
                                message_view.set_no_media(room_list_view.resolve_no_media(&room_id));
                            }
                            message_view.set_room_meta(&room_meta);
                            // Update bookmark button icon.
                            if let Some(btn) = window.imp().bookmark_button.get() {
                                btn.set_icon_name(if room_meta.is_favourite {
                                    "starred-symbolic"
                                } else {
                                    "non-starred-symbolic"
                                });
                                btn.set_tooltip_text(Some(if room_meta.is_favourite {
                                    "Remove bookmark"
                                } else {
                                    "Bookmark this room"
                                }));
                            }
                            // Queue avatar fetches only for senders of the displayed
                            // messages — not all room members (which can be thousands).
                            // Done before set_messages so messages isn't moved yet.
                            let avatar_lookup: std::collections::HashMap<&str, &str> =
                                room_meta.member_avatars.iter()
                                    .map(|(uid, mxc)| (uid.as_str(), mxc.as_str()))
                                    .collect();
                            let to_fetch: Vec<(String, String)> = {
                                let cached = window.imp().avatar_cache.borrow();
                                messages.iter()
                                    .map(|m| m.sender_id.as_str())
                                    .collect::<std::collections::HashSet<_>>()
                                    .into_iter()
                                    .filter(|uid| !uid.is_empty() && !cached.contains_key(*uid))
                                    .filter_map(|uid| {
                                        avatar_lookup.get(uid)
                                            .filter(|mxc| !mxc.is_empty())
                                            .map(|mxc| (uid.to_string(), mxc.to_string()))
                                    })
                                    .collect()
                            };

                            // For bg_refresh results, defer the splice to an idle so
                            // the GTK frame (and selection highlight) renders first.
                            // Re-check the current room inside the idle: a stale idle
                            // that fires after the user has switched rooms would otherwise
                            // splice the wrong data into the new room's list_store.
                            // Consume any pending flash event_id.  For background
                            // refreshes set_messages is deferred to an idle, so we
                            // must defer the scroll too — otherwise scroll_to_event
                            // fires before event_index is populated and falls through
                            // to SeekToEvent even when the event is in the batch.
                            let pending_flash = window.imp().pending_flash_event_id.take();

                            if is_background {
                                // Coalesce: always store the latest batch, dropping older
                                // ones.  If 10 bg_refresh events arrive while unfocused,
                                // only the last batch fires set_messages — not all 10.
                                let had_pending = bg_refresh_insert(
                                    &mut window.imp().pending_bg_refresh.borrow_mut(),
                                    &room_id,
                                    messages,
                                    prev_batch_token,
                                );
                                if should_process_bg_refresh_sync(
                                    window.is_active(),
                                    message_view.is_loading(),
                                ) {
                                    let Some((msgs, token)) = window.imp()
                                        .pending_bg_refresh.borrow_mut()
                                        .remove(&room_id)
                                    else { return };  // consumed by a concurrent drain
                                    let _t_sm = std::time::Instant::now();
                                    message_view.set_messages(&msgs, token);
                                    tracing::info!(
                                        "bg_refresh set_messages (sync) took {:?} (room={room_id})",
                                        _t_sm.elapsed()
                                    );
                                    if let Some(eid) = pending_flash {
                                        let found = message_view.scroll_to_event(&eid);
                                        if !found {
                                            message_view.start_seek_loading();
                                            let rid = room_id.clone();
                                            let tx = command_tx.clone();
                                            glib::spawn_future_local(async move {
                                                let _ = tx.send(MatrixCommand::SeekToEvent {
                                                    room_id: rid,
                                                    event_id: eid,
                                                }).await;
                                            });
                                        }
                                    }
                                } else if !had_pending {
                                    // Unfocused window: schedule one idle per room.
                                    // notify::is-active drains pending_bg_refresh on focus.
                                    let mv = message_view.clone();
                                    let weak_win = window.downgrade();
                                    let guard_rid = room_id.clone();
                                    let tx_seek = command_tx.clone();
                                    glib::idle_add_local_once(move || {
                                        let Some(win) = weak_win.upgrade() else { return };
                                        let is_active = win.is_active();
                                        tracing::info!(
                                            "bg_refresh idle fired: room={guard_rid} is_active={}",
                                            is_active
                                        );
                                        if !is_active {
                                            // Wayland pointer-enter woke the main loop but the
                                            // window has no keyboard focus.  Defer until focus.
                                            if pending_flash.is_some() {
                                                let mut slot = win.imp()
                                                    .pending_flash_event_id.borrow_mut();
                                                if slot.is_none() { *slot = pending_flash; }
                                            }
                                            return;
                                        }
                                        let Some((msgs, token)) = win.imp()
                                            .pending_bg_refresh.borrow_mut()
                                            .remove(&guard_rid)
                                        else { return };
                                        let still_here = win.imp().current_room_id.borrow()
                                            .as_deref() == Some(&guard_rid);
                                        if still_here {
                                            let _t_sm = std::time::Instant::now();
                                            mv.set_messages(&msgs, token);
                                            tracing::info!(
                                                "bg_refresh set_messages took {:?} (room={guard_rid})",
                                                _t_sm.elapsed()
                                            );
                                        }
                                        if let Some(eid) = pending_flash {
                                            let found = mv.scroll_to_event(&eid);
                                            if !found {
                                                mv.start_seek_loading();
                                                glib::spawn_future_local(async move {
                                                    let _ = tx_seek.send(MatrixCommand::SeekToEvent {
                                                        room_id: guard_rid,
                                                        event_id: eid,
                                                    }).await;
                                                });
                                            }
                                        }
                                    });
                                }
                            } else {
                                message_view.set_messages(&messages, prev_batch_token);
                                // For fresh loads set_messages ran synchronously above,
                                // so event_index is already populated — scroll immediately.
                                if let Some(eid) = pending_flash {
                                    let mv = message_view.clone();
                                    let found = mv.scroll_to_event(&eid);
                                    if !found {
                                        mv.start_seek_loading();
                                        let rid = room_id.clone();
                                        let tx = command_tx.clone();
                                        glib::idle_add_local_once(move || {
                                            glib::spawn_future_local(async move {
                                                let _ = tx.send(MatrixCommand::SeekToEvent {
                                                    room_id: rid,
                                                    event_id: eid,
                                                }).await;
                                            });
                                        });
                                    }
                                }
                            }
                            for (uid, mxc) in to_fetch {
                                let tx = command_tx.clone();
                                glib::spawn_future_local(async move {
                                    let _ = tx.send(MatrixCommand::FetchAvatar {
                                        user_id: uid, mxc_url: mxc,
                                    }).await;
                                });
                            }
                        }
                    }
                    MatrixEvent::AvatarReady { user_id, path } => {
                        window.imp().avatar_cache.borrow_mut().insert(user_id.clone(), path.clone());
                        // Update the visible nick-picker popover in place so
                        // the avatar appears without requiring the user to
                        // close and reopen the picker.
                        window.imp().message_view.refresh_nick_avatar(&user_id, &path);
                    }
                    MatrixEvent::RoomAvatarReady { room_id, path } => {
                        room_list_view.set_room_avatar_path(&room_id, &path);
                    }
                    MatrixEvent::OwnAvatarUpdated { success, new_mxc, error } => {
                        if success {
                            if new_mxc.is_empty() {
                                toast(&toast_overlay, "Profile picture removed");
                            } else {
                                toast(&toast_overlay, "Profile picture updated");
                                // Fetch the new avatar into our local cache so
                                // Preferences re-opens with the fresh image.
                                if let Some(tx) = window.imp().command_tx.get().cloned() {
                                    let uid = window.imp().user_id.borrow().clone();
                                    if !uid.is_empty() {
                                        let mxc = new_mxc;
                                        glib::spawn_future_local(async move {
                                            let _ = tx.send(MatrixCommand::FetchAvatar {
                                                user_id: uid, mxc_url: mxc,
                                            }).await;
                                        });
                                    }
                                }
                            }
                        } else {
                            toast_error(&toast_overlay, "Could not update avatar", &error);
                        }
                    }
                    MatrixEvent::OlderMessages { room_id, messages, prev_batch_token } => {
                        let current = window.imp().current_room_id.borrow().clone();
                        if current.as_deref() == Some(&room_id) {
                            message_view.prepend_messages(&messages, prev_batch_token);
                        }
                    }
                    MatrixEvent::SeekResult { room_id, target_event_id, messages, before_token } => {
                        let current = window.imp().current_room_id.borrow().clone();
                        if current.as_deref() == Some(&room_id) {
                            message_view.load_seek_result(&messages, &target_event_id, before_token);
                        }
                    }
                    MatrixEvent::MessageSent { room_id, echo_body, event_id } => {
                        let current = window.imp().current_room_id.borrow().clone();
                        if current.as_deref() == Some(&room_id) {
                            message_view.patch_echo_event_id(&echo_body, &event_id);
                            message_view.update_history_event_id(&echo_body, &event_id);
                        }
                        // Bubble the room to the top immediately — don't wait up to
                        // 3 minutes for the next RoomListUpdated to re-sort.
                        let now_secs = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        room_list_view.bump_room_activity(&room_id, now_secs);
                    }
                    MatrixEvent::NewMessage { room_id, room_name, sender_id, message, is_mention, is_dm } => {
                        let current = window.imp().current_room_id.borrow().clone();
                        let my_id = window.imp().user_id.borrow().clone();
                        // Guard: only treat as self-message when user_id is known.
                        let is_self = !my_id.is_empty() && sender_id == my_id;
                        let is_current_room = current.as_deref() == Some(&room_id);
                        let window_focused = window.is_active();
                        tracing::debug!(
                            "UI NewMessage room={room_id} is_dm={is_dm} is_self={is_self} is_current={is_current_room}"
                        );
                        // Diagnostic: log self-message detection for current room so we can
                        // tell from INFO logs whether is_self is correct when echoes appear.
                        if is_current_room && !message.is_system_event {
                            tracing::info!(
                                "NewMessage current: sender={sender_id} is_self={is_self} my_id_known={} event_id={}",
                                !my_id.is_empty(), message.event_id
                            );
                        }

                        // System events (join/leave/kick/ban) are shown inline
                        // but never count as unread messages or trigger the divider.
                        let is_system = message.is_system_event;

                        if is_current_room && !is_self {
                            // Insert a "New messages" divider before the first
                            // unseen message when the window is unfocused.
                            let unfocused = !window_focused;
                            if !is_system && unfocused && window.imp().unseen_while_unfocused.get() == 0 {
                                message_view.insert_divider();
                            }
                            // Tint the row blue when the window is not focused and
                            // it's a real message (system events shown without tint).
                            message_view.append_message(&message, unfocused && !is_system);
                            if !is_system && unfocused {
                                let count = window.imp().unseen_while_unfocused.get() + 1;
                                window.imp().unseen_while_unfocused.set(count);
                                // Keep the banner title in sync with the live count.
                                message_view.set_unseen_count(count);
                                // Also show unread badge since user hasn't seen these.
                                window.imp().local_unread.increment(&room_id, is_mention);
                            }
                        } else if is_current_room && is_self && !message_view.has_event(&message.event_id) {
                            // Sync arrived before MessageSent — patch the local echo
                            // if one exists (prevents duplicates). Only append if no
                            // unpatched echo is found (e.g. echo was already spliced out).
                            tracing::info!("NewMessage self-message: body={:?} event_id={} — attempting patch", message.body.char_indices().nth(40).map(|(i,_)|&message.body[..i]).unwrap_or(&message.body), message.event_id);
                            if !message_view.patch_echo_event_id(&message.body, &message.event_id) {
                                tracing::warn!("NewMessage self-message: patch failed, appending as new — possible duplicate!");
                                message_view.append_message(&message, false);
                            }
                        } else if is_current_room && is_self {
                            tracing::debug!("NewMessage self-message: already in event_index, skipping event_id={}", message.event_id);
                        }

                        // Update unread badge on rooms we're NOT viewing.
                        // Route through the local broker so counts persist across restarts.
                        // System events (join/leave) are not counted as unread.
                        if !is_system && !is_current_room && !is_self {
                            window.imp().local_unread.increment(&room_id, is_mention);
                        }

                        // Delegate all in-app banner + desktop notification logic
                        // to the NotificationManager, which checks UX state internally.
                        // Skip notifications until the first room list arrives — those
                        // are historical catchup messages the user has already seen.
                        let sync_done = window.imp().initial_sync_done.get();
                        // Suppress all notifications (banner, desktop, bell log)
                        // for the room the user is currently viewing.
                        // notification_manager.push also checks this internally,
                        // but push_notification (bell log) does not.
                        if !is_self && sync_done && !is_current_room && (is_mention || is_dm) {
                            window.imp().notification_manager.push(
                                &room_id,
                                &room_name,
                                &message.sender,
                                &message.body,
                                is_dm,
                            );
                            // In-session notification log (bell icon).
                            window.push_notification(
                                &room_id,
                                &message.event_id,
                                &message.sender,
                                &room_name,
                                &message.body,
                                message.timestamp,
                            );
                        }
                        // Bubble the room to the top of the list immediately —
                        // but only when the window is active.  When unfocused,
                        // room ordering is invisible to the user; the next
                        // update_rooms call (from the 50 ms ticker after a
                        // RoomListUpdated arrives) will sort rooms correctly.
                        // Skipping this avoids O(rooms) scans and rebuild_stores
                        // calls for every NewMessage during a backlog drain.
                        if message.timestamp > 0 && window_focused {
                            room_list_view.bump_room_activity(&room_id, message.timestamp);
                        }
                    }
                    #[cfg(feature = "community-health")]
                    MatrixEvent::HealthUpdate { room_id, score: _, trend: _, alert } => {
                        use crate::plugins::community_health::AlertLevel;
                        let alert_code: u8 = match alert {
                            AlertLevel::None    => 1,
                            AlertLevel::Watch   => 2,
                            AlertLevel::Warning => 3,
                        };
                        window.imp().room_list_view.set_room_health(&room_id, alert_code);
                        // Toast only on new warnings to avoid spamming.
                        if alert == AlertLevel::Warning {
                            toast_or_notify(
                                &window,
                                &toast_overlay,
                                &format!("health-{room_id}"),
                                "Community health warning",
                                &format!("Sustained tension detected in {room_id}"),
                            );
                        }
                    }
                    MatrixEvent::ReactionUpdate { room_id, event_id, reactions } => {
                        let current = window.imp().current_room_id.borrow().clone();
                        tracing::debug!("ReactionUpdate: room={room_id} current={current:?} target={event_id}");
                        if current.as_deref() == Some(&room_id) {
                            for (emoji, _count, senders) in &reactions {
                                for sender in senders {
                                    message_view.add_reaction(&event_id, emoji, sender);
                                }
                            }
                        } else {
                            tracing::debug!("ReactionUpdate skipped: not in that room");
                        }
                    }
                    MatrixEvent::ReactionNotification { room_id, room_name, reactor, emoji } => {
                        toast_or_notify(
                            &window,
                            &toast_overlay,
                            &format!("reaction-{room_id}"),
                            "New reaction",
                            &format!("{reactor} reacted {emoji} to your message in {room_name}"),
                        );
                    }
                    MatrixEvent::MessageEdited { room_id, event_id, new_body, formatted_body } => {
                        let current = window.imp().current_room_id.borrow().clone();
                        if current.as_deref() == Some(&room_id) {
                            message_view.update_message_body(&event_id, &new_body, formatted_body.as_deref());
                        }
                    }
                    MatrixEvent::MessageRedacted { room_id, event_id } => {
                        let current = window.imp().current_room_id.borrow().clone();
                        if current.as_deref() == Some(&room_id) {
                            message_view.remove_message(&event_id);
                        }
                    }
                    MatrixEvent::VerificationRequest {
                        flow_id,
                        other_user,
                        other_device,
                    } => {
                        let tx = window.imp().command_tx.get().unwrap().clone();
                        crate::widgets::verification_dialog::show_verification_request(
                            &window, &flow_id, &other_user, &other_device, tx,
                        );
                    }
                    MatrixEvent::VerificationEmojis { flow_id, emojis } => {
                        // Dismiss the waiting dialog if open.
                        if let Some(dialog) = window.imp().verify_dialog.take() {
                            dialog.force_close();
                        }
                        let pairs: Vec<(String, String)> = emojis
                            .into_iter()
                            .map(|e| (e.symbol, e.description))
                            .collect();
                        let tx = window.imp().command_tx.get().unwrap().clone();
                        crate::widgets::verification_dialog::show_verification_emojis(
                            &window, &flow_id, &pairs, tx,
                        );
                    }
                    MatrixEvent::VerificationDone { .. } => {
                        if let Some(dialog) = window.imp().verify_dialog.take() {
                            dialog.force_close();
                        }
                        window.imp().verify_banner.set_revealed(false);
                        let toast = adw::Toast::builder()
                            .title("Device verified! To decrypt your full message history, use Recover Keys.")
                            .button_label("Recover Keys")
                            .action_name("win.recover-keys")
                            .timeout(30)
                            .build();
                        toast_overlay.add_toast(toast);
                    }
                    MatrixEvent::VerificationCancelled { reason, .. } => {
                        if let Some(dialog) = window.imp().verify_dialog.take() {
                            dialog.force_close();
                        }
                        // Re-show the banner so user can try again.
                        window.imp().verify_banner.set_revealed(true);
                        toast_error(&toast_overlay, "Verification cancelled", &reason);
                    }
                    MatrixEvent::CrossSigningBootstrapped => {
                        // New account: keys just created.  Backup still needs
                        // connecting, handled by BackupVersionMismatch below.
                        toast(&toast_overlay, "Encryption ready. Use Recover Keys to connect a key backup.");
                    }
                    MatrixEvent::CrossSigningNeedsPassword => {
                        toast_error(
                            &toast_overlay,
                            "Encryption setup incomplete",
                            "Log out and back in to finish setting up encryption.",
                        );
                    }
                    MatrixEvent::DeviceUnverified => {
                        window.imp().verify_banner.set_revealed(true);
                        window.imp().login_page.show_verify_device();
                    }
                    MatrixEvent::RegistrationFailed { error } => {
                        window.imp().login_page.stop_register_spinner();
                        window.imp().login_page.show_register_error(&error);
                    }
                    MatrixEvent::RegistrationSuccess { .. } => {
                        // LoginSuccess is sent by do_register via do_login; this variant
                        // is reserved for future use.
                    }
                    MatrixEvent::RecoveryKeyGenerated { key } => {
                        window.imp().login_page.show_recovery_key(&key);
                    }
                    MatrixEvent::RecoveryStarted => {
                        toast(&toast_overlay, "Recovering keys… this may take up to a minute.");
                    }
                    MatrixEvent::RecoveryComplete { backup_connected } => {
                        if backup_connected {
                            toast(&toast_overlay, "Keys recovered — open each room to see decrypted messages.");
                        } else {
                            // SSSS has a key for an older backup version. The server's
                            // current backup was created without updating SSSS.
                            // The app will attempt to download from the matching backup
                            // version directly. If that fails, contact support.
                            toast_error(
                                &toast_overlay,
                                "Passphrase correct — downloading from backup",
                                "Your passphrase unlocks an older backup. Fetching keys now.",
                            );
                            let tx = window.imp().command_tx.get().unwrap().clone();
                            glib::spawn_future_local(async move {
                                let _ = tx.send(MatrixCommand::DownloadFromSsssBackup).await;
                            });
                        }
                    }
                    MatrixEvent::RecoveryFailed { error } => {
                        toast_error(&toast_overlay, "Key recovery failed", &error);
                    }
                    MatrixEvent::BackupVersionMismatch => {
                        // Suppress during onboarding — content_page is only
                        // set after show_main_view(), so None means we're
                        // still in the wizard/login flow.
                        if window.imp().content_page.get().is_some() {
                            toast_error(
                                &toast_overlay,
                                "Key backup not connected",
                                "Use Recover Keys in the menu to reconnect your key backup and decrypt messages.",
                            );
                        }
                    }
                    MatrixEvent::StaleBackupDeleted => {
                        toast(&toast_overlay, "Stale backup cleared — click Recover Keys once more to connect.");
                    }
                    MatrixEvent::KeysImported { imported, total } => {
                        toast(&toast_overlay, &format!("Imported {imported}/{total} session keys — messages will decrypt now."));
                    }
                    MatrixEvent::KeyImportFailed { error } => {
                        toast_error(&toast_overlay, "Key import failed", &error);
                    }
                    MatrixEvent::MetricsReady { path, event_count, metrics_text } => {
                        let msg = format!("Exported {event_count} events → {path}");
                        toast(&toast_overlay, &msg);
                        let cfg = crate::config::settings();
                        if !cfg.ollama.endpoint.is_empty() && cfg.ollama.enabled {
                            let tx = window.imp().command_tx.get().unwrap().clone();
                            let prompt = build_metrics_prompt(&metrics_text, cfg.ollama.detect_conflict, cfg.ollama.detect_coc);
                            glib::spawn_future_local(async move {
                                let _ = tx.send(MatrixCommand::RunOllamaMetrics {
                                    prompt,
                                    endpoint: cfg.ollama.endpoint,
                                    model: cfg.ollama.model,
                                }).await;
                            });
                        }
                    }
                    MatrixEvent::MetricsFailed { error } => {
                        toast_error(&toast_overlay, "Metrics export failed", &error);
                    }
                    MatrixEvent::MessagesExported { path, count } => {
                        toast(&toast_overlay, &format!("Exported {count} messages → {path}"));
                    }
                    MatrixEvent::MessagesExportFailed { error } => {
                        toast_error(&toast_overlay, "Message export failed", &error);
                    }
                    // RoomPreview is no longer sent — Ollama now runs on the tokio thread
                    // and sends OllamaChunk events directly.
                    MatrixEvent::RoomPreview { .. } => {}
                    MatrixEvent::OllamaChunk { context, chunk, done } => {
                        if let Some(room_id) = context.strip_prefix("preview:") {
                            // Room preview popover — only update if still hovering this room.
                            if window.imp().hover_room_id.borrow().as_deref() != Some(room_id) {
                                // Stale — ignore.
                            } else if done && chunk.is_empty() {
                                // Empty done = final protocol terminator after real content.
                                // Let the user read and close manually — don't auto-dismiss.
                                let has_content = window.imp().hover_popover.child()
                                    .and_then(|w| w.downcast::<gtk::Box>().ok())
                                    .map(|b| b.has_css_class("preview-content"))
                                    .unwrap_or(false);
                                if !has_content {
                                    // Nothing ever rendered — Ollama unavailable or no messages.
                                    // Show a brief error instead of silently dismissing.
                                    if let Some(sid) = window.imp().hover_pulse_timer.borrow_mut().take() {
                                        sid.remove();
                                    }
                                    window.imp().hover_popover.popdown();
                                    window.imp().hover_room_id.borrow_mut().take();
                                }
                            } else {
                                // First chunk: replace loading spinner with content vbox.
                                // Detect by checking for the "preview-content" CSS class.
                                let needs_setup = window.imp().hover_popover.child()
                                    .and_then(|w| w.downcast::<gtk::Box>().ok())
                                    .map(|b| !b.has_css_class("preview-content"))
                                    .unwrap_or(true);
                                if needs_setup {
                                    // Cancel the pulse timer — content is arriving.
                                    if let Some(sid) = window.imp().hover_pulse_timer.borrow_mut().take() {
                                        sid.remove();
                                    }
                                    let vbox = gtk::Box::builder()
                                        .orientation(gtk::Orientation::Vertical)
                                        .spacing(4)
                                        .margin_start(8).margin_end(8)
                                        .css_classes(["preview-content"])
                                        .margin_top(8).margin_bottom(4)
                                        .build();
                                    let label = gtk::Label::builder()
                                        .wrap(true).max_width_chars(40).xalign(0.0)
                                        .build();
                                    label.set_widget_name("preview-label");
                                    let close_btn = gtk::Button::builder()
                                        .label("Close")
                                        .halign(gtk::Align::End)
                                        .css_classes(["flat", "caption"])
                                        .build();
                                    let pop = window.imp().hover_popover.clone();
                                    close_btn.connect_clicked(move |_| pop.popdown());
                                    vbox.append(&label);
                                    vbox.append(&close_btn);
                                    window.imp().hover_popover.set_child(Some(&vbox));
                                }
                                // Append chunk to existing text.
                                if !chunk.is_empty() {
                                    if let Some(child) = window.imp().hover_popover.child() {
                                        if let Some(vbox) = child.downcast_ref::<gtk::Box>() {
                                            if let Some(label) = vbox.first_child()
                                                .and_then(|w| w.downcast::<gtk::Label>().ok())
                                            {
                                                let current = label.text().to_string();
                                                label.set_text(&format!("{current}{chunk}"));
                                            }
                                        }
                                    }
                                }
                            }
                        } else if context == "metrics" {
                            // Metrics summary — accumulate into a dialog.
                            if done {
                                let text = window.imp().metrics_summary_buf.take();
                                if !text.is_empty() {
                                    let dialog = adw::AlertDialog::builder()
                                        .heading("Metrics Summary")
                                        .body(&text)
                                        .build();
                                    dialog.add_response("ok", "OK");
                                    dialog.present(Some(&window));
                                }
                            } else if !chunk.is_empty() {
                                let mut buf = window.imp().metrics_summary_buf.borrow_mut();
                                buf.push_str(&chunk);
                            }
                        }
                    }
                    MatrixEvent::MediaReady { url, path } => {
                        // Cache the downloaded path.  Cap at 200 entries — files
                        // stay on disk so a cache miss just re-populates cheaply.
                        let mut cache = window.imp().media_cache.borrow_mut();
                        if cache.len() >= 200 {
                            cache.clear();
                        }
                        cache.insert(url, path.clone());

                        // Open with system viewer.
                        show_media_preview(&window, window.upcast_ref::<gtk::Widget>(), &path);
                    }
                    MatrixEvent::RoomJoined { room_id, room_name } => {
                        toast(&toast_overlay, &format!("Joined {room_name}"));
                        // Replace the "Joining…" button with a static "Joined" label.
                        if let Some(btn) = window.imp().directory_join_buttons.borrow_mut().remove(&room_id) {
                            if let Some(parent) = btn.parent().and_downcast::<adw::ActionRow>() {
                                parent.remove(&btn);
                                let badge = gtk::Label::builder()
                                    .label("Joined")
                                    .css_classes(["dim-label", "caption"])
                                    .build();
                                parent.add_suffix(&badge);
                            }
                        }
                    }
                    MatrixEvent::JoinFailed { error } => {
                        toast_error(&toast_overlay, "Failed to join", &error);
                        // Re-enable all stuck "Joining…" buttons so the user can retry.
                        for btn in window.imp().directory_join_buttons.borrow().values() {
                            btn.set_sensitive(true);
                            btn.set_label("Join");
                        }
                    }
                    MatrixEvent::PublicRoomDirectory { title, rooms } => {
                        // During onboarding the login page is the active content;
                        // route results to the local-spaces wizard page instead of
                        // the room browser.
                        let login_page_widget: gtk::Widget = window.imp().login_page.clone().upcast();
                        let toolbar_content = window.imp().toolbar.content();
                        let in_onboarding = toolbar_content
                            .as_ref()
                            .map(|w| w == &login_page_widget)
                            .unwrap_or(false);
                        tracing::info!(
                            "PublicRoomDirectory: {} rooms, in_onboarding={in_onboarding}, \
                             toolbar_content={:?}",
                            rooms.len(),
                            toolbar_content.as_ref().map(|w| w.widget_name().to_string()),
                        );
                        if in_onboarding {
                            let items: Vec<(String, String, String)> = rooms.iter()
                                .map(|r| (r.room_id.clone(), r.name.clone(), r.topic.clone()))
                                .collect();
                            window.imp().login_page.show_local_spaces(items);
                        } else {
                            window.show_or_update_directory(&title, &rooms);
                        }
                    }
                    MatrixEvent::SpaceDirectory { space_id, rooms } => {
                        // Route into the expander for this space, or into the
                        // scoped dialog list box if we're in single-space mode.
                        window.populate_space_expander(&space_id, &rooms);
                    }
                    MatrixEvent::PublicSpacesForServer { server, rooms } => {
                        window.add_browsed_spaces_to_server(&server, &rooms);
                    }
                    MatrixEvent::RoomLeft { room_id: _ } => {
                        // Room list will refresh on next sync.
                    }
                    MatrixEvent::LeaveFailed { error } => {
                        toast_error(&toast_overlay, "Failed to leave", &error);
                    }
                    MatrixEvent::RoomInvited { room_id, room_name, inviter_name } => {
                        // Desktop notification only when window is unfocused —
                        // bypass notification_manager.push() to avoid the "Jump" banner.
                        {
                            use gtk::prelude::GtkWindowExt;
                            use gio::prelude::ApplicationExt;
                            let win_ref: Option<gtk::Window> = window.clone().upcast_ref::<gtk::Window>().downgrade().upgrade();
                            let is_active = win_ref.as_ref().map(|w| w.is_active()).unwrap_or(false);
                            if !is_active {
                                let app: Option<gtk::Application> = win_ref.as_ref().and_then(|w| GtkWindowExt::application(w));
                                if let Some(app) = app {
                                    let notif = gio::Notification::new(&format!("Invite from {inviter_name}"));
                                    notif.set_body(Some(&format!("{inviter_name} invited you to {room_name}")));
                                    notif.set_priority(gio::NotificationPriority::High);
                                    notif.set_default_action_and_target_value(
                                        "app.open-room",
                                        Some(&glib::Variant::from(room_id.as_str())),
                                    );
                                    app.send_notification(Some(&format!("invite-{room_id}")), &notif);
                                }
                            }
                        }

                        // Store in bell log so the user can act on it later.
                        let invite_event_id = format!("__invite__{room_id}");
                        let ts = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        window.push_notification(
                            &room_id, &invite_event_id, &inviter_name,
                            &room_name, &format!("invited you to {room_name}"), ts,
                        );

                        // In-app toast with Accept button.
                        let accept_tx = window.imp().command_tx.get().unwrap().clone();
                        let accept_room_id = room_id.clone();
                        let t = adw::Toast::builder()
                            .title(&format!("{inviter_name} invited you to {room_name}"))
                            .button_label("Accept")
                            .timeout(0)
                            .build();
                        t.connect_button_clicked(move |toast| {
                            toast.dismiss();
                            let tx = accept_tx.clone();
                            let rid = accept_room_id.clone();
                            glib::spawn_future_local(async move {
                                let _ = tx.send(
                                    crate::matrix::MatrixCommand::AcceptInvite { room_id: rid }
                                ).await;
                            });
                        });
                        toast_overlay.add_toast(t);
                    }
                    MatrixEvent::InviteSuccess { user_id } => {
                        toast(&toast_overlay, &format!("Invited {user_id}"));
                    }
                    MatrixEvent::InviteFailed { error } => {
                        toast_error(&toast_overlay, "Invite failed", &error);
                    }
                    MatrixEvent::UserSearchResults { results } => {
                        if let Some(cb) = window.imp().user_search_cb.borrow().as_ref() {
                            cb(results);
                        }
                    }
                    MatrixEvent::DmReady { user_id: _, room_id, room_name } => {
                        // Navigate to the DM room — same as clicking it in the sidebar.
                        window.imp().current_room_id.replace(Some(room_id.clone()));
                        if let Some(page) = window.imp().content_page.get() {
                            page.set_title(&room_name);
                        }
                        message_view.clear(&room_id);
                        let tx = window.imp().command_tx.get().unwrap().clone();
                        let rid = room_id.clone();
                        glib::spawn_future_local(async move {
                            let _ = tx.send(MatrixCommand::SelectRoom { room_id: rid, known_unread: 0 }).await;
                        });
                    }
                    MatrixEvent::DmFailed { error } => {
                        toast_error(&toast_overlay, "Failed to open DM", &error);
                    }
                    MatrixEvent::SyncGap { room_id } => {
                        // The memory cache was wiped by the sync thread.
                        // If the user is currently viewing this room, trigger a
                        // silent bg_refresh so gap events (messages the limited
                        // sync response skipped) are fetched from the server.
                        // The "Updating messages" banner is suppressed when
                        // messages_loaded=true, so this is invisible to the user.
                        let current = window.imp().current_room_id.borrow().clone();
                        if current.as_deref() == Some(&room_id) {
                            tracing::info!("SyncGap for current room {room_id} — triggering silent refresh");
                            let tx = command_tx.clone();
                            let rid = room_id.clone();
                            glib::spawn_future_local(async move {
                                let _ = tx.send(MatrixCommand::RefreshRoom { room_id: rid }).await;
                            });
                        } else {
                            tracing::debug!("SyncGap for {room_id} (cache wiped; will refetch on next SelectRoom)");
                        }
                    }
                    MatrixEvent::TypingUsers { room_id, names } => {
                        let current = window.imp().current_room_id.borrow().clone();
                        if current.as_deref() == Some(&room_id) {
                            message_view.set_typing_users(&names);
                        }
                        // Update the room row typing indicator in the sidebar.
                        let reg = room_list_view.imp().room_registry.borrow();
                        if let Some(obj) = reg.get(&room_id) {
                            obj.set_is_typing(!names.is_empty());
                        }
                    }
                    MatrixEvent::ThreadReplies { room_id, thread_root_id: _, root_message, replies } => {
                        let current = window.imp().current_room_id.borrow().clone();
                        if current.as_deref() == Some(&room_id) {
                            window.show_thread_sidebar(&root_message, &replies);
                        }
                    }
                    MatrixEvent::LoggedOut => {
                        toast(&toast_overlay, "Logged out");
                        window.show_login();
                    }
                    #[cfg(feature = "motd")]
                    MatrixEvent::TopicChanged { room_id, new_topic } => {
                        if crate::config::settings().plugins.motd {
                            let current = window.imp().current_room_id.borrow().clone();
                            if current.as_deref() == Some(&room_id) {
                                // User is in this room — show a toast immediately.
                                let label = if new_topic.is_empty() {
                                    "Room topic was cleared".to_string()
                                } else {
                                    format!("Topic updated: {new_topic}")
                                };
                                toast(&toast_overlay, &label);
                            } else {
                                // Not in this room — flag the row in the sidebar.
                                room_list_view.set_topic_changed(&room_id, true);
                            }
                        }
                    }
                    MatrixEvent::RoomKeysReceived { room_ids } => {
                        let current = window.imp().current_room_id.borrow().clone();
                        if let Some(rid) = current {
                            // Empty room_ids means all rooms changed (post-recovery cache clear).
                            let affects_current = room_ids.is_empty()
                                || room_ids.iter().any(|id| *id == rid);
                            if affects_current {
                                let tx = window.imp().command_tx.get().unwrap().clone();
                                glib::spawn_future_local(async move {
                                    // Re-loading the current room after key recovery: user is
                                    // already viewing it so unread count is 0.
                                    let _ = tx.send(MatrixCommand::SelectRoom { room_id: rid, known_unread: 0 }).await;
                                });
                            }
                        }
                    }
                    #[cfg(feature = "ai")]
                    MatrixEvent::RoomAlert { room_id, room_name, matched_term } => {
                        room_list_view.set_watch_alert(&room_id, true);
                        let msg = format!("\u{201c}{matched_term}\u{201d} matched in {room_name}");
                        toast_or_notify(
                            &window,
                            &toast_overlay,
                            &format!("alert-{room_id}"),
                            &format!("Watch alert: {room_name}"),
                            &msg,
                        );
                    }
                    MatrixEvent::RoomPrefetched { room_id } => {
                        tracing::debug!("space prefetch warm: {room_id}");
                    }
                }
            }
        });

        window
    }

    fn show_onboarding(&self) {
        let imp = self.imp();
        let weak = self.downgrade();
        imp.onboarding_page.connect_get_started(move || {
            let Some(win) = weak.upgrade() else { return };
            win.show_login();
        });
        imp.toolbar.set_content(Some(&imp.onboarding_page));
    }

    fn show_login(&self) {
        let imp = self.imp();
        // Register window actions early so toast buttons (e.g. "Recover Keys")
        // are live even before the main view is shown during the wizard flow.
        self.setup_actions();
        imp.toolbar.set_content(Some(&imp.login_page));
    }

    fn show_main_view(&self) {
        let imp = self.imp();

        // Register actions for the menu.
        self.setup_actions();

        // Sidebar header with hamburger menu — no close button (content header has it).
        let sidebar_header = adw::HeaderBar::builder()
            .show_end_title_buttons(false)
            .build();

        // Search toggle button — shows/hides the search bar.
        let search_btn = gtk::ToggleButton::builder()
            .icon_name("system-search-symbolic")
            .tooltip_text("Search rooms")
            .build();
        search_btn.add_css_class("flat");
        sidebar_header.pack_start(&search_btn);

        // "New / Join Room" button — activates the inline join bar.
        let join_header_btn = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("Join or explore rooms (Ctrl+J)")
            .action_name("win.join-room")
            .build();
        join_header_btn.add_css_class("flat");
        sidebar_header.pack_start(&join_header_btn);

        let menu = gio::Menu::new();
        let main_section = gio::Menu::new();
        main_section.append(Some("_Verify Device"), Some("win.verify"));
        main_section.append(Some("_Recover Encryption Keys"), Some("win.recover-keys"));
        main_section.append(Some("_Import E2E Keys from File"), Some("win.import-keys"));
        main_section.append(Some("_Preferences"), Some("win.preferences"));
        menu.append_section(None, &main_section);
        let account_section = gio::Menu::new();
        account_section.append(Some("_Log Out"), Some("win.logout"));
        menu.append_section(None, &account_section);
        let about_section = gio::Menu::new();
        about_section.append(Some("_Keyboard Shortcuts"), Some("win.shortcuts"));
        about_section.append(Some("_About Hikyaku"), Some("win.about"));
        menu.append_section(None, &about_section);
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .build();
        sidebar_header.pack_end(&menu_button);

        // Bell toggle — shows/hides the notifications right sidebar.
        let bell_btn = gtk::ToggleButton::builder()
            .tooltip_text("Notifications")
            .build();
        bell_btn.add_css_class("flat");

        // Badge label overlaid on the bell button.
        let badge_label = gtk::Label::builder()
            .css_classes(["notif-badge"])
            .visible(false)
            .halign(gtk::Align::End)
            .valign(gtk::Align::Start)
            .can_focus(false)
            .can_target(false)
            .build();

        let bell_image = gtk::Image::from_icon_name("alarm-symbolic");
        let bell_overlay = gtk::Overlay::builder()
            .child(&bell_btn)
            .build();
        bell_btn.set_child(Some(&bell_image));
        bell_overlay.add_overlay(&badge_label);

        let _ = imp.notif_bell_button.set(bell_btn.clone());
        let _ = imp.notif_badge.set(badge_label);
        sidebar_header.pack_end(&bell_overlay);

        let sidebar_toolbar = adw::ToolbarView::new();
        sidebar_toolbar.add_top_bar(&sidebar_header);
        sidebar_toolbar.set_content(Some(&imp.room_list_view));

        // Wire search toggle button ↔ search bar.
        let search_bar = imp.room_list_view.search_bar();
        let rlv_weak = imp.room_list_view.downgrade();
        search_btn.connect_toggled(move |btn| {
            if let Some(rlv) = rlv_weak.upgrade() {
                let bar = rlv.search_bar();
                if btn.is_active() != bar.is_search_mode() {
                    rlv.toggle_search();
                }
            }
        });
        // Keep toggle button in sync when search mode is disabled (e.g. Escape).
        let search_btn_weak = search_btn.downgrade();
        search_bar.connect_notify_local(Some("search-mode-enabled"), move |bar: &gtk::SearchBar, _| {
            if let Some(btn) = search_btn_weak.upgrade() {
                btn.set_active(bar.is_search_mode());
            }
        });
        // Allow typing anywhere in the sidebar to activate search.
        search_bar.set_key_capture_widget(Some(&sidebar_toolbar));

        let sidebar_page = adw::NavigationPage::builder()
            .title("Rooms")
            .child(&sidebar_toolbar)
            .build();

        // Content: message view with info and leave buttons in header.
        let content_header = adw::HeaderBar::new();

        // Info button (blue circle with "i") — hidden until a room is selected.
        let info_button = gtk::Button::builder()
            .icon_name("help-about-symbolic")
            .tooltip_text("Room details")
            .visible(false)
            .build();
        info_button.add_css_class("info-button");
        info_button.add_css_class("circular");
        content_header.pack_end(&info_button);

        // Spacer between info and leave.
        let spacer = gtk::Separator::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_start(4)
            .margin_end(4)
            .build();
        spacer.add_css_class("spacer");
        content_header.pack_end(&spacer);

        // Bookmark toggle button — hidden until a room is selected.
        let bookmark_button = gtk::Button::builder()
            .icon_name("non-starred-symbolic")
            .tooltip_text("Bookmark this room")
            .visible(false)
            .build();
        bookmark_button.add_css_class("flat");
        content_header.pack_end(&bookmark_button);

        // Leave button (flat, to the left of info).
        let leave_button = gtk::Button::builder()
            .icon_name("application-exit-symbolic")
            .tooltip_text("Leave this room")
            .build();
        leave_button.add_css_class("flat");
        content_header.pack_end(&leave_button);

        // Export metrics button — hidden until a room is selected.
        let export_button = gtk::Button::builder()
            .icon_name("x-office-spreadsheet-symbolic")
            .tooltip_text("Export room metrics")
            .visible(false)
            .build();
        export_button.add_css_class("flat");
        content_header.pack_end(&export_button);

        // Export messages button — dumps visible messages to JSONL for health_test etc.
        let export_messages_button = gtk::Button::builder()
            .icon_name("document-save-symbolic")
            .tooltip_text("Export messages to file")
            .visible(false)
            .build();
        export_messages_button.add_css_class("flat");
        content_header.pack_end(&export_messages_button);

        // Store button references so room selection can show/hide them.
        let _ = imp.info_button.set(info_button.clone());
        let _ = imp.bookmark_button.set(bookmark_button.clone());
        let _ = imp.export_button.set(export_button.clone());
        let _ = imp.export_messages_button.set(export_messages_button.clone());

        // Wire bookmark toggle.
        let window_weak_bm = self.downgrade();
        let toast_bm = imp.toast_overlay.clone();
        bookmark_button.connect_clicked(move |btn| {
            let Some(window) = window_weak_bm.upgrade() else { return };
            let imp = window.imp();
            let Some(room_id) = imp.current_room_id.borrow().clone() else { return };
            let Some(tx) = imp.command_tx.get().cloned() else { return };

            // Toggle: check current icon to determine state.
            let is_currently_fav = btn.icon_name().as_deref() == Some("starred-symbolic");
            let new_fav = !is_currently_fav;

            // Update icon immediately.
            btn.set_icon_name(if new_fav { "starred-symbolic" } else { "non-starred-symbolic" });
            btn.set_tooltip_text(Some(if new_fav { "Remove bookmark" } else { "Bookmark this room" }));

            let msg = if new_fav { "Bookmarked" } else { "Bookmark removed" };
            toast(&toast_bm, msg);

            glib::spawn_future_local(async move {
                let _ = tx.send(MatrixCommand::SetFavourite { room_id, is_favourite: new_fav }).await;
            });
        });

        // Wire up info button — toggle room details sidebar.
        let window_weak_info = self.downgrade();
        info_button.connect_clicked(move |_| {
            let Some(window) = window_weak_info.upgrade() else { return };
            let imp = window.imp();
            let currently_visible = imp.details_revealer.reveals_child();
            if currently_visible {
                imp.details_revealer.set_reveal_child(false);
                imp.details_revealer.set_visible(false);
                if let Some(sep) = imp.details_separator.get() {
                    sep.set_visible(false);
                }
            } else {
                window.show_room_details();
                if let Some(sep) = imp.details_separator.get() {
                    sep.set_visible(true);
                }
                imp.details_revealer.set_visible(true);
                imp.details_revealer.set_reveal_child(true);
            }
        });

        // Wire up leave button.
        let window_weak_leave = self.downgrade();
        let toast_leave = imp.toast_overlay.clone();
        leave_button.connect_clicked(move |_| {
            let Some(window) = window_weak_leave.upgrade() else { return };
            let imp = window.imp();
            let Some(room_id) = imp.current_room_id.borrow().clone() else { return };
            let room_name = imp.content_page.get()
                .map(|p| p.title().to_string())
                .unwrap_or_default();
            let Some(tx) = imp.command_tx.get().cloned() else { return };

            let toast = adw::Toast::builder()
                .title(&format!("Leave {room_name}?"))
                .button_label("Leave")
                .timeout(5)
                .build();
            toast.connect_button_clicked(move |_| {
                let tx = tx.clone();
                let rid = room_id.clone();
                glib::spawn_future_local(async move {
                    let _ = tx.send(MatrixCommand::LeaveRoom { room_id: rid }).await;
                });
            });
            toast_leave.add_toast(toast);
        });

        // Wire up export metrics button.
        let window_weak_export = self.downgrade();
        export_button.connect_clicked(move |_| {
            let Some(window) = window_weak_export.upgrade() else { return };
            let imp = window.imp();
            let Some(room_id) = imp.current_room_id.borrow().clone() else { return };
            let tx = imp.command_tx.get().unwrap().clone();
            show_export_metrics_dialog(&window, room_id, tx);
        });

        // Wire up export messages button — dumps the currently displayed messages to JSONL.
        // Reads from the list_store (what's on screen) so it always matches what the user sees,
        // regardless of what the disk cache holds.
        let window_weak_msg_export = self.downgrade();
        let toast_msg_export = imp.toast_overlay.clone();
        export_messages_button.connect_clicked(move |_| {
            let Some(window) = window_weak_msg_export.upgrade() else { return };
            let imp = window.imp();
            let Some(room_id) = imp.current_room_id.borrow().clone() else { return };
            let safe_name: String = room_id.trim_start_matches('!')
                .chars()
                .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
                .collect();
            let filename = format!("hikyaku-{safe_name}.jsonl");
            let path = dirs::download_dir()
                .or_else(dirs::home_dir)
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join(&filename);
            match imp.message_view.export_messages_jsonl(&path) {
                Ok(0) => toast(&toast_msg_export,
                    "No messages to export — scroll up to load history first"),
                Ok(n) => toast(&toast_msg_export,
                    &format!("Exported {n} messages → {}", path.display())),
                Err(e) => toast_error(&toast_msg_export, "Export failed", &e.to_string()),
            }
        });

        // Details sidebar (right side, hidden by default).
        let details_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&imp.details_content)
            .build();
        // Pinned close button at the bottom of the sidebar.
        let details_close_btn = gtk::Button::builder()
            .label("Close")
            .css_classes(["suggested-action", "caption"])
            .margin_start(8)
            .margin_end(8)
            .margin_top(4)
            .margin_bottom(4)
            .build();
        let revealer_for_close = imp.details_revealer.clone();
        let sep_cell_for_close = imp.details_separator.clone();
        details_close_btn.connect_clicked(move |_| {
            revealer_for_close.set_reveal_child(false);
            revealer_for_close.set_visible(false);
            if let Some(sep) = sep_cell_for_close.get() {
                sep.set_visible(false);
            }
        });
        let details_wrapper = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        details_wrapper.append(&details_scroll);
        details_wrapper.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        details_wrapper.append(&details_close_btn);
        imp.details_revealer.set_child(Some(&details_wrapper));

        // Notification sidebar contents.
        let notif_list_box = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["navigation-sidebar"])
            .build();
        let _ = imp.notif_list_box.set(notif_list_box.clone());

        let notif_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&notif_list_box)
            .build();

        let notif_header_lbl = gtk::Label::builder()
            .label("Notifications")
            .halign(gtk::Align::Start)
            .hexpand(true)
            .css_classes(["heading"])
            .margin_start(12)
            .margin_end(8)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        let notif_clear_btn = gtk::Button::builder()
            .icon_name("edit-clear-all-symbolic")
            .tooltip_text("Clear all notifications")
            .css_classes(["flat", "circular"])
            .margin_end(4)
            .build();
        let notif_header_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(0)
            .build();
        notif_header_box.append(&notif_header_lbl);
        notif_header_box.append(&notif_clear_btn);

        let notif_wrapper = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .width_request(280)
            .css_classes(["notif-sidebar"])
            .build();
        notif_wrapper.append(&notif_header_box);
        notif_wrapper.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        notif_wrapper.append(&notif_scroll);
        imp.notif_revealer.set_child(Some(&notif_wrapper));

        // Clear all notifications.
        let notif_store_for_clear = imp.notif_store.clone();
        let notif_list_for_clear = notif_list_box.clone();
        let notif_badge_for_clear = imp.notif_badge.get().unwrap().clone();
        let notif_bell_for_clear = imp.notif_bell_button.get().unwrap().clone();
        let notif_count_ref = self.downgrade();
        notif_clear_btn.connect_clicked(move |_| {
            notif_store_for_clear.remove_all();
            while let Some(row) = notif_list_for_clear.first_child() {
                notif_list_for_clear.remove(&row);
            }
            notif_badge_for_clear.set_visible(false);
            if let Some(w) = notif_count_ref.upgrade() {
                w.imp().notif_unread_count.set(0);
            }
            notif_bell_for_clear.remove_css_class("notif-bell-unread");
        });

        // Bell button toggles the notification sidebar.
        let notif_revealer_for_bell = imp.notif_revealer.clone();
        bell_btn.connect_toggled(move |btn| {
            let visible = btn.is_active();
            notif_revealer_for_bell.set_visible(visible);
            notif_revealer_for_bell.set_reveal_child(visible);
        });

        // Content area: message view + optional notifications sidebar + optional details sidebar.
        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .build();
        let details_separator = gtk::Separator::builder()
            .orientation(gtk::Orientation::Vertical)
            .visible(false)
            .build();
        imp.details_separator.set(details_separator.clone()).ok();
        content_box.append(&imp.message_view);
        content_box.append(&imp.notif_revealer);
        content_box.append(&details_separator);
        content_box.append(&imp.details_revealer);
        // Make message view expand, sidebars stay fixed width.
        imp.message_view.set_hexpand(true);

        let content_toolbar = adw::ToolbarView::new();
        content_toolbar.add_top_bar(&content_header);
        content_toolbar.add_top_bar(&imp.notify_banner);
        content_toolbar.set_content(Some(&content_box));

        let content_page = adw::NavigationPage::builder()
            .title("Hikyaku")
            .child(&content_toolbar)
            .build();
        let _ = imp.content_page.set(content_page.clone());

        let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        paned.set_start_child(Some(&sidebar_page));
        paned.set_end_child(Some(&content_page));
        paned.set_shrink_start_child(false);
        paned.set_shrink_end_child(false);

        // Restore saved sidebar width (clamped to a sane range).
        let saved_width = crate::config::gsettings().int("sidebar-width").clamp(180, 600);
        paned.set_position(saved_width);

        // Persist the width whenever the user drags the divider.
        paned.connect_notify_local(Some("position"), |p, _| {
            let _ = crate::config::gsettings().set_int("sidebar-width", p.position());
        });

        // Wire up the banner's Verify button.
        let tx = imp.command_tx.get().unwrap().clone();
        let banner = imp.verify_banner.clone();
        let window_weak = self.downgrade();
        imp.verify_banner.connect_button_clicked(move |_| {
            // Guard: don't start a second verification if one is already in progress.
            if let Some(window) = window_weak.upgrade() {
                if window.imp().verify_dialog.borrow().is_some() {
                    return;
                }
                banner.set_revealed(false);
                let tx = tx.clone();
                let dialog = crate::widgets::verification_dialog::show_waiting_dialog(
                    &window, tx.clone(),
                );
                // Clear verify_dialog on any dismissal; re-show banner on cancel.
                let window_weak2 = window.downgrade();
                let tx2 = tx.clone();
                dialog.connect_response(None, move |_, response| {
                    if let Some(w) = window_weak2.upgrade() {
                        w.imp().verify_dialog.replace(None);
                        if response == "cancel" {
                            w.imp().verify_banner.set_revealed(true);
                            let tx = tx2.clone();
                            glib::spawn_future_local(async move {
                                let _ = tx
                                    .send(MatrixCommand::CancelVerification {
                                        flow_id: String::new(),
                                    })
                                    .await;
                            });
                        }
                    }
                });
                window.imp().verify_dialog.replace(Some(dialog));
                glib::spawn_future_local(async move {
                    let _ = tx
                        .send(MatrixCommand::RequestSelfVerification)
                        .await;
                });
            }
        });

        // verify_banner spans the full window width — use a wrapper box since
        // there is no single top-level ToolbarView+HeaderBar for the whole window.
        let main_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        paned.set_vexpand(true);
        main_box.append(&imp.verify_banner);
        main_box.append(&paned);

        // AdwBottomSheet: main view is always live; bookmarks slides up over it.
        // The sheet animation is handled by libadwaita — no layout loop risk.
        let bookmarks_sheet = adw::BottomSheet::builder()
            .content(&main_box)
            .sheet(&imp.bookmarks_overview)
            .full_width(true)
            .show_drag_handle(false)
            .modal(true)
            .open(false)
            .build();
        imp.bookmarks_sheet.set(bookmarks_sheet.clone()).ok();
        imp.toast_overlay.set_child(Some(&bookmarks_sheet));

        // Sync the sheet content height to the window height while the sheet
        // is open.  BookmarksOverview.measure() returns minimum=0 so this
        // height_request only sets the natural height (what BottomSheet uses
        // to size the panel), not the minimum — the window can resize freely.
        //
        // notify::default-height fires for every interactive resize because
        // minimum=0 lets the window manager honour any size the user requests.
        let sync_sheet_height = {
            let overview = imp.bookmarks_overview.clone();
            let sheet = bookmarks_sheet.clone();
            move |win: &crate::widgets::window::MxWindow| {
                if sheet.is_open() {
                    let h = win.height();
                    if h > 0 { overview.set_height_request(h); }
                } else {
                    // Always reset when the sheet is closed.  Without this, if
                    // the user closes the sheet via Escape or clicking outside
                    // (bypassing the action handlers), height_request stays at
                    // the old window height.  AdwBottomSheet positions the hidden
                    // sheet below the content by that natural height, locking the
                    // window minimum and preventing the user from shrinking it.
                    overview.set_height_request(-1);
                }
            }
        };
        self.connect_notify_local(Some("default-height"), {
            let f = sync_sheet_height.clone();
            move |win, _| f(win)
        });
        // Ensure height_request is reset immediately whenever the sheet closes,
        // regardless of how it was closed (action handler, Escape, outside click).
        // The default-height handler above also resets on the next resize, but
        // this catches the case where the window size doesn't change after close.
        {
            let overview = imp.bookmarks_overview.clone();
            bookmarks_sheet.connect_notify_local(Some("open"), move |sheet, _| {
                if !sheet.is_open() {
                    overview.set_height_request(-1);
                }
            });
        }
        // Fullscreen/maximise: allocation updates asynchronously — defer one
        // frame so win.height() returns the new value.
        for prop in ["fullscreened", "maximized"] {
            let f = sync_sheet_height.clone();
            let win_weak = self.downgrade();
            self.connect_notify_local(Some(prop), move |_, _| {
                let f = f.clone();
                let win_weak = win_weak.clone();
                glib::idle_add_local_once(move || {
                    if let Some(win) = win_weak.upgrade() { f(&win); }
                });
            });
        }

        // Bookmarks tab activated → show overlay + reload cards.
        let window_weak = self.downgrade();
        imp.room_list_view.connect_bookmarks_activated(move || {
            let Some(window) = window_weak.upgrade() else { return };
            let imp = window.imp();
            // Open the sheet first so libadwaita can start the slide-up animation
            // immediately. Heavy work (file I/O + widget construction) is deferred
            // to an idle callback so it doesn't block the first animation frame.
            if let Some(sheet) = imp.bookmarks_sheet.get() {
                imp.bookmarks_overview.set_height_request(window.height());
                sheet.set_open(true);
            }
            let window_weak2 = window.downgrade();
            glib::idle_add_local_once(move || {
                let Some(window) = window_weak2.upgrade() else { return };
                let imp = window.imp();
                imp.bookmarks_overview.reload_messages();
                imp.bookmarks_overview.clear_search();
                // Rebuild favourite room cards with current registry objects.
                // See comment on the other set_favourite_rooms call site: tombstoned
                // rooms are excluded so same-name duplicates don't appear next to
                // their replacement.
                let registry = imp.room_list_view.imp().room_registry.borrow();
                let cached = imp.room_list_view.imp().cached_rooms.borrow();
                let mut favs: Vec<crate::models::RoomObject> = cached.iter()
                    .filter(|r| r.is_favourite && !r.is_tombstoned)
                    .filter_map(|r| registry.get(&r.room_id).cloned())
                    .collect();
                favs.sort_by(|a, b| b.last_activity_ts().cmp(&a.last_activity_ts()));
                imp.bookmarks_overview.set_favourite_rooms(&favs);
            });
        });

        // Bookmarks overlay closed → slide down, switch sidebar to messages.
        let window_weak = self.downgrade();
        imp.bookmarks_overview.connect_close(move || {
            let Some(window) = window_weak.upgrade() else { return };
            let imp = window.imp();
            if let Some(sheet) = imp.bookmarks_sheet.get() {
                sheet.set_open(false);
                imp.bookmarks_overview.set_height_request(-1);
            }
            imp.room_list_view.select_messages_tab();
        });

        // Favourite room card clicked → instant dismiss + navigate to room.
        let window_weak = self.downgrade();
        imp.bookmarks_overview.connect_room_navigate(move |room_id, room_name| {
            let Some(window) = window_weak.upgrade() else { return };
            let imp = window.imp();
            if let Some(sheet) = imp.bookmarks_sheet.get() {
                sheet.set_open(false);
                imp.bookmarks_overview.set_height_request(-1);
            }
            imp.room_list_view.navigate_to_room_context(&room_id);
            let has_cb = imp.room_list_view.imp().on_room_selected.borrow().is_some();
            if has_cb {
                let borrow = imp.room_list_view.imp().on_room_selected.borrow();
                borrow.as_ref().unwrap()(room_id, room_name);
            }
        });

        // Saved message card clicked → instant dismiss + navigate to room + flash message.
        let window_weak = self.downgrade();
        imp.bookmarks_overview.connect_navigate(move |room_id, event_id| {
            let Some(window) = window_weak.upgrade() else { return };
            let imp = window.imp();
            if let Some(sheet) = imp.bookmarks_sheet.get() {
                sheet.set_open(false);
                imp.bookmarks_overview.set_height_request(-1);
            }
            imp.room_list_view.navigate_to_room_context(&room_id);
            // Navigate to room first; flash happens after messages load via pending_flash.
            let registry = imp.room_list_view.imp().room_registry.borrow();
            let name = registry.get(&room_id).map(|o| o.name()).unwrap_or_default();
            drop(registry);
            if let Some(ref cb) = *imp.room_list_view.imp().on_room_selected.borrow() {
                cb(room_id, name);
            }
            // Store event_id to flash after the room loads.
            imp.pending_flash_event_id.replace(Some(event_id));
        });

        // Message bookmark button → save to store + highlight row + add card to overview.
        let window_weak = self.downgrade();
        imp.message_view.connect_bookmark(move |event_id, sender, body, timestamp| {
            let Some(window) = window_weak.upgrade() else { return };
            let imp = window.imp();
            let room_id = imp.current_room_id.borrow().clone().unwrap_or_default();
            let room_name = imp.content_page.get()
                .map(|p| p.title().to_string())
                .unwrap_or_default();
            let entry = crate::bookmarks::BookmarkEntry {
                room_id,
                room_name,
                event_id: event_id.clone(),
                sender,
                body_preview: body.chars().take(200).collect(),
                timestamp,
            };
            crate::bookmarks::BOOKMARK_STORE.add(entry.clone());
            imp.message_view.set_message_bookmarked(&event_id, true);
            imp.bookmarks_overview.add_message_card(&entry);
            imp.toast_overlay.add_toast(
                adw::Toast::builder().title("Saved for later").timeout(2).build()
            );
        });

        // Unbookmark: remove from store, un-highlight row, remove card from overview.
        let window_weak = self.downgrade();
        imp.message_view.connect_unbookmark(move |event_id| {
            let Some(window) = window_weak.upgrade() else { return };
            let imp = window.imp();
            crate::bookmarks::BOOKMARK_STORE.remove(&event_id);
            imp.message_view.set_message_bookmarked(&event_id, false);
            imp.bookmarks_overview.remove_message_card(&event_id);
            imp.toast_overlay.add_toast(
                adw::Toast::builder().title("Bookmark removed").timeout(2).build()
            );
        });

        let window_weak = self.downgrade();
        imp.message_view.connect_add_to_rolodex(move |user_id, display_name| {
            let Some(window) = window_weak.upgrade() else { return };
            let store = &window.imp().rolodex_store;
            // Deduplicate: do nothing if already present.
            let already = (0..store.n_items()).any(|i| {
                store.item(i)
                    .and_downcast::<crate::models::RolodexEntryObject>()
                    .map(|o| o.user_id() == user_id)
                    .unwrap_or(false)
            });
            if already { return; }
            let added_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            store.append(&crate::models::RolodexEntryObject::new(
                &user_id, &display_name, "", added_at,
            ));
            // items-changed fires → save to JSON + GSettings automatically.
            window.imp().toast_overlay.add_toast(
                adw::Toast::builder()
                    .title(&format!("{display_name} added to contacts"))
                    .timeout(2)
                    .build()
            );
        });

        let window_weak = self.downgrade();
        imp.message_view.connect_remove_from_rolodex(move |user_id| {
            let Some(window) = window_weak.upgrade() else { return };
            let store = &window.imp().rolodex_store;
            if let Some(pos) = (0..store.n_items()).find(|&i| {
                store.item(i)
                    .and_downcast::<crate::models::RolodexEntryObject>()
                    .map(|o| o.user_id() == user_id)
                    .unwrap_or(false)
            }) {
                store.remove(pos);
                // items-changed fires → save to JSON + GSettings automatically.
            }
            window.imp().toast_overlay.add_toast(
                adw::Toast::builder().title("Removed from contacts").timeout(2).build()
            );
        });

        let window_weak = self.downgrade();
        imp.message_view.connect_get_rolodex_notes(move |user_id| {
            let window = window_weak.upgrade()?;
            let store = &window.imp().rolodex_store;
            (0..store.n_items()).find_map(|i| {
                store.item(i)
                    .and_downcast::<crate::models::RolodexEntryObject>()
                    .filter(|o| o.user_id() == user_id)
                    .map(|o| o.notes())
            })
        });

        let window_weak = self.downgrade();
        imp.message_view.connect_save_rolodex_notes(move |user_id, notes| {
            let Some(window) = window_weak.upgrade() else { return };
            let store = &window.imp().rolodex_store;
            if let Some(obj) = (0..store.n_items()).find_map(|i| {
                store.item(i)
                    .and_downcast::<crate::models::RolodexEntryObject>()
                    .filter(|o| o.user_id() == user_id)
            }) {
                obj.set_notes(notes.as_str());
                // Manually trigger persistence since set_property doesn't fire items-changed.
                let n = store.n_items();
                let mut entries = Vec::with_capacity(n as usize);
                let mut gs_entries = Vec::with_capacity(n as usize);
                for i in 0..n {
                    if let Some(o) = store.item(i).and_downcast::<crate::models::RolodexEntryObject>() {
                        gs_entries.push(o.user_id());
                        entries.push(o.to_entry());
                    }
                }
                crate::plugins::rolodex::save(&entries);
                crate::config::set_rolodex(&gs_entries);
            }
        });

        // Register custom icon theme for our symbolic icons.
        if let Some(display) = gtk::gdk::Display::default() {
            let theme = gtk::IconTheme::for_display(&display);
            // Add the data/icons directory relative to the binary's location,
            // or fall back to the source tree path for development.
            let exe_dir = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()));
            if let Some(dir) = exe_dir {
                let icons_dir = dir.join("../../data/icons");
                if icons_dir.exists() {
                    theme.add_search_path(&icons_dir);
                }
            }
            // Always add the source-tree path for `cargo run`.
            theme.add_search_path("data/icons");
        }

        // Apply font, tint, and bookmark highlight settings from config.
        apply_font_css(&crate::config::settings().appearance);
        apply_tint_css(&crate::config::settings().appearance);
        apply_bookmark_css(&crate::config::settings().appearance.bookmark_highlight_color);
        apply_new_message_css(&crate::config::settings().appearance.new_message_highlight_color);
        // Initialize static app CSS (active room highlight etc.).
        APP_CSS_PROVIDER.with(|_| {});

        // Persist rolodex store to JSON + GSettings on every change.
        imp.rolodex_store.connect_items_changed(|store, _, _, _| {
            let n = store.n_items();
            let mut entries = Vec::with_capacity(n as usize);
            let mut gs_entries = Vec::with_capacity(n as usize);
            for i in 0..n {
                if let Some(obj) = store.item(i).and_downcast::<crate::models::RolodexEntryObject>() {
                    gs_entries.push(format!("{}|{}", obj.display_name(), obj.user_id()));
                    entries.push(obj.to_entry());
                }
            }
            crate::plugins::rolodex::save(&entries);
            crate::config::set_rolodex(&gs_entries);
        });

        // Badge styles for room rows.
        let badge_css = gtk::CssProvider::new();
        badge_css.load_from_string(
            ".unread-badge {
                background: alpha(@accent_bg_color, 0.8);
                color: white;
                border-radius: 9px;
                padding: 1px 5px;
                font-size: 10px;
                font-weight: bold;
                min-width: 16px;
            }
            .highlight-badge {
                background: #e01b24;
                color: white;
                border-radius: 9px;
                padding: 1px 5px;
                font-size: 10px;
                font-weight: bold;
                min-width: 16px;
            }
            .tombstone-info {
                background: alpha(@warning_bg_color, 0.2);
                border-radius: 8px;
                padding: 8px 12px;
            }
            .tombstone-view {
                background: alpha(@warning_bg_color, 0.08);
            }
            .pinned-message {
                background: alpha(@accent_bg_color, 0.08);
                border-radius: 6px;
                padding: 6px 10px;
                margin-top: 2px;
            }
            .info-button {
                background: #3584e4;
                color: white;
            }
            .info-button:hover {
                background: #2a7de1;
            }
            .action-overlay {
                opacity: 0.7;
                transition: opacity 200ms ease-in;
            }
            .action-overlay:hover {
                opacity: 1.0;
            }
            .msg-action-bar {
                opacity: 0;
                transition: opacity 150ms ease-in-out;
                margin-top: 0;
                margin-bottom: 0;
            }
            .msg-action-bar-visible {
                opacity: 1.0;
            }
            .msg-action-bar button {
                min-height: 20px;
                min-width: 20px;
                padding: 2px;
            }
            .media-placeholder {
                background: alpha(@accent_bg_color, 0.1);
                border-radius: 8px;
                padding: 8px 12px;
            }
            .reaction-pill {
                background: alpha(@window_fg_color, 0.1);
                border-radius: 12px;
                padding: 4px 8px;
                font-size: 14px;
            }
            .flash-highlight {
                background: alpha(@accent_bg_color, 0.25);
                transition: background 500ms ease-out;
            }
            .mention-row {
                background: alpha(@accent_bg_color, 0.12);
                border-radius: 8px;
                border-left: 3px solid @accent_bg_color;
                padding: 6px 10px;
                margin: 2px 4px;
            }
            .message-divider {
                background: none;
                padding: 4px 0;
                margin: 8px 12px;
                border-top: 1px solid alpha(@accent_bg_color, 0.5);
            }
            .active-room-row {
                background: alpha(@accent_bg_color, 0.18);
                border-radius: 6px;
            }
            @keyframes typing-pulse {
                0%   { opacity: 1.0; }
                50%  { opacity: 0.4; }
                100% { opacity: 1.0; }
            }
            .typing-indicator {
                animation: typing-pulse 1.4s ease-in-out infinite;
                font-style: italic;
            }
            .code-block {
                border-radius: 6px;
                padding: 8px;
                font-family: monospace;
            }
",
        );
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &badge_css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
    }

    /// Push a new notification into the session log and update the sidebar.
    /// Called for @mentions and DMs.  Max 50 notifications kept; oldest dropped.
    fn push_notification(
        &self,
        room_id: &str,
        event_id: &str,
        sender: &str,
        room_name: &str,
        body: &str,
        timestamp: u64,
    ) {
        let imp = self.imp();
        let notif = crate::models::NotificationObject::new(
            room_id, event_id, sender, room_name, body, timestamp,
        );

        // Cap at 50 — drop the oldest (position 0) if over the limit.
        if imp.notif_store.n_items() >= 50 {
            imp.notif_store.remove(0);
            if let Some(lb) = imp.notif_list_box.get() {
                if let Some(first) = lb.row_at_index(0) {
                    lb.remove(&first);
                }
            }
        }

        imp.notif_store.append(&notif);
        let count = imp.notif_unread_count.get() + 1;
        imp.notif_unread_count.set(count);

        // Update badge.
        if let Some(badge) = imp.notif_badge.get() {
            badge.set_label(&count.to_string());
            badge.set_visible(true);
        }
        if let Some(btn) = imp.notif_bell_button.get() {
            btn.add_css_class("notif-bell-unread");
        }

        // Build and prepend a new row to the listbox (newest at top).
        if let Some(lb) = imp.notif_list_box.get() {
            let row = self.build_notif_row(&notif);
            lb.prepend(&row);
        }
    }

    /// Build a single ListBoxRow for a NotificationObject.
    fn build_notif_row(&self, notif: &crate::models::NotificationObject) -> gtk::ListBoxRow {
        let is_invite = notif.event_id().starts_with("__invite__");

        let row_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_start(8).margin_end(8)
            .margin_top(6).margin_bottom(6)
            .spacing(2)
            .build();

        let header = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .build();

        let room_lbl = gtk::Label::builder()
            .label(&notif.room_name())
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["caption-heading"])
            .build();

        // Format timestamp as HH:MM or "Today HH:MM".
        let ts = notif.timestamp();
        let time_str = {
            let secs = ts % 86400;
            format!("{:02}:{:02}", secs / 3600, (secs % 3600) / 60)
        };
        let time_lbl = gtk::Label::builder()
            .label(&time_str)
            .css_classes(["caption", "dim-label"])
            .build();

        header.append(&room_lbl);
        header.append(&time_lbl);

        let preview_text = format!("{}: {}", notif.sender(), notif.body());
        let preview_end = preview_text.char_indices().nth(80).map(|(i, _)| i).unwrap_or(preview_text.len());
        let preview_lbl = gtk::Label::builder()
            .label(&preview_text[..preview_end])
            .halign(gtk::Align::Start)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["caption"])
            .build();

        row_box.append(&header);
        row_box.append(&preview_lbl);

        // Invite rows get Accept / Decline buttons; other rows navigate on click.
        if is_invite {
            let btn_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .halign(gtk::Align::Start)
                .spacing(6)
                .margin_top(4)
                .build();

            let accept_btn = gtk::Button::builder()
                .label("Accept")
                .css_classes(["suggested-action", "pill"])
                .build();
            let decline_btn = gtk::Button::builder()
                .label("Decline")
                .css_classes(["destructive-action", "pill"])
                .build();

            btn_box.append(&accept_btn);
            btn_box.append(&decline_btn);
            row_box.append(&btn_box);

            // Accept
            let room_id = notif.room_id();
            let tx_accept = self.imp().command_tx.get().unwrap().clone();
            let notif_accept = notif.clone();
            let window_weak_accept = self.downgrade();
            accept_btn.connect_clicked(move |_| {
                let rid = room_id.clone();
                let tx = tx_accept.clone();
                glib::spawn_future_local(async move {
                    let _ = tx.send(crate::matrix::MatrixCommand::AcceptInvite { room_id: rid }).await;
                });
                notif_accept.set_is_read(true);
                if let Some(w) = window_weak_accept.upgrade() {
                    let imp = w.imp();
                    let cur = imp.notif_unread_count.get().saturating_sub(1);
                    imp.notif_unread_count.set(cur);
                    if let Some(badge) = imp.notif_badge.get() {
                        if cur == 0 { badge.set_visible(false); } else { badge.set_label(&cur.to_string()); }
                    }
                    if let Some(btn) = imp.notif_bell_button.get() {
                        if imp.notif_unread_count.get() == 0 { btn.remove_css_class("notif-bell-unread"); }
                    }
                }
            });

            // Decline
            let room_id2 = notif.room_id();
            let tx_decline = self.imp().command_tx.get().unwrap().clone();
            let notif_decline = notif.clone();
            let window_weak_decline = self.downgrade();
            decline_btn.connect_clicked(move |_| {
                let rid = room_id2.clone();
                let tx = tx_decline.clone();
                glib::spawn_future_local(async move {
                    let _ = tx.send(crate::matrix::MatrixCommand::DeclineInvite { room_id: rid }).await;
                });
                notif_decline.set_is_read(true);
                if let Some(w) = window_weak_decline.upgrade() {
                    let imp = w.imp();
                    let cur = imp.notif_unread_count.get().saturating_sub(1);
                    imp.notif_unread_count.set(cur);
                    if let Some(badge) = imp.notif_badge.get() {
                        if cur == 0 { badge.set_visible(false); } else { badge.set_label(&cur.to_string()); }
                    }
                    if let Some(btn) = imp.notif_bell_button.get() {
                        if imp.notif_unread_count.get() == 0 { btn.remove_css_class("notif-bell-unread"); }
                    }
                }
            });
        }

        let row = gtk::ListBoxRow::builder()
            .child(&row_box)
            .activatable(!is_invite)
            .build();

        // Unread styling: add CSS class, remove when is_read fires.
        if !notif.is_read() {
            row.add_css_class("notif-unread");
        }
        let row_weak = row.downgrade();
        notif.connect_notify_local(Some("is-read"), move |n, _| {
            if n.is_read() {
                if let Some(r) = row_weak.upgrade() {
                    r.remove_css_class("notif-unread");
                }
            }
        });

        if !is_invite {
            // Clicking: mark read, navigate to room + event, close sidebar.
            let notif_clone = notif.clone();
            let window_weak = self.downgrade();
            row.connect_activate(move |_row| {
                let Some(window) = window_weak.upgrade() else { return };
                let imp = window.imp();

                // Mark as read and update badge count.
                if !notif_clone.is_read() {
                    notif_clone.set_is_read(true);
                    let cur = imp.notif_unread_count.get().saturating_sub(1);
                    imp.notif_unread_count.set(cur);
                    if let Some(badge) = imp.notif_badge.get() {
                        if cur == 0 {
                            badge.set_visible(false);
                            if let Some(btn) = imp.notif_bell_button.get() {
                                btn.remove_css_class("notif-bell-unread");
                            }
                        } else {
                            badge.set_label(&cur.to_string());
                        }
                    }
                }

                // Store event_id to flash after the room messages load.
                imp.pending_flash_event_id.replace(Some(notif_clone.event_id()));

                // Navigate to room (same pattern as bookmark navigation).
                let room_id = notif_clone.room_id();
                imp.room_list_view.navigate_to_room_context(&room_id);
                let name = {
                    let reg = imp.room_list_view.imp().room_registry.borrow();
                    reg.get(&room_id).map(|o| o.name()).unwrap_or_else(|| notif_clone.room_name())
                };
                if let Some(ref cb) = *imp.room_list_view.imp().on_room_selected.borrow() {
                    cb(room_id, name);
                }

                // Close the notification sidebar.
                if let Some(btn) = imp.notif_bell_button.get() {
                    btn.set_active(false);
                }
                imp.notif_revealer.set_reveal_child(false);
                imp.notif_revealer.set_visible(false);
            });
        }

        row
    }

    fn setup_actions(&self) {
        use gio::ActionEntryBuilder;

        let about_action = ActionEntryBuilder::new("about")
            .activate(|window: &Self, _, _| {
                window.show_about_dialog();
            })
            .build();

        let preferences_action = ActionEntryBuilder::new("preferences")
            .activate(|window: &Self, _, _| {
                window.show_preferences();
            })
            .build();

        let verify_action = ActionEntryBuilder::new("verify")
            .activate(|window: &Self, _, _| {
                if window.imp().verify_dialog.borrow().is_some() {
                    return;
                }
                let tx = window.imp().command_tx.get().unwrap().clone();
                let dialog = crate::widgets::verification_dialog::show_waiting_dialog(
                    window, tx.clone(),
                );
                // Clear verify_dialog on any dismissal; re-show banner on cancel.
                let window_weak = window.downgrade();
                let tx2 = tx.clone();
                dialog.connect_response(None, move |_, response| {
                    if let Some(w) = window_weak.upgrade() {
                        w.imp().verify_dialog.replace(None);
                        if response == "cancel" {
                            w.imp().verify_banner.set_revealed(true);
                            let tx = tx2.clone();
                            glib::spawn_future_local(async move {
                                let _ = tx
                                    .send(MatrixCommand::CancelVerification {
                                        flow_id: String::new(),
                                    })
                                    .await;
                            });
                        }
                    }
                });
                window.imp().verify_dialog.replace(Some(dialog));
                glib::spawn_future_local(async move {
                    let _ = tx
                        .send(MatrixCommand::RequestSelfVerification)
                        .await;
                });
            })
            .build();

        let recover_action = ActionEntryBuilder::new("recover-keys")
            .activate(|window: &Self, _, _| {
                let tx = window.imp().command_tx.get().unwrap().clone();
                crate::widgets::verification_dialog::show_recovery_key_dialog(window, tx);
            })
            .build();

        let import_keys_action = ActionEntryBuilder::new("import-keys")
            .activate(|window: &Self, _, _| {
                let tx = window.imp().command_tx.get().unwrap().clone();
                let window = window.clone();
                glib::spawn_future_local(async move {
                    show_import_keys_dialog(&window, tx).await;
                });
            })
            .build();

        let logout_action = ActionEntryBuilder::new("logout")
            .activate(|window: &Self, _, _| {
                let tx = window.imp().command_tx.get().unwrap().clone();
                glib::spawn_future_local(async move {
                    let _ = tx.send(MatrixCommand::Logout).await;
                });
            })
            .build();

        let join_action = ActionEntryBuilder::new("join-room")
            .activate(|window: &Self, _, _| {
                if window.imp().content_page.get().is_none() { return; }
                if window.imp().directory_dialog.borrow().is_some() { return; }
                let tab = window.imp().room_list_view.visible_tab();
                match tab.as_deref() {
                    Some("messages") => {
                        window.show_new_dm_bar();
                    }
                    Some("spaces") => {
                        // If the user has drilled into a space, scope the dialog to it.
                        let scoped = window.imp().room_list_view.current_space_id()
                            .and_then(|id| {
                                let reg = window.imp().room_list_view.imp().room_registry.borrow();
                                reg.get(&id).map(|o| (id.clone(), o.name().to_string()))
                            });
                        window.show_space_directory(scoped);
                    }
                    _ => {
                        // "rooms" tab or fallback: global browser.
                        window.show_space_directory(None);
                    }
                }
            })
            .build();

        let shortcuts_action = ActionEntryBuilder::new("shortcuts")
            .activate(|window: &Self, _, _| {
                show_shortcuts_window(window);
            })
            .build();

        let prev_room_action = ActionEntryBuilder::new("prev-room")
            .activate(|window: &Self, _, _| {
                if window.imp().content_page.get().is_none() { return; }
                let imp = window.imp();
                let current = imp.current_room_id.borrow().clone().unwrap_or_default();
                imp.room_list_view.navigate_room(&current, -1);
            })
            .build();

        let next_room_action = ActionEntryBuilder::new("next-room")
            .activate(|window: &Self, _, _| {
                if window.imp().content_page.get().is_none() { return; }
                let imp = window.imp();
                let current = imp.current_room_id.borrow().clone().unwrap_or_default();
                imp.room_list_view.navigate_room(&current, 1);
            })
            .build();

        self.add_action_entries([about_action, preferences_action, verify_action, recover_action, import_keys_action, logout_action, join_action, shortcuts_action, prev_room_action, next_room_action]);

        // Register app.open-room on the *application* so desktop notification
        // clicks (which fire app.* actions, not win.* actions) can navigate to
        // the source room. Takes a single string variant: the room ID.
        let window_weak = self.downgrade();
        let open_room_action = gio::SimpleAction::new("open-room", Some(glib::VariantTy::STRING));
        open_room_action.connect_activate(move |_, param| {
            let Some(win) = window_weak.upgrade() else { return };
            let Some(room_id) = param.and_then(|p| p.get::<String>()) else { return };
            win.present();
            let reg = win.imp().room_list_view.imp().room_registry.borrow();
            if let Some(obj) = reg.get(&room_id) {
                let name = obj.name();
                drop(reg);
                if let Some(ref cb) = *win.imp().room_list_view.imp().on_room_selected.borrow() {
                    cb(room_id, name);
                }
            }
        });
        if let Some(app) = gtk::prelude::GtkWindowExt::application(self) {
            use gio::prelude::ActionMapExt;
            app.add_action(&open_room_action);
        }
    }

    /// Navigate to a room identified by a Matrix ID or alias.
    /// If the room is already joined, opens it directly.
    /// Otherwise shows the join bar pre-filled with the identifier.
    pub fn handle_matrix_link(&self, identifier: &str) {
        // User-id form (@user:server) — open the user-info dialog
        // instead of trying to join it as a room. Matrix URIs returned
        // by parse_matrix_uri are prefixed with @, !, or # so the
        // first character reliably disambiguates the three kinds.
        if identifier.starts_with('@') {
            self.show_user_info_dialog(identifier);
            return;
        }
        let imp = self.imp();
        // Check if we already have this room.
        let registry = imp.room_list_view.imp().room_registry.borrow();
        if let Some(obj) = registry.values().find(|o| o.room_id() == identifier) {
            let room_id = obj.room_id();
            let name = obj.name();
            drop(registry);
            if let Some(ref cb) = *imp.room_list_view.imp().on_room_selected.borrow() {
                cb(room_id, name);
            }
            return;
        }
        drop(registry);
        // Not joined — show the join bar pre-filled.
        self.show_join_bar_with(Some(identifier));
    }

    fn show_join_bar_with(&self, initial: Option<&str>) {
        let imp = self.imp();
        if imp.inline_bar_active.get() { return; }
        imp.inline_bar_active.set(true);

        let toast_overlay = imp.toast_overlay.clone();
        let tx = imp.command_tx.get().unwrap().clone();

        // Create an inline entry bar for the room ID/alias.
        let entry = gtk::Entry::builder()
            .placeholder_text("#room:server, !id:server, or https://matrix.to/…")
            .hexpand(true)
            .build();
        if let Some(text) = initial {
            entry.set_text(text);
        }
        let join_btn = gtk::Button::builder()
            .label("Join")
            .css_classes(["suggested-action"])
            .build();
        let cancel_btn = gtk::Button::builder()
            .label("Cancel")
            .build();

        let bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_start(8)
            .margin_end(8)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        bar.append(&entry);
        bar.append(&join_btn);
        bar.append(&cancel_btn);

        let revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .reveal_child(true)
            .child(&bar)
            .build();

        imp.room_list_view.prepend(&revealer);
        entry.grab_focus();

        let dismiss = {
            let win_weak = self.downgrade();
            let revealer = revealer.clone();
            move || {
                revealer.set_reveal_child(false);
                if let Some(win) = win_weak.upgrade() {
                    win.imp().inline_bar_active.set(false);
                }
                let r = revealer.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                    if let Some(parent) = r.parent() {
                        if let Some(b) = parent.downcast_ref::<gtk::Box>() {
                            b.remove(&r);
                        }
                    }
                });
            }
        };

        let tx2 = tx.clone();
        let entry2 = entry.clone();
        let dismiss2 = dismiss.clone();
        let toast2 = toast_overlay.clone();
        let do_join = move || {
            let raw = entry2.text().to_string();
            if raw.is_empty() { return; }
            // Parse matrix.to / matrix: links into canonical room IDs/aliases.
            let room_id_or_alias = parse_matrix_link_or_id(&raw).unwrap_or(raw);
            let tx = tx2.clone();
            let dismiss = dismiss2.clone();
            let _toast = toast2.clone();
            glib::spawn_future_local(async move {
                let _ = tx.send(MatrixCommand::JoinRoom { room_id_or_alias, via_servers: vec![] }).await;
                dismiss();
            });
        };

        let join_fn = do_join.clone();
        join_btn.connect_clicked(move |_| join_fn());
        entry.connect_activate(move |_| do_join());

        cancel_btn.connect_clicked(move |_| dismiss());
    }

    /// Show an inline bar to start a new DM with a Matrix user.
    /// Open the user-info dialog for a Matrix user id. Displays a 64px
    /// avatar (cached image or initials fallback), the display name (if
    /// known), the mxid as selectable monospace text with a "Copy mxid"
    /// action, and a "Send DM" action that reuses the existing CreateDm
    /// flow. Opened from a sender-name click in the message view and
    /// from matrix.to user-link clicks (future issue).
    pub fn show_user_info_dialog(&self, user_id: &str) {
        if user_id.is_empty() { return; }
        // Best-effort display-name lookup: the current room's member
        // list is the most reliable source (matches what the user is
        // already seeing). Fall back to localpart so the heading is
        // never empty even for members not yet fetched into the room.
        let display_name = {
            let members = self.imp().message_view.imp().room_members.borrow();
            members.iter()
                .find(|(_, _, uid)| uid == user_id)
                .map(|(_, name, _)| name.clone())
                .unwrap_or_else(|| {
                    user_id.trim_start_matches('@')
                        .split(':').next()
                        .unwrap_or(user_id)
                        .to_string()
                })
        };

        let dialog = adw::Dialog::builder()
            .title(&display_name)
            .content_width(360)
            .content_height(320)
            .build();

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(24).margin_end(24)
            .margin_top(24).margin_bottom(24)
            .build();

        let initials_source = display_name.clone();
        let avatar = adw::Avatar::builder()
            .size(64)
            .text(&initials_source)
            .show_initials(true)
            .halign(gtk::Align::Center)
            .build();
        if let Some(path) = self.imp().avatar_cache.borrow().get(user_id) {
            if !path.is_empty() {
                if let Ok(tex) = gtk::gdk::Texture::from_filename(path) {
                    avatar.set_custom_image(Some(&tex));
                }
            }
        }
        content.append(&avatar);

        let name_label = gtk::Label::builder()
            .label(&display_name)
            .css_classes(["title-2"])
            .halign(gtk::Align::Center)
            .build();
        content.append(&name_label);

        // mxid row: monospace, selectable, with a Copy button that
        // writes to the clipboard.
        let mxid_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .halign(gtk::Align::Center)
            .build();
        let mxid_label = gtk::Label::builder()
            .label(user_id)
            .selectable(true)
            .css_classes(["monospace", "dim-label"])
            .build();
        let copy_btn = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .tooltip_text("Copy Matrix address")
            .css_classes(["flat", "circular"])
            .build();
        let mxid_for_copy = user_id.to_string();
        let toast_overlay_for_copy = self.imp().toast_overlay.clone();
        copy_btn.connect_clicked(move |btn| {
            btn.display().clipboard().set_text(&mxid_for_copy);
            toast(&toast_overlay_for_copy, "Matrix address copied");
        });
        mxid_row.append(&mxid_label);
        mxid_row.append(&copy_btn);
        content.append(&mxid_row);

        // Spacer pushes the action buttons to the bottom of the dialog.
        let spacer = gtk::Box::builder().vexpand(true).build();
        content.append(&spacer);

        let actions = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::Fill)
            .homogeneous(true)
            .build();
        let dm_btn = gtk::Button::builder()
            .label("Send DM")
            .css_classes(["suggested-action"])
            .build();
        actions.append(&dm_btn);
        content.append(&actions);

        // Send-DM action: dispatch CreateDm, close the dialog.
        let tx_dm = self.imp().command_tx.get().unwrap().clone();
        let dialog_weak = dialog.downgrade();
        let uid_for_dm = user_id.to_string();
        dm_btn.connect_clicked(move |_| {
            let tx = tx_dm.clone();
            let uid = uid_for_dm.clone();
            let dlg = dialog_weak.clone();
            glib::spawn_future_local(async move {
                let _ = tx.send(MatrixCommand::CreateDm { user_id: uid }).await;
                if let Some(d) = dlg.upgrade() { d.close(); }
            });
        });

        dialog.set_child(Some(&content));
        dialog.present(Some(self));
    }

    fn show_new_dm_bar(&self) {
        let imp = self.imp();
        if imp.inline_bar_active.get() { return; }
        imp.inline_bar_active.set(true);

        let tx = imp.command_tx.get().unwrap().clone();

        let entry = gtk::Entry::builder()
            .placeholder_text("@user:server — or just user")
            .hexpand(true)
            .build();
        let start_btn = gtk::Button::builder()
            .label("Message")
            .css_classes(["suggested-action"])
            .build();
        // Close affordance: small icon-only X instead of a full-width
        // "Cancel" label. Saves bar width for the entry + primary action
        // and matches the standard GNOME pattern for dismissable inline
        // bars. Adding "flat" + "circular" CSS so it's visually subordinate.
        let cancel_btn = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Cancel")
            .css_classes(["flat", "circular"])
            .build();

        let bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_start(8)
            .margin_end(8)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        bar.append(&entry);
        bar.append(&start_btn);
        bar.append(&cancel_btn);

        let revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .reveal_child(true)
            .child(&bar)
            .build();

        imp.room_list_view.prepend(&revealer);
        entry.grab_focus();

        // Known servers for the completion popover. Order puts the user's
        // own homeserver first (most common target), then a curated
        // fallback list covering big community servers. A future refinement
        // (#14 phase 2) can extend this with servers actually observed in
        // joined-room members, but the fallback alone already unblocks the
        // common case of typing just a localpart.
        let known_servers: Vec<String> = {
            let uid = imp.user_id.borrow();
            let mut servers: Vec<String> = Vec::new();
            if let Some(own) = uid.splitn(2, ':').nth(1) {
                if !own.is_empty() {
                    servers.push(own.to_string());
                }
            }
            for fallback in [
                "matrix.org",
                "gnome.org",
                "kde.org",
                "mozilla.org",
                "fedora.im",
                "element.io",
                "tchncs.de",
            ] {
                if !servers.iter().any(|s| s == fallback) {
                    servers.push(fallback.to_string());
                }
            }
            servers
        };

        // Completion popover with a ListBox of "@local:server" rows.
        let completion_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(["boxed-list"])
            .build();
        let completion_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .max_content_height(240)
            .propagate_natural_height(true)
            .child(&completion_list)
            .build();
        let completion_popover = gtk::Popover::builder()
            .child(&completion_scroll)
            .has_arrow(false)
            .autohide(false)
            .build();
        completion_popover.set_parent(&entry);
        completion_popover.set_position(gtk::PositionType::Bottom);

        let dismiss = {
            let win_weak = self.downgrade();
            let revealer = revealer.clone();
            let popover = completion_popover.clone();
            move || {
                popover.popdown();
                popover.unparent();
                revealer.set_reveal_child(false);
                if let Some(win) = win_weak.upgrade() {
                    win.imp().inline_bar_active.set(false);
                }
                let r = revealer.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                    if let Some(parent) = r.parent() {
                        if let Some(b) = parent.downcast_ref::<gtk::Box>() {
                            b.remove(&r);
                        }
                    }
                });
            }
        };

        // Helper to submit a completed user id.
        let tx_submit = tx.clone();
        let dismiss_submit = dismiss.clone();
        let submit_user_id = move |user_id: String| {
            if user_id.is_empty() { return; }
            let tx = tx_submit.clone();
            let dismiss = dismiss_submit.clone();
            glib::spawn_future_local(async move {
                let _ = tx.send(MatrixCommand::CreateDm { user_id }).await;
                dismiss();
            });
        };

        // Rebuild the completion list on text-changed. If the text already
        // contains a ':' we treat it as a full user id and hide the popover
        // (the user knows what they want). If it's empty, hide. Otherwise
        // list @localpart:server for each known server.
        let popover_for_changed = completion_popover.clone();
        let list_for_changed = completion_list.clone();
        let servers_for_changed = known_servers.clone();
        let submit_from_list = submit_user_id.clone();
        entry.connect_changed(move |e| {
            let raw = e.text().to_string();
            let trimmed = raw.trim_start_matches('@').to_string();
            if trimmed.is_empty() || trimmed.contains(':') {
                popover_for_changed.popdown();
                return;
            }
            // Clear existing rows.
            while let Some(child) = list_for_changed.first_child() {
                list_for_changed.remove(&child);
            }
            for server in &servers_for_changed {
                let full = format!("@{trimmed}:{server}");
                let row = gtk::ListBoxRow::builder()
                    .activatable(true)
                    .build();
                let label = gtk::Label::builder()
                    .label(&full)
                    .halign(gtk::Align::Start)
                    .margin_start(8).margin_end(8).margin_top(4).margin_bottom(4)
                    .build();
                row.set_child(Some(&label));
                let full_clone = full.clone();
                let submit = submit_from_list.clone();
                let popover = popover_for_changed.clone();
                row.connect_activate(move |_| {
                    popover.popdown();
                    submit(full_clone.clone());
                });
                list_for_changed.append(&row);
            }
            // Select the first row so Arrow-Down / Enter behave naturally.
            if let Some(first) = list_for_changed.row_at_index(0) {
                list_for_changed.select_row(Some(&first));
            }
            popover_for_changed.popup();
        });

        // Submit from the Message button or raw Enter without a list pick.
        let entry_for_start = entry.clone();
        let submit_for_start = submit_user_id.clone();
        let list_for_start = completion_list.clone();
        let do_start = move || {
            let text = entry_for_start.text().to_string();
            if text.is_empty() { return; }
            // If the text lacks ':' try the highlighted completion row first.
            if !text.contains(':') {
                if let Some(selected) = list_for_start.selected_row() {
                    if let Some(label) = selected.child().and_then(|c| c.downcast::<gtk::Label>().ok()) {
                        submit_for_start(label.label().to_string());
                        return;
                    }
                }
            }
            submit_for_start(text);
        };

        let start_fn = do_start.clone();
        start_btn.connect_clicked(move |_| start_fn());
        entry.connect_activate(move |_| do_start());

        cancel_btn.connect_clicked(move |_| dismiss());
    }

    /// `PublicRoomDirectory` arrived: update the global-search results list in-place.
    fn show_or_update_directory(&self, _title: &str, rooms: &[crate::matrix::SpaceDirectoryRoom]) {
        let imp = self.imp();
        if let Some(list_box) = imp.directory_list_box.borrow().as_ref() {
            Self::populate_directory_list(
                list_box,
                rooms,
                imp.command_tx.get().unwrap(),
                &mut imp.directory_join_buttons.borrow_mut(),
                None,
            );
            if !rooms.is_empty() {
                if let Some(stack) = imp.directory_stack.borrow().as_ref() {
                    stack.set_visible_child_name("results");
                }
            }
        }
    }

    /// `SpaceDirectory` arrived: populate the matching expander (or scoped list).
    fn populate_space_expander(&self, space_id: &str, rooms: &[crate::matrix::SpaceDirectoryRoom]) {
        let imp = self.imp();
        // Try directory_space_expanders first (new tuple type), then scoped list_box.
        let (list_box, space_ctx) = {
            let exp = imp.directory_space_expanders.borrow();
            if let Some((lb, ctx)) = exp.get(space_id) {
                (lb.clone(), ctx.clone())
            } else {
                match imp.directory_list_box.borrow().clone() {
                    Some(lb) => (lb, None),
                    None => return,
                }
            }
        };

        Self::populate_directory_list(
            &list_box,
            rooms,
            imp.command_tx.get().unwrap(),
            &mut imp.directory_join_buttons.borrow_mut(),
            space_ctx.as_ref().map(|(id, via)| (id.as_str(), via.as_slice())),
        );
        if !rooms.is_empty() {
            if let Some(stack) = imp.directory_stack.borrow().as_ref() {
                stack.set_visible_child_name("results");
            }
        }
    }

    /// Fill a ListBox with directory room rows.
    fn populate_directory_list(
        list_box: &gtk::ListBox,
        rooms: &[crate::matrix::SpaceDirectoryRoom],
        tx: &async_channel::Sender<MatrixCommand>,
        join_buttons: &mut std::collections::HashMap<String, gtk::Button>,
        space_context: Option<(&str, &[String])>,
    ) {
        // Remove old rows.
        while let Some(child) = list_box.first_child() {
            list_box.remove(&child);
        }
        join_buttons.retain(|k, _| rooms.iter().any(|r| &r.room_id == k));

        // Always show a header clarifying these are rooms inside the space.
        let header_text = if space_context.is_some() {
            "These are individual rooms inside the space. Joining a room will also join the space."
        } else {
            "Individual rooms inside this space — join each one separately."
        };
        let banner_row = adw::ActionRow::builder()
            .title(header_text)
            .css_classes(["dim-label"])
            .activatable(false)
            .build();
        list_box.append(&banner_row);

        for room in rooms {
            let safe_name = glib::markup_escape_text(&room.name);
            let safe_topic = glib::markup_escape_text(&room.topic);
            let subtitle = if safe_topic.is_empty() {
                format!("{} members", room.member_count)
            } else {
                format!("{safe_topic} — {} members", room.member_count)
            };
            let row = adw::ActionRow::builder()
                .title(safe_name.as_str())
                .subtitle(&subtitle)
                .activatable(false)
                .build();

            if room.already_joined {
                row.add_suffix(&gtk::Label::builder()
                    .label("Joined")
                    .css_classes(["dim-label", "caption"])
                    .build());
            } else {
                let join_btn = gtk::Button::builder()
                    .label("Join")
                    .css_classes(["suggested-action"])
                    .valign(gtk::Align::Center)
                    .build();
                let tx = tx.clone();
                // Prefer alias for joining — alias resolution returns live
                // federation servers, avoiding "no known servers" errors.
                let room_id_or_alias = room.canonical_alias.clone()
                    .unwrap_or_else(|| room.room_id.clone());
                let room_via: Vec<String> = room.via_servers.clone();
                // Capture space context for joined-space-then-room.
                let space_join: Option<(String, Vec<String>)> = space_context.map(|(sid, svia)| {
                    (sid.to_string(), svia.to_vec())
                });
                join_btn.connect_clicked(move |btn| {
                    btn.set_sensitive(false);
                    btn.set_label("Joining…");
                    let tx = tx.clone();
                    let roa = room_id_or_alias.clone();
                    let via = room_via.clone();
                    let sj = space_join.clone();
                    glib::spawn_future_local(async move {
                        // If space not yet joined, join it first.
                        if let Some((space_id, space_via)) = sj {
                            let _ = tx.send(MatrixCommand::JoinRoom {
                                room_id_or_alias: space_id,
                                via_servers: space_via,
                            }).await;
                        }
                        let _ = tx.send(MatrixCommand::JoinRoom {
                            room_id_or_alias: roa,
                            via_servers: via,
                        }).await;
                    });
                });
                join_buttons.insert(room.room_id.clone(), join_btn.clone());
                row.add_suffix(&join_btn);
            }
            list_box.append(&row);
        }
    }

    /// Open the room/space browser dialog.
    ///
    /// `scoped_space`: when `Some((space_id, space_name))` the dialog opens
    /// pre-constrained to that space.  `None` opens the full browser with
    /// per-joined-space expanders plus a global public-directory search.
    fn show_space_directory(&self, scoped_space: Option<(String, String)>) {
        let imp = self.imp();
        let tx = imp.command_tx.get().unwrap().clone();

        let homeserver = {
            let uid = imp.user_id.borrow();
            uid.splitn(2, ':').nth(1).unwrap_or("").to_string()
        };

        let (dialog_title, placeholder) = match &scoped_space {
            Some((_, name)) => (
                format!("Rooms in {name}"),
                "Filter rooms, or paste a room ID / matrix.to link + Enter…",
            ),
            None => (
                "Join a Room or Space".to_string(),
                "Search rooms and spaces, or paste a matrix.to link + Enter…",
            ),
        };

        let dialog = adw::Dialog::builder()
            .title(&dialog_title)
            .content_width(480)
            .content_height(600)
            .build();

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();

        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text(placeholder)
            .hexpand(true)
            .build();
        header.set_title_widget(Some(&search_entry));
        toolbar.add_top_bar(&header);

        // Direct-join path: when the user types a room ID / alias / matrix.to
        // link into the search bar and presses Enter, trigger a join rather
        // than client-side filtering an empty result set. This is what the
        // placeholder advertises ("paste a matrix.to link"). Without this,
        // tombstone replacement rooms created after the last directory fetch
        // are impossible to reach from the + dialog — the server-indexed
        // public directory doesn't know about them yet, so filtering finds
        // nothing and there is no other affordance.
        {
            let tx = tx.clone();
            let dialog_weak = dialog.downgrade();
            search_entry.connect_activate(move |se| {
                let raw = se.text().to_string();
                let Some(canonical) = parse_matrix_link_or_id(&raw) else {
                    return;
                };
                let tx = tx.clone();
                let dialog_weak = dialog_weak.clone();
                glib::spawn_future_local(async move {
                    let _ = tx.send(MatrixCommand::JoinRoom {
                        room_id_or_alias: canonical,
                        via_servers: vec![],
                    }).await;
                    if let Some(d) = dialog_weak.upgrade() {
                        d.close();
                    }
                });
            });
        }

        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .vexpand(true)
            .build();

        // ── Scoped mode: show one space's rooms with a filter ─────────────────
        if let Some((space_id, _space_name)) = &scoped_space {
            let list_box = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .css_classes(["boxed-list"])
                .margin_start(12).margin_end(12)
                .margin_top(6).margin_bottom(12)
                .build();

            let spinner = gtk::Spinner::builder().spinning(true).build();
            let spinner_row = adw::ActionRow::builder()
                .title("Loading rooms…")
                .build();
            spinner_row.add_suffix(&spinner);
            list_box.append(&spinner_row);

            let scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&list_box)
                .build();
            content_box.append(&scroll);
            toolbar.set_content(Some(&content_box));

            *imp.directory_dialog.borrow_mut() = Some(dialog.clone());
            *imp.directory_list_box.borrow_mut() = Some(list_box.clone());

            // Filter in-place as user types (client-side, after first load).
            let lb = list_box.clone();
            search_entry.connect_search_changed(move |se| {
                let q = se.text().to_string().to_lowercase();
                let mut child = lb.first_child();
                while let Some(w) = child {
                    child = w.next_sibling();
                    if let Some(row) = w.downcast_ref::<adw::ActionRow>() {
                        row.set_visible(q.is_empty() ||
                            row.title().to_lowercase().contains(&q) ||
                            row.subtitle().unwrap_or_default().to_lowercase().contains(&q));
                    }
                }
            });

            let win_weak = self.downgrade();
            dialog.connect_closed(move |_| {
                if let Some(win) = win_weak.upgrade() {
                    *win.imp().directory_dialog.borrow_mut() = None;
                    *win.imp().directory_list_box.borrow_mut() = None;
                    win.imp().directory_join_buttons.borrow_mut().clear();
                    *win.imp().directory_stack.borrow_mut() = None;
                    win.imp().directory_space_expanders.borrow_mut().clear();
                }
            });

            dialog.set_child(Some(&toolbar));
            let se = search_entry.clone();
            dialog.connect_map(move |_| { se.grab_focus(); });
            dialog.present(Some(self));

            // Fetch space rooms immediately.
            let sid = space_id.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(50), move || {
                let tx2 = tx.clone();
                glib::spawn_future_local(async move {
                    let _ = tx2.send(MatrixCommand::BrowseSpaceRooms { space_id: sid }).await;
                });
            });
            return;
        }

        // ── Global mode: hierarchical server → space → room ─────────────────────

        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();

        let outer_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_start(12).margin_end(12)
            .margin_top(6).margin_bottom(12)
            .spacing(12)
            .build();
        scroll.set_child(Some(&outer_box));
        content_box.append(&scroll);
        toolbar.set_content(Some(&content_box));

        // Group joined spaces by their server.
        let joined = imp.room_list_view.joined_spaces();
        let mut server_map: std::collections::BTreeMap<String, Vec<(String, String)>> =
            std::collections::BTreeMap::new();
        for (space_id, space_name) in joined {
            let server = space_id.splitn(2, ':').nth(1).unwrap_or("unknown").to_string();
            server_map.entry(server).or_default().push((space_id, space_name));
        }
        // Always include the user's homeserver even if no joined spaces there.
        server_map.entry(homeserver.clone()).or_default();

        for (server, spaces) in &server_map {
            let group = adw::PreferencesGroup::builder()
                .title(server)
                .build();

            // Joined spaces shown immediately.
            for (space_id, space_name) in spaces {
                Self::make_space_expander(
                    &group,
                    space_id,
                    space_name,
                    true,  // joined
                    None,  // no space join needed
                    &tx,
                    &imp.directory_space_expanders,
                    &imp.directory_join_buttons,
                );
            }

            // "Browse more" row to load unjoined spaces from this server.
            let browse_row = adw::ActionRow::builder()
                .title("Browse more spaces on this server…")
                .activatable(false)
                .build();
            let load_btn = gtk::Button::builder()
                .label("Load")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            let tx2 = tx.clone();
            let srv = server.clone();
            load_btn.connect_clicked(move |_| {
                let tx3 = tx2.clone();
                let s = srv.clone();
                glib::spawn_future_local(async move {
                    let _ = tx3.send(MatrixCommand::BrowsePublicRooms {
                        search_term: None,
                        spaces_only: true,
                        server: Some(s),
                    }).await;
                });
            });
            browse_row.add_suffix(&load_btn);
            group.add(&browse_row);

            outer_box.append(&group);
            imp.directory_server_groups.borrow_mut().insert(server.clone(), group);
        }

        // EntryRow to browse a completely new server.
        let add_server_group = adw::PreferencesGroup::new();
        let server_entry = adw::EntryRow::builder()
            .title("Browse another server…")
            .show_apply_button(true)
            .build();
        let tx2 = tx.clone();
        let win_weak2 = self.downgrade();
        let outer_box2 = outer_box.clone();
        server_entry.connect_apply(move |entry| {
            let srv = entry.text().to_string();
            if srv.is_empty() { return; }
            entry.set_text("");
            if let Some(win) = win_weak2.upgrade() {
                let imp2 = win.imp();
                if imp2.directory_server_groups.borrow().contains_key(&srv) {
                    // Server already present — just trigger a fetch.
                } else {
                    let group = adw::PreferencesGroup::builder().title(&srv).build();
                    let browse_row = adw::ActionRow::builder()
                        .title("Browse spaces on this server…")
                        .activatable(false)
                        .build();
                    let load_btn2 = gtk::Button::builder()
                        .label("Load")
                        .valign(gtk::Align::Center)
                        .css_classes(["flat"])
                        .build();
                    let tx3 = tx2.clone();
                    let srv2 = srv.clone();
                    load_btn2.connect_clicked(move |_| {
                        let tx4 = tx3.clone();
                        let s = srv2.clone();
                        glib::spawn_future_local(async move {
                            let _ = tx4.send(MatrixCommand::BrowsePublicRooms {
                                search_term: None,
                                spaces_only: true,
                                server: Some(s),
                            }).await;
                        });
                    });
                    browse_row.add_suffix(&load_btn2);
                    group.add(&browse_row);
                    // Insert before the "add server" group (the last child).
                    // We want the new server group to appear above it, so insert
                    // after the second-to-last child (or prepend if there's only one).
                    let before_last = outer_box2.last_child()
                        .and_then(|last| last.prev_sibling());
                    outer_box2.insert_child_after(&group, before_last.as_ref().map(|w| w as &gtk::Widget));
                    imp2.directory_server_groups.borrow_mut().insert(srv.clone(), group);
                }
            }
            let tx3 = tx2.clone();
            glib::spawn_future_local(async move {
                let _ = tx3.send(MatrixCommand::BrowsePublicRooms {
                    search_term: None,
                    spaces_only: true,
                    server: Some(srv),
                }).await;
            });
        });
        add_server_group.add(&server_entry);
        outer_box.append(&add_server_group);

        // Search bar filters space/room names client-side.
        let outer_box3 = outer_box.clone();
        search_entry.connect_search_changed(move |se| {
            let q = se.text().to_string().to_lowercase();
            let mut child = outer_box3.first_child();
            while let Some(node) = child {
                child = node.next_sibling();
                if let Some(group) = node.downcast_ref::<adw::PreferencesGroup>() {
                    let mut gc = group.first_child();
                    while let Some(gnode) = gc {
                        gc = gnode.next_sibling();
                        if let Some(er) = gnode.downcast_ref::<adw::ExpanderRow>() {
                            let title = er.title().to_lowercase();
                            er.set_visible(q.is_empty() || title.contains(&q));
                        }
                    }
                }
            }
        });

        *imp.directory_dialog.borrow_mut() = Some(dialog.clone());
        *imp.directory_list_box.borrow_mut() = None;
        *imp.directory_stack.borrow_mut() = None;
        *imp.directory_outer_box.borrow_mut() = Some(outer_box);

        let win_weak = self.downgrade();
        dialog.connect_closed(move |_| {
            if let Some(win) = win_weak.upgrade() {
                *win.imp().directory_dialog.borrow_mut() = None;
                *win.imp().directory_list_box.borrow_mut() = None;
                *win.imp().directory_stack.borrow_mut() = None;
                *win.imp().directory_outer_box.borrow_mut() = None;
                win.imp().directory_join_buttons.borrow_mut().clear();
                win.imp().directory_space_expanders.borrow_mut().clear();
                win.imp().directory_server_groups.borrow_mut().clear();
            }
        });

        dialog.set_child(Some(&toolbar));
        let se = search_entry.clone();
        dialog.connect_map(move |_| { se.grab_focus(); });
        dialog.present(Some(self));
    }

    /// Creates an adw::ExpanderRow for a space, adds it to `group`, stores the
    /// inner ListBox in `expanders`. If `space_join_ctx` is Some, rooms shown
    /// inside will also join the space on click.
    fn make_space_expander(
        group: &adw::PreferencesGroup,
        space_id: &str,
        space_name: &str,
        space_joined: bool,
        space_join_ctx: Option<(String, Vec<String>)>,
        tx: &async_channel::Sender<MatrixCommand>,
        expanders: &std::cell::RefCell<std::collections::HashMap<String, (gtk::ListBox, Option<(String, Vec<String>)>)>>,
        _join_buttons: &std::cell::RefCell<std::collections::HashMap<String, gtk::Button>>,
    ) {
        let expander = adw::ExpanderRow::builder()
            .title(space_name)
            .subtitle(if space_joined { "Joined" } else { "Expand to browse rooms" })
            .build();

        // For unjoined spaces: add a "Join Space" button on the expander row itself
        // so the user can join the space independently (needed for restricted rooms).
        if !space_joined {
            if let Some((space_id_or_alias, space_via)) = space_join_ctx.clone() {
                let join_space_btn = gtk::Button::builder()
                    .label("Join Space")
                    .css_classes(["suggested-action"])
                    .valign(gtk::Align::Center)
                    .tooltip_text("Join this space (required for member-only rooms)")
                    .build();
                let tx_js = tx.clone();
                join_space_btn.connect_clicked(move |btn| {
                    btn.set_sensitive(false);
                    btn.set_label("Joining…");
                    let tx2 = tx_js.clone();
                    let roa = space_id_or_alias.clone();
                    let via = space_via.clone();
                    glib::spawn_future_local(async move {
                        let _ = tx2.send(MatrixCommand::JoinRoom {
                            room_id_or_alias: roa,
                            via_servers: via,
                        }).await;
                    });
                });
                expander.add_action(&join_space_btn);
            }
        }

        let inner_lb = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .margin_start(12).margin_end(12)
            .margin_top(4).margin_bottom(4)
            .build();

        let spinner_row = adw::ActionRow::builder().title("Loading…").build();
        spinner_row.add_suffix(&gtk::Spinner::builder().spinning(true).build());
        inner_lb.append(&spinner_row);
        expander.add_row(&inner_lb);
        group.add(&expander);

        expanders.borrow_mut().insert(
            space_id.to_string(),
            (inner_lb, space_join_ctx),
        );

        let tx2 = tx.clone();
        let sid = space_id.to_string();
        let already_fetched = std::rc::Rc::new(std::cell::Cell::new(false));
        expander.connect_expanded_notify(move |exp| {
            if exp.is_expanded() && !already_fetched.get() {
                already_fetched.set(true);
                let tx3 = tx2.clone();
                let s = sid.clone();
                glib::spawn_future_local(async move {
                    let _ = tx3.send(MatrixCommand::BrowseSpaceRooms { space_id: s }).await;
                });
            }
        });
    }

    /// Called when PublicSpacesForServer arrives — adds ExpanderRows for spaces
    /// not already shown under the server's PreferencesGroup.
    fn add_browsed_spaces_to_server(&self, server: &str, spaces: &[crate::matrix::SpaceDirectoryRoom]) {
        let imp = self.imp();
        let group = {
            let groups = imp.directory_server_groups.borrow();
            groups.get(server).cloned()
        };
        let Some(group) = group else { return };

        let tx = imp.command_tx.get().unwrap().clone();

        for space in spaces {
            // Skip if already shown.
            if imp.directory_space_expanders.borrow().contains_key(&space.room_id) {
                continue;
            }
            // Only add unjoined spaces here — joined ones are shown at open time.
            if space.already_joined { continue; }

            let space_via: Vec<String> = space.via_servers.clone();
            let space_id_or_alias = space.canonical_alias.clone()
                .unwrap_or_else(|| space.room_id.clone());

            Self::make_space_expander(
                &group,
                &space.room_id,
                &space.name,
                false,  // not joined
                Some((space_id_or_alias, space_via)),
                &tx,
                &imp.directory_space_expanders,
                &imp.directory_join_buttons,
            );
        }
    }

    fn show_room_details(&self) {
        let imp = self.imp();
        let meta = imp.current_room_meta.borrow();
        let Some(ref meta) = *meta else { return };
        let room_name = imp.content_page.get()
            .map(|p| p.title().to_string())
            .unwrap_or_else(|| "Room".to_string());
        let room_id = imp.current_room_id.borrow().clone().unwrap_or_default();

        // Clear previous content.
        let container = &imp.details_content;
        while let Some(child) = container.first_child() {
            container.remove(&child);
        }

        container.set_margin_start(12);
        container.set_margin_end(12);
        container.set_margin_top(12);
        container.set_margin_bottom(12);
        container.set_spacing(8);

        // Room name.
        let name_label = gtk::Label::builder()
            .label(&room_name)
            .css_classes(["title-3"])
            .halign(gtk::Align::Start)
            .wrap(true)
            .build();
        container.append(&name_label);

        // Room ID.
        let id_label = gtk::Label::builder()
            .label(&room_id)
            .css_classes(["caption", "dim-label"])
            .halign(gtk::Align::Start)
            .selectable(true)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::Char)
            .build();
        container.append(&id_label);

        // Topic.
        if !meta.topic.is_empty() {
            container.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            let topic_label = gtk::Label::builder()
                .label(&meta.topic)
                .halign(gtk::Align::Start)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .css_classes(["body"])
                .build();
            container.append(&topic_label);
        }

        container.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        // Info rows.
        let info = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .build();

        let add_info_row = |parent: &gtk::Box, label: &str, value: &str| {
            let row = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(8)
                .build();
            row.append(&gtk::Label::builder()
                .label(label)
                .css_classes(["dim-label", "caption"])
                .halign(gtk::Align::Start)
                .hexpand(true)
                .build());
            row.append(&gtk::Label::builder()
                .label(value)
                .css_classes(["caption"])
                .halign(gtk::Align::End)
                .build());
            parent.append(&row);
        };

        add_info_row(&info, "Members", &meta.member_count.to_string());
        add_info_row(&info, "Encrypted", if meta.is_encrypted { "Yes" } else { "No" });
        if meta.is_tombstoned {
            add_info_row(&info, "Status", "Upgraded");
            if let Some(ref name) = meta.replacement_room_name {
                add_info_row(&info, "New room", name);
            }
        }
        container.append(&info);

        // Media preview toggle — walks the context chain (room → space → global).
        {
            container.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

            // Determine current override level and resolved state.
            let registry = imp.room_list_view.imp().room_registry.borrow();
            let room_obj = registry.get(&room_id);
            use crate::room_context::CtxValue;
            let room_override = room_obj.map(|o| o.ctx_no_media()).unwrap_or(CtxValue::Inherit);
            let parent_space_id = room_obj.map(|o| o.parent_space_id()).unwrap_or_default();
            let space_override = if !parent_space_id.is_empty() {
                registry.get(&parent_space_id).map(|o| o.ctx_no_media()).unwrap_or(CtxValue::Inherit)
            } else { CtxValue::Inherit };
            drop(registry);

            let resolved_no_media = match room_override {
                CtxValue::NoMedia => true,
                CtxValue::ShowMedia => false,
                CtxValue::Inherit => space_override == CtxValue::NoMedia,
            };

            let source_note = match room_override {
                CtxValue::Inherit if space_override != CtxValue::Inherit => "Inherited from space",
                CtxValue::Inherit => "Default",
                _ => "Room setting",
            };

            let media_row = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(8)
                .build();
            let label_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(2)
                .hexpand(true)
                .build();
            label_box.append(&gtk::Label::builder()
                .label("Show media previews")
                .halign(gtk::Align::Start)
                .css_classes(["body"])
                .build());
            label_box.append(&gtk::Label::builder()
                .label(source_note)
                .halign(gtk::Align::Start)
                .css_classes(["caption", "dim-label"])
                .build());
            media_row.append(&label_box);

            let media_switch = gtk::Switch::builder()
                .active(!resolved_no_media)
                .valign(gtk::Align::Center)
                .build();
            let rid_toggle = room_id.clone();
            let rl_toggle = imp.room_list_view.clone();
            let msg_view_toggle = imp.message_view.clone();
            media_switch.connect_state_set(move |_, show| {
                // Toggle at the room level (user explicitly sets this room's preference).
                use crate::room_context::CtxValue;
                let value = if show { CtxValue::ShowMedia } else { CtxValue::NoMedia };
                rl_toggle.set_context_override(&rid_toggle, value);
                msg_view_toggle.set_no_media(!show);
                glib::Propagation::Proceed
            });
            media_row.append(&media_switch);
            container.append(&media_row);
        }

        // Invite button.
        {
            container.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            let invite_btn = gtk::Button::builder()
                .label("Invite User")
                .icon_name("list-add-symbolic")
                .css_classes(["pill"])
                .halign(gtk::Align::Start)
                .build();
            let tx = imp.command_tx.get().unwrap().clone();
            let rid = room_id.clone();
            // Local members (lowercase_name, display_name, user_id) for instant search.
            let local_members: Vec<(String, String, String)> = meta.members.iter()
                .map(|(uid, name)| (name.to_lowercase(), name.clone(), uid.clone()))
                .collect();
            let win_weak = self.downgrade();
            invite_btn.connect_clicked(move |btn| {
                build_invite_dialog(
                    btn,
                    &win_weak,
                    &tx,
                    &rid,
                    &local_members,
                );
            });
            container.append(&invite_btn);
        }

        // Members list.
        if !meta.members.is_empty() {
            container.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            // For large rooms we only show timeline participants, not all members.
            let header_text = if meta.members_fetched {
                format!("Members ({})", meta.members.len())
            } else {
                format!("Recent participants ({})", meta.members.len())
            };
            let members_header = gtk::Label::builder()
                .label(&header_text)
                .css_classes(["heading", "caption"])
                .halign(gtk::Align::Start)
                .build();
            container.append(&members_header);

            let my_id = imp.user_id.borrow().clone();
            for (uid, name) in meta.members.iter().take(50) {
                let row = gtk::Box::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .spacing(6)
                    .margin_top(2)
                    .margin_bottom(2)
                    .build();
                let info = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .spacing(1)
                    .hexpand(true)
                    .build();
                info.append(&gtk::Label::builder()
                    .label(name)
                    .halign(gtk::Align::Start)
                    .css_classes(["caption"])
                    .build());
                info.append(&gtk::Label::builder()
                    .label(uid)
                    .halign(gtk::Align::Start)
                    .css_classes(["caption", "dim-label"])
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .build());
                row.append(&info);
                // DM button — hidden for self.
                if *uid != my_id {
                    let dm_btn = gtk::Button::builder()
                        .icon_name("avatar-default-symbolic")
                        .tooltip_text("Direct Message")
                        .valign(gtk::Align::Center)
                        .build();
                    dm_btn.add_css_class("flat");
                    dm_btn.add_css_class("circular");
                    let tx = imp.command_tx.get().unwrap().clone();
                    let uid_for_dm = uid.clone();
                    let toast = imp.toast_overlay.clone();
                    dm_btn.connect_clicked(move |_| {
                        let t = adw::Toast::builder()
                            .title(&format!("Opening DM..."))
                            .timeout(3)
                            .build();
                        toast.add_toast(t);
                        let tx = tx.clone();
                        let uid = uid_for_dm.clone();
                        glib::spawn_future_local(async move {
                            let _ = tx.send(MatrixCommand::CreateDm { user_id: uid }).await;
                        });
                    });
                    row.append(&dm_btn);
                }
                container.append(&row);
            }
            if meta.members.len() > 50 {
                container.append(&gtk::Label::builder()
                    .label(&format!("… and {} more", meta.members.len() - 50))
                    .css_classes(["caption", "dim-label"])
                    .halign(gtk::Align::Start)
                    .build());
            }
        }

    }

    /// Show thread replies in the details sidebar.
    fn show_thread_sidebar(
        &self,
        root_message: &Option<crate::matrix::MessageInfo>,
        replies: &[crate::matrix::MessageInfo],
    ) {
        let imp = self.imp();
        let container = &imp.details_content;

        // Clear previous content.
        while let Some(child) = container.first_child() {
            container.remove(&child);
        }

        // Thread header.
        container.append(&gtk::Label::builder()
            .label("Thread")
            .halign(gtk::Align::Start)
            .css_classes(["title-4"])
            .margin_bottom(4)
            .build());

        // Root message.
        if let Some(root) = root_message {
            let root_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(2)
                .css_classes(["card"])
                .margin_bottom(8)
                .build();
            root_box.append(&gtk::Label::builder()
                .label(&root.sender)
                .halign(gtk::Align::Start)
                .css_classes(["caption-heading"])
                .build());
            root_box.append(&gtk::Label::builder()
                .label(&root.body)
                .halign(gtk::Align::Start)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .css_classes(["body"])
                .build());
            container.append(&root_box);
        }

        if replies.is_empty() {
            container.append(&gtk::Label::builder()
                .label("No replies yet")
                .halign(gtk::Align::Start)
                .css_classes(["dim-label"])
                .build());
        } else {
            container.append(&gtk::Label::builder()
                .label(&format!("{} replies", replies.len()))
                .halign(gtk::Align::Start)
                .css_classes(["caption", "dim-label"])
                .margin_bottom(4)
                .build());

            for reply in replies {
                let msg_box = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .spacing(1)
                    .margin_top(4)
                    .margin_bottom(4)
                    .build();

                let header = gtk::Box::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .spacing(6)
                    .build();
                header.append(&gtk::Label::builder()
                    .label(&reply.sender)
                    .halign(gtk::Align::Start)
                    .css_classes(["caption-heading"])
                    .build());
                let ts = crate::widgets::message_row::format_timestamp(reply.timestamp);
                header.append(&gtk::Label::builder()
                    .label(&ts)
                    .halign(gtk::Align::End)
                    .hexpand(true)
                    .css_classes(["caption", "dim-label"])
                    .build());
                msg_box.append(&header);

                msg_box.append(&gtk::Label::builder()
                    .label(&reply.body)
                    .halign(gtk::Align::Start)
                    .wrap(true)
                    .wrap_mode(gtk::pango::WrapMode::WordChar)
                    .selectable(true)
                    .css_classes(["body"])
                    .build());

                container.append(&msg_box);
            }
        }

        // Show the sidebar.
        if let Some(sep) = imp.details_separator.get() {
            sep.set_visible(true);
        }
        imp.details_revealer.set_visible(true);
        imp.details_revealer.set_reveal_child(true);
    }

    fn show_about_dialog(&self) {
        let dialog = adw::AboutDialog::builder()
            .application_name(crate::config::APP_NAME)
            .application_icon(crate::config::APP_ID)
            .developer_name("Hikyaku Contributors")
            .version(env!("CARGO_PKG_VERSION"))
            .comments("A Matrix client built with Rust and libadwaita, designed around activity awareness.")
            .website("https://gitlab.gnome.org/ramkrishna/hikyaku")
            .license_type(gtk::License::Gpl30)
            .build();

        dialog.present(Some(self));
    }

    fn show_preferences(&self) {
        use crate::config;

        let dialog = adw::PreferencesDialog::new();
        let cfg = config::settings().clone();

        // --- Appearance group ---
        let appearance_group = adw::PreferencesGroup::builder()
            .title("Appearance")
            .description("Customize the look of the message view")
            .build();

        // Build a label showing the current font.
        let font_label = {
            let family = &cfg.appearance.font_family;
            let size = cfg.appearance.font_size;
            if family.is_empty() {
                format!("Default, {size}pt")
            } else {
                format!("{family} {size}pt")
            }
        };
        let font_row = adw::ActionRow::builder()
            .title("Message Font")
            .subtitle(&font_label)
            .activatable(true)
            .build();
        font_row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        appearance_group.add(&font_row);

        // Open a FontDialog when the row is activated.
        let cfg_for_font = cfg.clone();
        let window_ref = self.downgrade();
        font_row.connect_activated(glib::clone!(
            #[weak] font_row,
            move |_| {
                let font_dialog = gtk::FontDialog::new();
                // Set initial font from current config.
                let initial = {
                    let desc_str = if cfg_for_font.appearance.font_family.is_empty() {
                        format!("Sans {}", cfg_for_font.appearance.font_size)
                    } else {
                        format!("{} {}", cfg_for_font.appearance.font_family, cfg_for_font.appearance.font_size)
                    };
                    gtk::pango::FontDescription::from_string(&desc_str)
                };
                let parent = window_ref.upgrade();
                font_dialog.choose_font(
                    parent.as_ref(),
                    Some(&initial),
                    gio::Cancellable::NONE,
                    glib::clone!(
                        #[weak] font_row,
                        move |result| {
                            if let Ok(desc) = result {
                                let family = desc
                                    .family()
                                    .map(|f| f.to_string())
                                    .unwrap_or_default();
                                let size_pt = (desc.size() as f64
                                    / gtk::pango::SCALE as f64)
                                    .round() as u32;
                                let size_pt = size_pt.max(6).min(48);

                                // Update config and apply.
                                let mut new_cfg = config::settings().clone();
                                new_cfg.appearance.font_family = family.clone();
                                new_cfg.appearance.font_size = size_pt;
                                apply_font_css(&new_cfg.appearance);
                                if let Err(e) = config::save_settings(&new_cfg) {
                                    tracing::error!("Failed to save settings: {e}");
                                }

                                // Update the subtitle.
                                if family.is_empty() {
                                    font_row.set_subtitle(&format!("Default, {size_pt}pt"));
                                } else {
                                    font_row.set_subtitle(&format!("{family} {size_pt}pt"));
                                }
                            }
                        }
                    ),
                );
            }
        ));

        // Background tint row.
        fn tint_subtitle(color: &str, color2: &str, opacity: f64) -> String {
            if color.is_empty() { return "None".to_string(); }
            let pct = (opacity * 100.0).round();
            if color2.is_empty() {
                format!("{color} at {pct}%")
            } else {
                format!("{color} → {color2} at {pct}%")
            }
        }

        let tint_row = adw::ActionRow::builder()
            .title("Background Tint")
            .subtitle(tint_subtitle(
                &cfg.appearance.tint_color,
                &cfg.appearance.tint_color2,
                cfg.appearance.tint_opacity,
            ))
            .build();

        let color_btn = gtk::ColorDialogButton::builder()
            .tooltip_text("Start color")
            .valign(gtk::Align::Center)
            .build();
        color_btn.set_dialog(&gtk::ColorDialog::new());
        if !cfg.appearance.tint_color.is_empty() {
            if let Ok(c) = gtk::gdk::RGBA::parse(&cfg.appearance.tint_color) {
                color_btn.set_rgba(&c);
            }
        }

        let color2_btn = gtk::ColorDialogButton::builder()
            .tooltip_text("Gradient end color (optional)")
            .valign(gtk::Align::Center)
            .build();
        color2_btn.set_dialog(&gtk::ColorDialog::new());
        if !cfg.appearance.tint_color2.is_empty() {
            if let Ok(c) = gtk::gdk::RGBA::parse(&cfg.appearance.tint_color2) {
                color2_btn.set_rgba(&c);
            }
        }

        let opacity_scale = gtk::Scale::builder()
            .orientation(gtk::Orientation::Horizontal)
            .adjustment(&gtk::Adjustment::new(
                cfg.appearance.tint_opacity * 100.0, 0.0, 50.0, 1.0, 5.0, 0.0,
            ))
            .width_request(120)
            .valign(gtk::Align::Center)
            .draw_value(false)
            .build();

        let clear_btn = gtk::Button::builder()
            .icon_name("edit-clear-symbolic")
            .tooltip_text("Remove tint")
            .valign(gtk::Align::Center)
            .build();
        clear_btn.add_css_class("flat");

        tint_row.add_suffix(&color_btn);
        tint_row.add_suffix(&color2_btn);
        tint_row.add_suffix(&opacity_scale);
        tint_row.add_suffix(&clear_btn);
        appearance_group.add(&tint_row);

        // Helper: read hex from a ColorDialogButton.
        fn rgba_to_hex(btn: &gtk::ColorDialogButton) -> String {
            let c = btn.rgba();
            format!("#{:02x}{:02x}{:02x}",
                (c.red() * 255.0) as u8, (c.green() * 255.0) as u8, (c.blue() * 255.0) as u8)
        }

        // Wire color/opacity changes.
        let tint_row_for_color = tint_row.clone();
        let color2_for_color = color2_btn.clone();
        let scale_for_color = opacity_scale.clone();
        color_btn.connect_rgba_notify(move |btn| {
            let hex = rgba_to_hex(btn);
            let hex2 = rgba_to_hex(&color2_for_color);
            let opacity = scale_for_color.value() / 100.0;
            let mut new_cfg = config::settings().clone();
            new_cfg.appearance.tint_color = hex.clone();
            new_cfg.appearance.tint_color2 = hex2.clone();
            new_cfg.appearance.tint_opacity = opacity;
            apply_tint_css(&new_cfg.appearance);
            let _ = config::save_settings(&new_cfg);
            tint_row_for_color.set_subtitle(&tint_subtitle(&hex, &hex2, opacity));
        });

        let tint_row_for_color2 = tint_row.clone();
        let color_for_color2 = color_btn.clone();
        let scale_for_color2 = opacity_scale.clone();
        color2_btn.connect_rgba_notify(move |btn| {
            let hex = rgba_to_hex(&color_for_color2);
            let hex2 = rgba_to_hex(btn);
            let opacity = scale_for_color2.value() / 100.0;
            let mut new_cfg = config::settings().clone();
            new_cfg.appearance.tint_color = hex.clone();
            new_cfg.appearance.tint_color2 = hex2.clone();
            new_cfg.appearance.tint_opacity = opacity;
            apply_tint_css(&new_cfg.appearance);
            let _ = config::save_settings(&new_cfg);
            tint_row_for_color2.set_subtitle(&tint_subtitle(&hex, &hex2, opacity));
        });

        // Debounce opacity changes: rapid slider drags crash GTK's CSS engine
        // if load_from_string fires on every tick. Apply after 120ms idle.
        let tint_row_for_scale = tint_row.clone();
        let color_btn_for_scale = color_btn.clone();
        let color2_btn_for_scale = color2_btn.clone();
        let pending_scale: std::rc::Rc<std::cell::Cell<Option<glib::SourceId>>> =
            std::rc::Rc::new(std::cell::Cell::new(None));
        opacity_scale.connect_value_changed(move |scale| {
            let hex = rgba_to_hex(&color_btn_for_scale);
            let hex2 = rgba_to_hex(&color2_btn_for_scale);
            let opacity = scale.value() / 100.0;
            tint_row_for_scale.set_subtitle(&tint_subtitle(&hex, &hex2, opacity));
            if let Some(id) = pending_scale.take() { id.remove(); }
            let pending = pending_scale.clone();
            let id = glib::timeout_add_local_once(
                std::time::Duration::from_millis(120),
                move || {
                    pending.set(None);
                    let mut new_cfg = config::settings().clone();
                    new_cfg.appearance.tint_color = hex.clone();
                    new_cfg.appearance.tint_color2 = hex2.clone();
                    new_cfg.appearance.tint_opacity = opacity;
                    apply_tint_css(&new_cfg.appearance);
                    let _ = config::save_settings(&new_cfg);
                },
            );
            pending_scale.set(Some(id));
        });

        let tint_row_for_clear = tint_row.clone();
        clear_btn.connect_clicked(move |_| {
            let mut new_cfg = config::settings().clone();
            new_cfg.appearance.tint_color = String::new();
            new_cfg.appearance.tint_color2 = String::new();
            new_cfg.appearance.tint_opacity = 0.05;
            apply_tint_css(&new_cfg.appearance);
            let _ = config::save_settings(&new_cfg);
            tint_row_for_clear.set_subtitle("None");
        });

        // Sidebar tint row.
        let sidebar_tint_row = adw::ActionRow::builder()
            .title("Sidebar Tint")
            .subtitle(tint_subtitle(
                &cfg.appearance.sidebar_tint_color,
                &cfg.appearance.sidebar_tint_color2,
                cfg.appearance.sidebar_tint_opacity,
            ))
            .build();

        let sidebar_color_btn = gtk::ColorDialogButton::builder()
            .tooltip_text("Start color")
            .valign(gtk::Align::Center)
            .build();
        sidebar_color_btn.set_dialog(&gtk::ColorDialog::new());
        if !cfg.appearance.sidebar_tint_color.is_empty() {
            if let Ok(c) = gtk::gdk::RGBA::parse(&cfg.appearance.sidebar_tint_color) {
                sidebar_color_btn.set_rgba(&c);
            }
        }

        let sidebar_color2_btn = gtk::ColorDialogButton::builder()
            .tooltip_text("Gradient end color (optional)")
            .valign(gtk::Align::Center)
            .build();
        sidebar_color2_btn.set_dialog(&gtk::ColorDialog::new());
        if !cfg.appearance.sidebar_tint_color2.is_empty() {
            if let Ok(c) = gtk::gdk::RGBA::parse(&cfg.appearance.sidebar_tint_color2) {
                sidebar_color2_btn.set_rgba(&c);
            }
        }

        let sidebar_opacity_scale = gtk::Scale::builder()
            .orientation(gtk::Orientation::Horizontal)
            .adjustment(&gtk::Adjustment::new(
                cfg.appearance.sidebar_tint_opacity * 100.0, 0.0, 50.0, 1.0, 5.0, 0.0,
            ))
            .width_request(120)
            .valign(gtk::Align::Center)
            .draw_value(false)
            .build();

        let sidebar_clear_btn = gtk::Button::builder()
            .icon_name("edit-clear-symbolic")
            .tooltip_text("Remove sidebar tint")
            .valign(gtk::Align::Center)
            .build();
        sidebar_clear_btn.add_css_class("flat");

        sidebar_tint_row.add_suffix(&sidebar_color_btn);
        sidebar_tint_row.add_suffix(&sidebar_color2_btn);
        sidebar_tint_row.add_suffix(&sidebar_opacity_scale);
        sidebar_tint_row.add_suffix(&sidebar_clear_btn);
        appearance_group.add(&sidebar_tint_row);

        let sidebar_tint_row_for_color = sidebar_tint_row.clone();
        let sidebar_color2_for_color = sidebar_color2_btn.clone();
        let sidebar_scale_for_color = sidebar_opacity_scale.clone();
        sidebar_color_btn.connect_rgba_notify(move |btn| {
            let hex = rgba_to_hex(btn);
            let hex2 = rgba_to_hex(&sidebar_color2_for_color);
            let opacity = sidebar_scale_for_color.value() / 100.0;
            let mut new_cfg = config::settings().clone();
            new_cfg.appearance.sidebar_tint_color = hex.clone();
            new_cfg.appearance.sidebar_tint_color2 = hex2.clone();
            new_cfg.appearance.sidebar_tint_opacity = opacity;
            apply_tint_css(&new_cfg.appearance);
            let _ = config::save_settings(&new_cfg);
            sidebar_tint_row_for_color.set_subtitle(&tint_subtitle(&hex, &hex2, opacity));
        });

        let sidebar_tint_row_for_color2 = sidebar_tint_row.clone();
        let sidebar_color_for_color2 = sidebar_color_btn.clone();
        let sidebar_scale_for_color2 = sidebar_opacity_scale.clone();
        sidebar_color2_btn.connect_rgba_notify(move |btn| {
            let hex = rgba_to_hex(&sidebar_color_for_color2);
            let hex2 = rgba_to_hex(btn);
            let opacity = sidebar_scale_for_color2.value() / 100.0;
            let mut new_cfg = config::settings().clone();
            new_cfg.appearance.sidebar_tint_color = hex.clone();
            new_cfg.appearance.sidebar_tint_color2 = hex2.clone();
            new_cfg.appearance.sidebar_tint_opacity = opacity;
            apply_tint_css(&new_cfg.appearance);
            let _ = config::save_settings(&new_cfg);
            sidebar_tint_row_for_color2.set_subtitle(&tint_subtitle(&hex, &hex2, opacity));
        });

        let sidebar_tint_row_for_scale = sidebar_tint_row.clone();
        let sidebar_color_btn_for_scale = sidebar_color_btn.clone();
        let sidebar_color2_btn_for_scale = sidebar_color2_btn.clone();
        let pending_sidebar: std::rc::Rc<std::cell::Cell<Option<glib::SourceId>>> =
            std::rc::Rc::new(std::cell::Cell::new(None));
        sidebar_opacity_scale.connect_value_changed(move |scale| {
            let hex = rgba_to_hex(&sidebar_color_btn_for_scale);
            let hex2 = rgba_to_hex(&sidebar_color2_btn_for_scale);
            let opacity = scale.value() / 100.0;
            sidebar_tint_row_for_scale.set_subtitle(&tint_subtitle(&hex, &hex2, opacity));
            if let Some(id) = pending_sidebar.take() { id.remove(); }
            let pending = pending_sidebar.clone();
            let id = glib::timeout_add_local_once(
                std::time::Duration::from_millis(120),
                move || {
                    pending.set(None);
                    let mut new_cfg = config::settings().clone();
                    new_cfg.appearance.sidebar_tint_color = hex.clone();
                    new_cfg.appearance.sidebar_tint_color2 = hex2.clone();
                    new_cfg.appearance.sidebar_tint_opacity = opacity;
                    apply_tint_css(&new_cfg.appearance);
                    let _ = config::save_settings(&new_cfg);
                },
            );
            pending_sidebar.set(Some(id));
        });

        let sidebar_tint_row_for_clear = sidebar_tint_row.clone();
        sidebar_clear_btn.connect_clicked(move |_| {
            let mut new_cfg = config::settings().clone();
            new_cfg.appearance.sidebar_tint_color = String::new();
            new_cfg.appearance.sidebar_tint_color2 = String::new();
            new_cfg.appearance.sidebar_tint_opacity = 0.05;
            apply_tint_css(&new_cfg.appearance);
            let _ = config::save_settings(&new_cfg);
            sidebar_tint_row_for_clear.set_subtitle("None");
        });

        // Bookmark highlight color row.
        let bm_row = adw::ActionRow::builder()
            .title("Bookmark Highlight Color")
            .subtitle("Left-border tint for bookmarked messages")
            .build();
        let bm_color_btn = gtk::ColorDialogButton::builder()
            .tooltip_text("Highlight color")
            .valign(gtk::Align::Center)
            .build();
        bm_color_btn.set_dialog(&gtk::ColorDialog::new());
        if let Ok(c) = gtk::gdk::RGBA::parse(&cfg.appearance.bookmark_highlight_color) {
            bm_color_btn.set_rgba(&c);
        }
        bm_row.add_suffix(&bm_color_btn);
        appearance_group.add(&bm_row);
        bm_color_btn.connect_rgba_notify(move |btn| {
            let c = btn.rgba();
            let hex = format!("#{:02x}{:02x}{:02x}",
                (c.red() * 255.0) as u8, (c.green() * 255.0) as u8, (c.blue() * 255.0) as u8);
            let mut new_cfg = config::settings().clone();
            new_cfg.appearance.bookmark_highlight_color = hex.clone();
            apply_bookmark_css(&hex);
            let _ = config::save_settings(&new_cfg);
        });

        // New message highlight color row.
        let nm_row = adw::ActionRow::builder()
            .title("New Message Highlight Color")
            .subtitle("Background tint for unread messages")
            .build();
        let nm_color_btn = gtk::ColorDialogButton::builder()
            .tooltip_text("New message color")
            .valign(gtk::Align::Center)
            .build();
        nm_color_btn.set_dialog(&gtk::ColorDialog::new());
        if let Ok(c) = gtk::gdk::RGBA::parse(&cfg.appearance.new_message_highlight_color) {
            nm_color_btn.set_rgba(&c);
        }
        nm_row.add_suffix(&nm_color_btn);
        appearance_group.add(&nm_row);
        nm_color_btn.connect_rgba_notify(move |btn| {
            let c = btn.rgba();
            let hex = format!("#{:02x}{:02x}{:02x}",
                (c.red() * 255.0) as u8, (c.green() * 255.0) as u8, (c.blue() * 255.0) as u8);
            let mut new_cfg = config::settings().clone();
            new_cfg.appearance.new_message_highlight_color = hex.clone();
            apply_new_message_css(&hex);
            let _ = config::save_settings(&new_cfg);
        });

        // --- AI group (lives on its own page, shown only when AI plugin is enabled) ---
        #[cfg(feature = "ai")]
        let (ai_page, _extra_row) = {
        let ai_group = adw::PreferencesGroup::builder()
            .title("AI (Ollama)")
            .description("Local LLM for room summaries. No data leaves the machine.")
            .build();

        // Status row — probed asynchronously when the dialog opens.
        let status_row = adw::ActionRow::builder()
            .title("Status")
            .subtitle("Checking…")
            .build();
        let spinner = gtk::Spinner::builder().spinning(true).build();
        status_row.add_suffix(&spinner);
        ai_group.add(&status_row);

        let endpoint_row = adw::EntryRow::builder()
            .title("Endpoint")
            .text(&cfg.ollama.endpoint)
            .build();

        // Active model — ComboRow populated from /api/tags on open.
        let model_list = gtk::StringList::new(&[]);
        let model_combo = adw::ComboRow::builder()
            .title("Active Model")
            .subtitle("Loading…")
            .model(&model_list)
            .build();

        // Pull row — downloads a new model by name.
        let pull_name_row = adw::EntryRow::builder()
            .title("Pull new model  (e.g. qwen2.5:3b)")
            .text(&cfg.ollama.model)
            .build();
        let pull_btn = gtk::Button::builder()
            .label("Pull")
            .valign(gtk::Align::Center)
            .css_classes(["pill"])
            .build();
        pull_name_row.add_suffix(&pull_btn);

        // Detect button — runs GPU detection and fills in the recommended model.
        let detect_btn = gtk::Button::builder()
            .icon_name("find-location-symbolic")
            .tooltip_text("Detect best model for this hardware")
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build();
        {
            let pull_name_row = pull_name_row.clone();
            detect_btn.connect_clicked(move |btn| {
                use crate::intelligence::gpu_detect;
                let gpu = gpu_detect::detect_gpu();
                let model = gpu_detect::suggested_model(gpu.as_ref());
                let reason = gpu_detect::suggestion_reason(gpu.as_ref());
                pull_name_row.set_text(model);
                btn.set_tooltip_text(Some(&format!("Detected: {reason}")));
            });
        }
        pull_name_row.add_suffix(&detect_btn);

        // Installed models group — one row per model with a Delete button.
        let installed_group = adw::PreferencesGroup::builder()
            .title("Installed Models")
            .build();

        ai_group.add(&endpoint_row);
        ai_group.add(&model_combo);
        ai_group.add(&pull_name_row);

        let extra_row = adw::EntryRow::builder()
            .title("Extra Instructions")
            .text(&cfg.ollama.room_preview_extra)
            .show_apply_button(true)
            .build();
        extra_row.set_tooltip_text(Some(
            "Appended to the summary prompt, e.g. \"Name the active participants. Describe the mood.\""
        ));
        ai_group.add(&extra_row);

        // Probe Ollama status asynchronously, then load installed models.
        {
            let status_row = status_row.clone();
            let spinner = spinner.clone();
            let endpoint = cfg.ollama.endpoint.clone();
            let model_combo = model_combo.clone();
            let model_list = model_list.clone();
            let installed_group = installed_group.clone();
            let saved_model = cfg.ollama.model.clone();
            let endpoint_for_delete = endpoint.clone();
            glib::spawn_future_local(async move {
                use crate::intelligence::ollama_manager::{detect, OllamaStatus};
                let st = detect(&endpoint).await;
                spinner.set_spinning(false);
                spinner.set_visible(false);
                let (subtitle, icon) = match &st {
                    OllamaStatus::Running { .. } => (st.label(), "emblem-ok-symbolic"),
                    OllamaStatus::Found { .. }   => (st.label(), "dialog-warning-symbolic"),
                    _                             => (st.label(), "dialog-error-symbolic"),
                };
                status_row.set_subtitle(&subtitle);
                let img = gtk::Image::from_icon_name(icon);
                img.set_tooltip_text(Some(&subtitle));
                status_row.add_suffix(&img);

                // Only fetch models when Ollama is actually running.
                if !matches!(st, OllamaStatus::Running { .. }) {
                    model_combo.set_subtitle("Ollama not running");
                    return;
                }

                let models = crate::intelligence::list_models(&endpoint).await;
                if models.is_empty() {
                    model_combo.set_subtitle("No models installed — pull one below");
                    return;
                }
                model_combo.set_subtitle("");

                // Populate combo and select the saved model (or first).
                let mut selected_idx = 0u32;
                for (i, name) in models.iter().enumerate() {
                    model_list.append(name);
                    if name == &saved_model { selected_idx = i as u32; }
                }
                model_combo.set_selected(selected_idx);

                // Build installed-models rows with Delete buttons.
                for name in &models {
                    let row = adw::ActionRow::builder()
                        .title(name)
                        .build();
                    let del_btn = gtk::Button::builder()
                        .icon_name("user-trash-symbolic")
                        .tooltip_text("Delete this model")
                        .valign(gtk::Align::Center)
                        .css_classes(["flat", "circular"])
                        .build();
                    let name_clone = name.clone();
                    let ep_clone = endpoint_for_delete.clone();
                    let row_weak = row.downgrade();
                    del_btn.connect_clicked(move |btn| {
                        let name = name_clone.clone();
                        let ep = ep_clone.clone();
                        let btn_weak = btn.downgrade();
                        let row_weak = row_weak.clone();
                        glib::spawn_future_local(async move {
                            if let Some(btn) = btn_weak.upgrade() {
                                btn.set_sensitive(false);
                            }
                            match crate::intelligence::delete_model(&ep, &name).await {
                                Ok(()) => {
                                    if let Some(row) = row_weak.upgrade() {
                                        if let Some(parent) = row.parent()
                                            .and_then(|p| p.parent())
                                            .and_then(|p| p.downcast::<adw::PreferencesGroup>().ok())
                                        {
                                            parent.remove(&row);
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("Delete model failed: {e}");
                                    if let Some(btn) = btn_weak.upgrade() {
                                        btn.set_sensitive(true);
                                    }
                                }
                            }
                        });
                    });
                    row.add_suffix(&del_btn);
                    installed_group.add(&row);
                }
            });
        }

        // Re-probe when endpoint changes.
        {
            let status_row = status_row.clone();
            endpoint_row.connect_changed(move |row| {
                let mut new_cfg = config::settings().clone();
                new_cfg.ollama.endpoint = row.text().to_string();
                let _ = config::save_settings(&new_cfg);
                status_row.set_subtitle("Changed — reopen Preferences to check");
            });
        }

        // Save active model when selection changes.
        model_combo.connect_notify_local(Some("selected"), move |combo, _| {
            if let Some(item) = combo.selected_item()
                .and_then(|o| o.downcast::<gtk::StringObject>().ok())
            {
                let mut new_cfg = config::settings().clone();
                new_cfg.ollama.model = item.string().to_string();
                let _ = config::save_settings(&new_cfg);
            }
        });

        // Pull model button — uses Ollama's own /api/pull with live progress.
        {
            let model_row_for_pull = pull_name_row.clone();
            let pull_btn_clone = pull_btn.clone();
            pull_btn.connect_clicked(move |_| {
                let model = model_row_for_pull.text().to_string();
                let endpoint = config::settings().ollama.endpoint.clone();
                if model.is_empty() || endpoint.is_empty() { return; }
                pull_btn_clone.set_label("0%");
                pull_btn_clone.set_sensitive(false);
                let btn = pull_btn_clone.clone();
                let provider = gtk::CssProvider::new();
                #[allow(deprecated)]
                btn.style_context().add_provider(
                    &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION
                );
                glib::spawn_future_local(async move {
                    let btn_prog = btn.clone();
                    let prov_prog = provider.clone();
                    let result = crate::intelligence::pull_model(
                        &endpoint, &model,
                        move |p| {
                            let pct = (p * 100.0) as u32;
                            btn_prog.set_label(&format!("{pct}%"));
                            #[allow(deprecated)]
                            prov_prog.load_from_data(&format!(
                                "button {{ background: linear-gradient(\
                                    to right,\
                                    alpha(@accent_bg_color,0.4) {pct}%,\
                                    alpha(@accent_bg_color,0.1) {pct}%\
                                ); }}"
                            ));
                        }
                    ).await;
                    #[allow(deprecated)]
                    provider.load_from_data("button {}");
                    match result {
                        Ok(()) => btn.set_label("Done ✓"),
                        Err(e) => {
                            tracing::warn!("Model pull failed: {e}");
                            btn.set_label("Failed");
                        }
                    }
                    btn.set_sensitive(true);
                });
            });
        }

        extra_row.connect_apply(|row| {
            let mut new_cfg = config::settings().clone();
            new_cfg.ollama.room_preview_extra = row.text().to_string();
            let _ = config::save_settings(&new_cfg);
        });

        // Build the AI page — added to the dialog only when the plugin is enabled.
        let ai_page = adw::PreferencesPage::builder()
            .icon_name("view-reveal-symbolic")
            .title("AI")
            .build();
        ai_page.add(&ai_group);
        ai_page.add(&installed_group);

        (ai_page, extra_row)
        }; // end #[cfg(feature = "ai")]

        // --- Info group ---
        let info_group = adw::PreferencesGroup::builder()
            .title("Storage")
            .build();

        let config_path_row = adw::ActionRow::builder()
            .title("Config File")
            .subtitle("GSettings (dconf) · me.ramkrishna.hikyaku")
            .build();
        info_group.add(&config_path_row);

        // --- Account group ---
        let account_group = adw::PreferencesGroup::builder()
            .title("Account")
            .build();

        // Avatar row — preview on the left, Change + Remove buttons on
        // the right. Preview uses adw::Avatar which handles both the
        // cached-image path (if we've downloaded the user's own avatar)
        // and the initials-on-colour fallback when no image is known yet.
        let my_user_id = self.imp().user_id.borrow().clone();
        // Use the localpart (@alice:server → "alice") for initials when
        // we don't have a display name at hand.
        let initials_source = my_user_id
            .trim_start_matches('@')
            .split(':')
            .next()
            .unwrap_or(&my_user_id)
            .to_string();
        let avatar_preview = adw::Avatar::builder()
            .size(48)
            .text(&initials_source)
            .show_initials(true)
            .build();
        if !my_user_id.is_empty() {
            if let Some(path) = self.imp().avatar_cache.borrow().get(&my_user_id) {
                if !path.is_empty() {
                    if let Ok(tex) = gtk::gdk::Texture::from_filename(path) {
                        avatar_preview.set_custom_image(Some(&tex));
                    }
                }
            }
        }

        let avatar_row = adw::ActionRow::builder()
            .title("Profile Picture")
            .subtitle("PNG / JPEG / GIF / WebP, up to 8 MB")
            .build();
        avatar_row.add_prefix(&avatar_preview);

        let change_avatar_btn = gtk::Button::builder()
            .label("Change")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        let remove_avatar_btn = gtk::Button::builder()
            .label("Remove")
            .valign(gtk::Align::Center)
            .css_classes(["flat", "destructive-action"])
            .build();
        avatar_row.add_suffix(&change_avatar_btn);
        avatar_row.add_suffix(&remove_avatar_btn);
        account_group.add(&avatar_row);

        // Change-avatar click → file chooser → SetOwnAvatar command.
        let tx_change = self.imp().command_tx.get().unwrap().clone();
        let toast_change = self.imp().toast_overlay.clone();
        let window_ref_avatar = self.downgrade();
        change_avatar_btn.connect_clicked(move |_| {
            let Some(win) = window_ref_avatar.upgrade() else { return };
            let tx = tx_change.clone();
            let overlay = toast_change.clone();
            let image_filter = gtk::FileFilter::new();
            image_filter.set_name(Some("Images"));
            image_filter.add_mime_type("image/png");
            image_filter.add_mime_type("image/jpeg");
            image_filter.add_mime_type("image/gif");
            image_filter.add_mime_type("image/webp");
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&image_filter);
            let file_dialog = gtk::FileDialog::builder()
                .title("Choose a profile picture")
                .filters(&filters)
                .build();
            file_dialog.open(Some(&win), gio::Cancellable::NONE, move |result| {
                let Ok(file) = result else { return };
                let Some(path) = file.path() else { return };
                let Some(path_str) = path.to_str().map(|s| s.to_string()) else { return };
                let tx_inner = tx.clone();
                toast(&overlay, "Uploading avatar…");
                glib::spawn_future_local(async move {
                    let _ = tx_inner.send(MatrixCommand::SetOwnAvatar {
                        file_path: Some(path_str),
                    }).await;
                });
            });
        });

        // Remove-avatar click → SetOwnAvatar { None }.
        let tx_remove = self.imp().command_tx.get().unwrap().clone();
        let toast_remove = self.imp().toast_overlay.clone();
        remove_avatar_btn.connect_clicked(move |_| {
            let tx = tx_remove.clone();
            toast(&toast_remove, "Removing avatar…");
            glib::spawn_future_local(async move {
                let _ = tx.send(MatrixCommand::SetOwnAvatar { file_path: None }).await;
            });
        });

        let logout_row = adw::ActionRow::builder()
            .title("Log Out")
            .subtitle("Sign out and return to the login screen")
            .activatable(true)
            .build();
        logout_row.add_suffix(&gtk::Image::from_icon_name("system-log-out-symbolic"));
        let tx = self.imp().command_tx.get().unwrap().clone();
        let dialog_weak = dialog.downgrade();
        logout_row.connect_activated(move |_| {
            let tx = tx.clone();
            let dialog_weak = dialog_weak.clone();
            glib::spawn_future_local(async move {
                let _ = tx.send(MatrixCommand::Logout).await;
                if let Some(dialog) = dialog_weak.upgrade() {
                    dialog.close();
                }
            });
        });
        account_group.add(&logout_row);

        let page = adw::PreferencesPage::builder()
            .icon_name("preferences-system-symbolic")
            .title("General")
            .build();
        page.add(&appearance_group);
        page.add(&info_group);
        page.add(&account_group);
        dialog.add(&page);

        // --- Rolodex page ---
        #[cfg(feature = "rolodex")]
        {
        let rolodex_page = adw::PreferencesPage::builder()
            .icon_name("contact-new-symbolic")
            .title("Rolodex")
            .build();

        let rolodex_group = adw::PreferencesGroup::builder()
            .title("Contacts")
            .description("Right-click any sender name in chat to add them. Also appear first in @ completion.")
            .build();

        // Bind a ListBox to the GObject store so it stays live.
        let contact_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();

        let store = self.imp().rolodex_store.clone();
        let store_for_factory = store.clone();
        contact_list.bind_model(Some(&store), move |obj| {
            let store = store_for_factory.clone();
            let entry = obj.downcast_ref::<crate::models::RolodexEntryObject>().unwrap();
            let row = adw::ActionRow::builder()
                .title(entry.display_name())
                .subtitle(entry.user_id())
                .build();
            let remove_btn = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .valign(gtk::Align::Center)
                .css_classes(["flat", "circular"])
                .tooltip_text("Remove contact")
                .build();
            // Bind display-name notify so the row title stays live if edited.
            entry.bind_property("display-name", &row, "title")
                .sync_create()
                .build();
            let uid = entry.user_id();
            remove_btn.connect_clicked(move |_| {
                if let Some(pos) = (0..store.n_items()).find(|&i| {
                    store.item(i)
                        .and_downcast::<crate::models::RolodexEntryObject>()
                        .map(|o| o.user_id() == uid)
                        .unwrap_or(false)
                }) {
                    store.remove(pos);
                }
            });
            row.add_suffix(&remove_btn);
            row.upcast::<gtk::Widget>()
        });

        rolodex_group.add(&contact_list);

        // Manual add group.
        let add_group = adw::PreferencesGroup::builder()
            .title("Add Contact Manually")
            .build();

        let name_entry = adw::EntryRow::builder()
            .title("Display Name")
            .build();
        let uid_entry = adw::EntryRow::builder()
            .title("User ID  (e.g. @alice:example.com)")
            .build();
        let add_btn = gtk::Button::builder()
            .label("Add")
            .halign(gtk::Align::End)
            .margin_top(4)
            .css_classes(["suggested-action", "pill"])
            .build();

        let store = self.imp().rolodex_store.clone();
        {
            let name_entry = name_entry.clone();
            let uid_entry = uid_entry.clone();
            add_btn.connect_clicked(move |_| {
                let name = name_entry.text().trim().to_string();
                let uid = uid_entry.text().trim().to_string();
                if name.is_empty() || uid.is_empty() { return; }
                // Deduplicate.
                let exists = (0..store.n_items()).any(|i| {
                    store.item(i)
                        .and_downcast::<crate::models::RolodexEntryObject>()
                        .map(|o| o.user_id() == uid)
                        .unwrap_or(false)
                });
                if exists { return; }
                let added_at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                store.append(&crate::models::RolodexEntryObject::new(
                    &uid, &name, "", added_at,
                ));
                // items-changed → save JSON + GSettings automatically.
                name_entry.set_text("");
                uid_entry.set_text("");
            });
        }

        add_group.add(&name_entry);
        add_group.add(&uid_entry);
        add_group.add(&add_btn);

        rolodex_page.add(&rolodex_group);
        rolodex_page.add(&add_group);
        dialog.add(&rolodex_page);
        } // #[cfg(feature = "rolodex")]

        // --- Plugins page ---
        let plugins_page = adw::PreferencesPage::builder()
            .icon_name("application-x-addon-symbolic")
            .title("Plugins")
            .build();

        let plugins_group = adw::PreferencesGroup::builder()
            .title("Installed Plugins")
            .description("Enable or disable optional features. Changes take effect immediately.")
            .build();

        // AI plugin toggle — enabling shows the AI settings tab; disabling removes it.
        #[cfg(feature = "ai")]
        {
            let ai_switch = adw::SwitchRow::builder()
                .title("AI Summaries")
                .subtitle("Ctrl+click a room for an Ollama-powered summary (no data leaves your machine)")
                .active(cfg.ollama.enabled)
                .build();
            plugins_group.add(&ai_switch);

            // Add the AI page now if already enabled.
            if cfg.ollama.enabled {
                dialog.add(&ai_page);
            }

            let window_weak_ai = self.downgrade();
            let dialog_weak = dialog.downgrade();
            let ai_page_ref = ai_page.clone();
            ai_switch.connect_active_notify(move |sw| {
                let gs = crate::config::gsettings();
                if sw.is_active() {
                    // Inform the user exactly what will be downloaded before
                    // committing. Cancelling reverts the switch — nothing is saved.
                    let already_setup = crate::config::settings().ollama.setup_done;
                    let body = if already_setup {
                        "AI summaries are powered by a local Ollama model.\n\n\
                         ⚠ AI responses can be inaccurate — always verify \
                         important information yourself."
                            .to_string()
                    } else {
                        "Enabling AI will download:\n\
                         • Ollama runtime  (~50 MB)\n\
                         • A language model of your choice  (1–3 GB)\n\n\
                         Your machine must be online for the download. \
                         You will choose the model in the next step.\n\n\
                         ⚠ AI responses can be inaccurate — always verify \
                         important information yourself."
                            .to_string()
                    };
                    let alert = adw::AlertDialog::builder()
                        .heading("Enable AI Summaries?")
                        .body(&body)
                        .close_response("cancel")
                        .default_response("enable")
                        .build();
                    alert.add_response("cancel", "Cancel");
                    alert.add_response("enable", if already_setup { "Enable" } else { "Download & Enable" });
                    alert.set_response_appearance("enable", adw::ResponseAppearance::Suggested);
                    let sw_weak = sw.downgrade();
                    let dialog_weak2 = dialog_weak.clone();
                    let ai_page_ref2 = ai_page_ref.clone();
                    let window_weak2 = window_weak_ai.clone();
                    alert.connect_response(None, move |_, response| {
                        if response == "enable" {
                            let _ = gs.set_boolean("ollama-enabled", true);
                            if let Some(d) = dialog_weak2.upgrade() {
                                d.add(&ai_page_ref2);
                            }
                            // First enable ever → launch the setup wizard so the
                            // user is walked through the Ollama + model download.
                            if !crate::config::settings().ollama.setup_done {
                                if let Some(win) = window_weak2.upgrade() {
                                    glib::idle_add_local_once(move || {
                                        show_ai_setup_dialog(&win);
                                    });
                                }
                            }
                        } else {
                            // User cancelled — revert the switch without saving.
                            if let Some(sw) = sw_weak.upgrade() {
                                sw.set_active(false);
                            }
                        }
                    });
                    if let Some(win) = window_weak_ai.upgrade() {
                        alert.present(Some(&win));
                    }
                } else {
                    let _ = gs.set_boolean("ollama-enabled", false);
                    if let Some(d) = dialog_weak.upgrade() {
                        d.remove(&ai_page_ref);
                    }
                }
            });
        }

        // Rolodex plugin.
        #[cfg(feature = "rolodex")]
        {
            let rolodex_switch = adw::SwitchRow::builder()
                .title("Rolodex")
                .subtitle("Personal contact book with notes and @-completion")
                .active(cfg.plugins.rolodex)
                .build();
            plugins_group.add(&rolodex_switch);
            rolodex_switch.connect_active_notify(|sw| {
                let gs = crate::config::gsettings();
                let _ = gs.set_boolean("plugin-rolodex-enabled", sw.is_active());
            });
        }

        // Pinning plugin.
        #[cfg(feature = "pinning")]
        {
            let pinning_switch = adw::SwitchRow::builder()
                .title("Message Pinning")
                .subtitle("Bookmark messages locally to come back to later")
                .active(cfg.plugins.pinning)
                .build();
            plugins_group.add(&pinning_switch);
            pinning_switch.connect_active_notify(|sw| {
                let gs = crate::config::gsettings();
                let _ = gs.set_boolean("plugin-pinning-enabled", sw.is_active());
            });
        }

        // MOTD plugin.
        #[cfg(feature = "motd")]
        {
            let rl_view = self.imp().room_list_view.clone();
            let motd_switch = adw::SwitchRow::builder()
                .title("Topic Change Tracker")
                .subtitle("Toast when the room topic changes; icon on rooms you haven't visited yet")
                .active(cfg.plugins.motd)
                .build();
            plugins_group.add(&motd_switch);
            motd_switch.connect_active_notify(move |sw| {
                let gs = crate::config::gsettings();
                let _ = gs.set_boolean("plugin-motd-enabled", sw.is_active());
                if !sw.is_active() {
                    rl_view.clear_all_topic_changed();
                }
            });
        }

        // Community health monitor plugin.
        #[cfg(feature = "community-health")]
        {
            let rl_view = self.imp().room_list_view.clone();
            let health_switch = adw::SwitchRow::builder()
                .title("Community Health Monitor")
                .subtitle("Show an emotional tone indicator on each room (uses local NPU/GPU via OpenVINO)")
                .active(cfg.plugins.community_health)
                .build();
            plugins_group.add(&health_switch);
            health_switch.connect_active_notify(move |sw| {
                let gs = crate::config::gsettings();
                let _ = gs.set_boolean("plugin-community-health-enabled", sw.is_active());
                if !sw.is_active() {
                    // Clear all health dots when the plugin is disabled.
                    rl_view.clear_all_health_dots();
                }
            });
        }

        plugins_page.add(&plugins_group);
        dialog.add(&plugins_page);

        // --- Watch page ---
        #[cfg(feature = "ai")]
        {
        let watch_page = adw::PreferencesPage::builder()
            .icon_name("view-reveal-symbolic")
            .title("Watch")
            .build();

        let watch_group = adw::PreferencesGroup::builder()
            .title("Room Interest Watcher")
            .description("Get notified when messages semantically match your keywords (CPU ONNX)")
            .build();

        let watch_enabled_row = adw::SwitchRow::builder()
            .title("Enable Watcher")
            .subtitle("Alert when a watched term matches a recent message")
            .active(cfg.watch.enabled)
            .build();
        watch_group.add(&watch_enabled_row);

        let watch_terms_row = adw::EntryRow::builder()
            .title("Terms (comma-separated)")
            .text(&cfg.watch.terms.join(", "))
            .show_apply_button(true)
            .build();
        watch_terms_row.set_tooltip_text(Some(
            "E.g. \"urgent deploy, security incident, budget approval\""
        ));
        watch_group.add(&watch_terms_row);

        let watch_threshold_row = adw::SpinRow::builder()
            .title("Similarity Threshold")
            .subtitle("Cosine similarity required for a match (0.0–1.0, default 0.65)")
            .adjustment(&gtk::Adjustment::new(
                cfg.watch.threshold, 0.1, 1.0, 0.05, 0.1, 0.0,
            ))
            .digits(2)
            .build();
        watch_group.add(&watch_threshold_row);

        watch_enabled_row.connect_active_notify(|row| {
            let gs = crate::config::gsettings();
            let _ = gs.set_boolean("watch-enabled", row.is_active());
        });
        watch_terms_row.connect_apply(|row| {
            let gs = crate::config::gsettings();
            let raw = row.text().to_string();
            let terms: Vec<String> = raw
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let refs: Vec<&str> = terms.iter().map(|s| s.as_str()).collect();
            let _ = gs.set_strv("watch-terms", refs.as_slice());
        });
        watch_threshold_row.connect_value_notify(|row| {
            let gs = crate::config::gsettings();
            let _ = gs.set_double("watch-threshold", row.value());
        });

        watch_page.add(&watch_group);
        dialog.add(&watch_page);
        } // #[cfg(feature = "ai")]

        dialog.present(Some(self));
    }
}

/// Show the GNOME-standard keyboard shortcuts window (Ctrl+?).
/// Built programmatically using gtk::ShortcutsWindow / Section / Group / Shortcut.
/// First-run AI setup dialog shown once after login.
/// Lets the user pick and pull a recommended Ollama model.
/// Marks setup_done=true on skip or after a successful pull.
fn show_ai_setup_dialog(window: &MxWindow) {
    use crate::intelligence::ollama_manager;

    let dialog = adw::Dialog::builder()
        .title("Set Up AI Summaries")
        .content_width(420)
        .build();

    let toolbar = adw::ToolbarView::new();
    dialog.set_child(Some(&toolbar));

    let header = adw::HeaderBar::new();
    toolbar.add_top_bar(&header);

    let vbox = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .margin_start(24)
        .margin_end(24)
        .margin_top(12)
        .margin_bottom(24)
        .build();
    toolbar.set_content(Some(&vbox));

    // Description.
    let desc = gtk::Label::builder()
        .label("Hikyaku can use a local AI model to summarize room activity. \
                No data leaves your machine. Choose a model to download via Ollama, \
                or skip to configure later in Preferences.")
        .wrap(true)
        .halign(gtk::Align::Start)
        .css_classes(["body"])
        .build();
    vbox.append(&desc);

    // Ollama status row.
    let status_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    let status_icon = gtk::Image::from_icon_name("emblem-synchronizing-symbolic");
    let status_label = gtk::Label::builder()
        .label("Checking for Ollama…")
        .halign(gtk::Align::Start)
        .hexpand(true)
        .css_classes(["caption", "dim-label"])
        .build();
    status_box.append(&status_icon);
    status_box.append(&status_label);
    vbox.append(&status_box);

    // Model choices — displayed as a list of radio-style ActionRows.
    // (name, tag, description)
    let models: &[(&str, &str, &str)] = &[
        ("Qwen 2.5 7B",      "qwen2.5:7b",       "Best accuracy · ~4.5 GB · recommended for 16 GB+ RAM"),
        ("Qwen 2.5 3B",      "qwen2.5:3b",       "Good balance · ~2 GB · recommended for 8 GB RAM"),
        ("Qwen 2.5 1.5B",    "qwen2.5:1.5b",     "Lightweight · ~1 GB · faster on low-end hardware"),
        ("Llama 3.2 3B",     "llama3.2:3b",       "Meta 3B · ~2 GB · general-purpose chat"),
        ("Phi 4 Mini 3.8B",  "phi4-mini:3.8b",    "Microsoft · ~2.5 GB · compact and capable"),
    ];

    let model_group = adw::PreferencesGroup::builder()
        .title("Choose a model")
        .description("All models run locally — nothing leaves your machine")
        .build();
    vbox.append(&model_group);

    // Track selected model tag.
    let selected_model = std::rc::Rc::new(std::cell::RefCell::new("qwen2.5:3b".to_string()));

    // Build (ActionRow, Image, tag) triples so we can wire everything in one
    // place without re-querying the PreferencesGroup widget tree (unreliable).
    let model_triples: Vec<(adw::ActionRow, gtk::Image, String)> = models.iter().enumerate()
        .map(|(i, (name, tag, desc))| {
            let row = adw::ActionRow::builder()
                .title(*name)
                .subtitle(*desc)
                .activatable(true)
                .build();
            let check = gtk::Image::new();
            // First entry is pre-selected; rest show no icon (None ≠ "").
            if i == 0 {
                check.set_icon_name(Some("emblem-ok-symbolic"));
            }
            row.add_suffix(&check);
            model_group.add(&row);
            (row, check, tag.to_string())
        })
        .collect();

    // Wire each row: on activate, update selected_model and repaint all
    // checkmarks in a single pass over the triples vec.
    let tags: Vec<String> = model_triples.iter().map(|(_, _, t)| t.clone()).collect();
    let checks: Vec<gtk::Image> = model_triples.iter().map(|(_, c, _)| c.clone()).collect();
    for (row, _check, tag) in &model_triples {
        let sel = selected_model.clone();
        let tag_str = tag.clone();
        let checks_inner = checks.clone();
        let tags_inner = tags.clone();
        row.connect_activated(move |_| {
            *sel.borrow_mut() = tag_str.clone();
            for (c, t) in checks_inner.iter().zip(tags_inner.iter()) {
                c.set_icon_name(if *t == tag_str { Some("emblem-ok-symbolic") } else { None });
            }
        });
    }

    // Custom model entry — lets knowledgeable users type any Ollama model tag.
    let custom_group = adw::PreferencesGroup::builder()
        .title("Or enter any Ollama model tag")
        .build();
    vbox.append(&custom_group);

    let custom_entry = adw::EntryRow::builder()
        .title("Model tag  (e.g. mistral:7b, phi4:latest)")
        .build();
    custom_group.add(&custom_entry);

    // Typing in the custom entry deselects the radio list and uses the typed tag.
    {
        let sel = selected_model.clone();
        let checks_ref = checks.clone();
        custom_entry.connect_changed(move |entry| {
            let text = entry.text().to_string();
            if !text.is_empty() {
                *sel.borrow_mut() = text;
                // Clear all radio checkmarks — custom overrides.
                for c in &checks_ref {
                    c.set_icon_name(None::<&str>);
                }
            }
        });
    }

    // Progress label + bar (hidden until pull starts).
    let progress_bar = gtk::ProgressBar::builder()
        .visible(false)
        .show_text(true)
        .build();
    let progress_label = gtk::Label::builder()
        .label("")
        .visible(false)
        .css_classes(["caption"])
        .halign(gtk::Align::Start)
        .build();
    vbox.append(&progress_label);
    vbox.append(&progress_bar);

    // Button row.
    let btn_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    vbox.append(&btn_row);

    let skip_btn = gtk::Button::builder()
        .label("Skip")
        .css_classes(["flat"])
        .build();
    let pull_btn = gtk::Button::builder()
        .label("Download & Use")
        .css_classes(["suggested-action", "pill"])
        .build();
    btn_row.append(&skip_btn);
    btn_row.append(&pull_btn);

    // Skip → close without marking setup done. The wizard will reappear
    // on next login so the user can complete setup when they're ready.
    {
        let dialog = dialog.clone();
        skip_btn.connect_clicked(move |_| {
            dialog.close();
        });
    }

    // Pull → download model via Ollama API, then mark done.
    {
        let dialog = dialog.clone();
        let selected = selected_model.clone();
        let progress_bar = progress_bar.clone();
        let progress_label = progress_label.clone();
        let pull_btn_clone = pull_btn.clone();
        let skip_btn = skip_btn.clone();
        pull_btn.connect_clicked(move |_| {
            let model = selected.borrow().clone();
            let endpoint = crate::config::settings().ollama.endpoint.clone();

            pull_btn_clone.set_sensitive(false);
            skip_btn.set_sensitive(false);
            pull_btn_clone.set_label("Downloading…");
            progress_bar.set_visible(true);
            progress_label.set_visible(true);
            progress_label.set_label(&format!("Pulling {model} via Ollama…"));
            progress_bar.pulse();

            let progress_bar_inner = progress_bar.clone();
            let progress_label_inner = progress_label.clone();
            let pull_btn_inner = pull_btn_clone.clone();
            let dialog_inner = dialog.clone();
            let model_clone = model.clone();

            glib::spawn_future_local(async move {
                match ollama_manager::ensure_running(&endpoint).await {
                    None => {
                        progress_label_inner.set_label(
                            "Ollama is not running. Install Ollama first."
                        );
                        pull_btn_inner.set_label("Download & Use");
                        pull_btn_inner.set_sensitive(true);
                        progress_bar_inner.set_visible(false);
                    }
                    Some(ep) => {
                        let bar = progress_bar_inner.clone();
                        let lbl = progress_label_inner.clone();
                        let btn_p = pull_btn_inner.clone();
                        let model_p = model_clone.clone();
                        match crate::intelligence::pull_model(
                            &ep, &model_clone,
                            move |p| {
                                bar.set_fraction(p);
                                let pct = (p * 100.0) as u32;
                                lbl.set_label(&format!("Pulling {model_p}… {pct}%"));
                                btn_p.set_label(&format!("{pct}%"));
                            }
                        ).await {
                            Ok(()) => {
                                let mut cfg = crate::config::settings().clone();
                                cfg.ollama.model = model.clone();
                                cfg.ollama.setup_done = true;
                                let _ = crate::config::save_settings(&cfg);
                                progress_label_inner.set_label(&format!("✓ {model} is ready"));
                                progress_bar_inner.set_fraction(1.0);
                                pull_btn_inner.set_label("Done");
                                glib::timeout_add_local_once(
                                    std::time::Duration::from_secs(1),
                                    move || { dialog_inner.close(); },
                                );
                            }
                            Err(e) => {
                                progress_label_inner.set_label(&format!("Pull failed: {e}"));
                                pull_btn_inner.set_label("Download & Use");
                                pull_btn_inner.set_sensitive(true);
                            }
                        }
                    }
                }
            });
        });
    }

    // Probe Ollama in background and update the status row.
    {
        use ollama_manager::OllamaStatus;
        let endpoint = crate::config::settings().ollama.endpoint.clone();
        glib::spawn_future_local(async move {
            match ollama_manager::detect(&endpoint).await {
                OllamaStatus::Running { .. } => {
                    status_icon.set_icon_name(Some("emblem-ok-symbolic"));
                    status_label.set_label("Ollama is running");
                }
                OllamaStatus::Found { ref path } => {
                    status_icon.set_icon_name(Some("dialog-warning-symbolic"));
                    status_label.set_label(&format!(
                        "Ollama found at {} (will start on demand)", path.display()
                    ));
                }
                OllamaStatus::NeedDownload => {
                    status_icon.set_icon_name(Some("folder-download-symbolic"));
                    status_label.set_label("Ollama not found — click to download (~50 MB)");
                    pull_btn.set_label("Download Ollama first");
                    // Wire pull_btn to download Ollama, then re-enable model pull.
                    let status_icon2 = status_icon.clone();
                    let status_label2 = status_label.clone();
                    let pull_btn2 = pull_btn.clone();
                    let progress_bar2 = progress_bar.clone();
                    let progress_label2 = progress_label.clone();
                    pull_btn.connect_clicked(move |btn| {
                        btn.set_sensitive(false);
                        btn.set_label("Downloading Ollama…");
                        progress_bar2.set_visible(true);
                        progress_label2.set_visible(true);
                        progress_label2.set_label("Downloading Ollama…");
                        let pb = progress_bar2.clone();
                        let pl = progress_label2.clone();
                        let btn2 = pull_btn2.clone();
                        let si = status_icon2.clone();
                        let sl = status_label2.clone();
                        glib::spawn_future_local(async move {
                            use crate::intelligence::ollama_manager::download_ollama_binary;
                            let pb_cb = pb.clone();
                            let pl_cb = pl.clone();
                            let result = download_ollama_binary(move |p| {
                                if p >= 0.0 {
                                    pb_cb.set_fraction(p);
                                    pl_cb.set_label(&format!(
                                        "Downloading Ollama… {:.0}%", p * 100.0
                                    ));
                                } else {
                                    pb_cb.pulse();
                                }
                            }).await;
                            match result {
                                Ok(_) => {
                                    si.set_icon_name(Some("emblem-ok-symbolic"));
                                    sl.set_label("Ollama downloaded — ready to use");
                                    pl.set_label("Ollama downloaded. Now pull a model.");
                                    pb.set_fraction(1.0);
                                    // Re-wire pull_btn for model pull.
                                    btn2.set_label("Download & Use");
                                    btn2.set_sensitive(true);
                                }
                                Err(e) => {
                                    si.set_icon_name(Some("dialog-error-symbolic"));
                                    sl.set_label(&format!("Download failed: {e}"));
                                    pl.set_label(&format!("Error: {e}"));
                                    btn2.set_label("Retry");
                                    btn2.set_sensitive(true);
                                }
                            }
                        });
                    });
                }
                OllamaStatus::NotAvailable => {
                    status_icon.set_icon_name(Some("dialog-error-symbolic"));
                    status_label.set_label("Ollama not found — install from ollama.com");
                    pull_btn.set_sensitive(false);
                }
            }
        });
    }

    dialog.present(Some(window));
}

fn show_shortcuts_window(window: &MxWindow) {
    let shortcuts_window = gtk::ShortcutsWindow::builder()
        .transient_for(window)
        .modal(true)
        .build();

    // The ShortcutsWindow requires at least one ShortcutsSection.
    let section = gtk::ShortcutsSection::builder()
        .title("Hikyaku")
        .section_name("main")
        .build();
    shortcuts_window.add_section(&section);

    // --- Navigation group ---
    let nav_group = gtk::ShortcutsGroup::builder()
        .title("Navigation")
        .build();
    section.add_group(&nav_group);

    for (accel, title) in [
        ("<Alt>Up",   "Previous room"),
        ("<Alt>Down", "Next room"),
    ] {
        nav_group.add_shortcut(&gtk::ShortcutsShortcut::builder()
            .accelerator(accel)
            .title(title)
            .build());
    }

    // --- Messaging group ---
    let msg_group = gtk::ShortcutsGroup::builder()
        .title("Messaging")
        .build();
    section.add_group(&msg_group);

    for (accel, title) in [
        ("Return",        "Send message"),
        ("Tab",           "Complete @mention"),
    ] {
        msg_group.add_shortcut(&gtk::ShortcutsShortcut::builder()
            .accelerator(accel)
            .title(title)
            .build());
    }

    // --- Application group ---
    let app_group = gtk::ShortcutsGroup::builder()
        .title("Application")
        .build();
    section.add_group(&app_group);

    for (accel, title) in [
        ("<Control>comma",    "Preferences"),
        ("<Control><Shift>j", "Join a room"),
        ("<Control>question", "Keyboard shortcuts"),
    ] {
        app_group.add_shortcut(&gtk::ShortcutsShortcut::builder()
            .accelerator(accel)
            .title(title)
            .build());
    }

    shortcuts_window.present();
}

/// Show a file picker + passphrase dialog to import E2E session keys
/// exported from Element (or any Matrix client using the standard format).
async fn show_import_keys_dialog(window: &MxWindow, command_tx: async_channel::Sender<MatrixCommand>) {
    // File picker.
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Key files (*.txt)"));
    filter.add_pattern("*.txt");
    filter.add_pattern("*.key");

    let file_dialog = gtk::FileDialog::builder()
        .title("Select exported key file")
        .default_filter(&filter)
        .build();

    let file = match file_dialog.open_future(Some(window)).await {
        Ok(f) => f,
        Err(_) => return, // cancelled
    };

    let path = match file.path() {
        Some(p) => p,
        None => return,
    };

    // Passphrase dialog.
    let entry = gtk::PasswordEntry::builder()
        .placeholder_text("Passphrase used when exporting")
        .show_peek_icon(true)
        .build();

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(8)
        .build();
    content.append(&gtk::Label::builder()
        .label("Enter the passphrase you used when exporting keys from Element.")
        .wrap(true)
        .xalign(0.0)
        .build());
    content.append(&entry);

    let passphrase_dialog = adw::AlertDialog::builder()
        .heading("Import Encryption Keys")
        .extra_child(&content)
        .build();
    passphrase_dialog.add_response("cancel", "Cancel");
    passphrase_dialog.add_response("import", "Import");
    passphrase_dialog.set_response_appearance("import", adw::ResponseAppearance::Suggested);
    passphrase_dialog.set_default_response(Some("import"));

    let response = passphrase_dialog.choose_future(window).await;
    if response != "import" {
        return;
    }

    let passphrase = entry.text().to_string();
    if passphrase.is_empty() {
        return;
    }

    let _ = command_tx.send(MatrixCommand::ImportRoomKeys { path, passphrase }).await;
}

fn show_export_metrics_dialog(
    parent: &MxWindow,
    room_id: String,
    command_tx: async_channel::Sender<MatrixCommand>,
) {
    let spin = gtk::SpinButton::with_range(1.0, 365.0, 1.0);
    spin.set_value(30.0);
    spin.set_digits(0);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(8)
        .build();
    content.append(&gtk::Label::builder()
        .label("Export events from the last N days:")
        .xalign(0.0)
        .build());
    content.append(&spin);

    let dialog = adw::AlertDialog::builder()
        .heading("Export Room Metrics")
        .extra_child(&content)
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("export", "Export");
    dialog.set_response_appearance("export", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("export"));

    dialog.connect_response(None, move |_dialog, response| {
        if response == "export" {
            let days = spin.value() as u32;
            let tx = command_tx.clone();
            let rid = room_id.clone();
            glib::spawn_future_local(async move {
                let _ = tx.send(MatrixCommand::ExportRoomMetrics {
                    room_id: rid,
                    days,
                }).await;
            });
        }
    });

    dialog.present(Some(parent));
}

/// Build the "Invite User" dialog with live name-based search.
///
/// The user types a display name (or partial Matrix ID); local room members are
/// searched instantly and the Matrix user directory is queried asynchronously.
/// Selecting a result fills in the Matrix ID, so the user never has to type it.
fn build_invite_dialog(
    fallback_parent: &gtk::Button,
    win_weak: &glib::WeakRef<MxWindow>,
    tx: &async_channel::Sender<MatrixCommand>,
    room_id: &str,
    local_members: &[(String, String, String)], // (lowercase_name, display_name, user_id)
) {
    // The selected Matrix user ID — updated when a result row is chosen.
    let selected_uid: std::rc::Rc<std::cell::RefCell<String>> = std::rc::Rc::new(std::cell::RefCell::new(String::new()));

    // ── Search entry ──────────────────────────────────────────────────────────
    let search_entry = adw::EntryRow::builder()
        .title("Name or @user:server")
        .build();

    // ── Results list ─────────────────────────────────────────────────────────
    let results_list = gtk::ListBox::new();
    results_list.set_selection_mode(gtk::SelectionMode::Single);
    results_list.add_css_class("boxed-list");

    // Scrollable wrapper so large result sets don't overflow the dialog.
    let results_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(0)
        .max_content_height(220)
        .propagate_natural_height(true)
        .child(&results_list)
        .visible(false)
        .build();

    // Hint shown when directory search returns nothing.
    let no_results_label = gtk::Label::builder()
        .label("No results found")
        .css_classes(["dim-label", "caption"])
        .halign(gtk::Align::Center)
        .margin_top(4)
        .visible(false)
        .build();

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_top(4)
        .build();
    content.append(&search_entry);
    content.append(&results_scroll);
    content.append(&no_results_label);

    let dialog = adw::AlertDialog::builder()
        .heading("Invite User")
        .extra_child(&content)
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("invite", "Invite");
    dialog.set_response_appearance("invite", adw::ResponseAppearance::Suggested);
    dialog.set_response_enabled("invite", false); // enabled only when a user is selected
    dialog.set_default_response(Some("invite"));

    // ── Populate results_list from a Vec<(display_name, user_id)> ────────────
    let populate_results = {
        let results_list = results_list.clone();
        let results_scroll = results_scroll.clone();
        let no_results_label = no_results_label.clone();
        let selected_uid = selected_uid.clone();
        let dialog_weak = dialog.downgrade();
        move |results: Vec<(String, String)>| {
            while let Some(child) = results_list.first_child() { results_list.remove(&child); }
            if results.is_empty() {
                results_scroll.set_visible(false);
                no_results_label.set_visible(true);
                return;
            }
            no_results_label.set_visible(false);
            for (name, uid) in &results {
                let row = adw::ActionRow::builder()
                    .title(name)
                    .subtitle(uid)
                    .activatable(true)
                    .build();
                results_list.append(&row);
            }
            results_scroll.set_visible(true);
            // Auto-select the first result so pressing Enter immediately works.
            if let Some(first) = results_list.first_child() {
                if let Some(row) = first.downcast_ref::<gtk::ListBoxRow>() {
                    results_list.select_row(Some(row));
                    *selected_uid.borrow_mut() = results[0].1.clone();
                    if let Some(d) = dialog_weak.upgrade() {
                        d.set_response_enabled("invite", true);
                    }
                }
            }
        }
    };

    // Row selection → update selected_uid.
    {
        let selected_uid = selected_uid.clone();
        let dialog_weak = dialog.downgrade();
        let results_list2 = results_list.clone();
        results_list.connect_row_selected(move |_, row| {
            let Some(row) = row else {
                *selected_uid.borrow_mut() = String::new();
                if let Some(d) = dialog_weak.upgrade() { d.set_response_enabled("invite", false); }
                return;
            };
            // Match row position back to subtitle (user_id).
            let mut child = results_list2.first_child();
            while let Some(w) = child {
                if let Some(lbr) = w.downcast_ref::<gtk::ListBoxRow>() {
                    if lbr == row { break; }
                }
                child = w.next_sibling();
            }
            if let Some(action_row) = row.child().and_downcast::<adw::ActionRow>() {
                *selected_uid.borrow_mut() = action_row.subtitle().unwrap_or_default().to_string();
                if let Some(d) = dialog_weak.upgrade() { d.set_response_enabled("invite", true); }
            }
        });
    }

    // ── Keystroke handler: local search + directory search ───────────────────
    {
        let local_members: Vec<(String, String, String)> = local_members.to_vec();
        let tx = tx.clone();
        let populate = populate_results.clone();
        let win_weak = win_weak.clone();
        let known_servers = crate::config::settings().known_servers.clone();
        search_entry.connect_changed(move |entry| {
            let query = entry.text().to_string();
            if query.is_empty() {
                populate(vec![]);
                return;
            }
            let q_lower = query.to_lowercase();

            // Instant local search (binary search on sorted members).
            let start = local_members.partition_point(|(lower, _, _)| lower.as_str() < q_lower.as_str());
            let local_results: Vec<(String, String)> = local_members[start..]
                .iter()
                .take_while(|(lower, _, _)| lower.starts_with(&q_lower))
                .take(10)
                .map(|(_, name, uid)| (name.clone(), uid.clone()))
                .collect();

            // If the query looks like a Matrix ID, add server suggestions from known_servers.
            let server_hints: Vec<(String, String)> = if !query.starts_with('@') && !query.contains(':') {
                known_servers.iter()
                    .map(|srv| (format!("@{query}:{srv}"), format!("@{query}:{srv}")))
                    .filter(|(_, uid)| !local_results.iter().any(|(_, u)| u == uid))
                    .collect()
            } else {
                vec![]
            };

            // Show local results immediately; directory search will overwrite.
            let combined: Vec<(String, String)> = local_results.iter().cloned()
                .chain(server_hints)
                .collect();
            populate(combined.clone());

            // Register this dialog's result callback in the window.
            let populate2 = populate.clone();
            let local2 = local_results.clone();
            let _q2 = query.clone();
            if let Some(win) = win_weak.upgrade() {
                *win.imp().user_search_cb.borrow_mut() = Some(Box::new(move |dir_results| {
                    // Merge: local results first (deduplicated), then directory results.
                    let local_uids: std::collections::HashSet<String> =
                        local2.iter().map(|(_, u)| u.clone()).collect();
                    let merged: Vec<(String, String)> = local2.iter().cloned()
                        .chain(dir_results.into_iter().filter(|(_, u)| !local_uids.contains(u)))
                        .take(20)
                        .collect();
                    populate2(merged);
                }));
            }

            // Fire directory search (results arrive asynchronously via UserSearchResults event).
            let tx2 = tx.clone();
            let q3 = query.clone();
            glib::spawn_future_local(async move {
                let _ = tx2.send(MatrixCommand::SearchUsers { query: q3 }).await;
            });
        });
    }

    // ── Confirm invite ────────────────────────────────────────────────────────
    let tx2 = tx.clone();
    let rid2 = room_id.to_string();
    let win_weak2 = win_weak.clone();
    dialog.connect_response(None, move |_, response| {
        // Always unregister the search callback when the dialog closes.
        if let Some(win) = win_weak2.upgrade() {
            *win.imp().user_search_cb.borrow_mut() = None;
        }
        if response == "invite" {
            let uid = selected_uid.borrow().clone();
            if !uid.is_empty() {
                let tx = tx2.clone();
                let room_id = rid2.clone();
                glib::spawn_future_local(async move {
                    let _ = tx.send(MatrixCommand::InviteUser { room_id, user_id: uid }).await;
                });
            }
        }
    });

    if let Some(w) = win_weak.upgrade() {
        dialog.present(Some(&w));
    } else {
        dialog.present(Some(fallback_parent));
    }
}

/// Detect whether `text` is a directly-joinable Matrix identifier or link.
///
/// Accepts:
///   - `https://matrix.to/#/!roomid:server`
///   - `https://matrix.to/#/#alias:server`
///   - `matrix:r/alias/server` / `matrix:roomid/id/server`
///   - `#alias:server`
///   - `!roomid:server`
///
/// Returns the canonical room ID or alias if recognised, else `None`.
fn parse_matrix_link_or_id(text: &str) -> Option<String> {
    let t = text.trim();
    // https://matrix.to/#/ prefix.
    if let Some(rest) = t.strip_prefix("https://matrix.to/#/") {
        let id = rest.split('?').next().unwrap_or(rest);
        let id = percent_decode_simple(id);
        if id.starts_with('!') || id.starts_with('#') {
            return Some(id);
        }
    }
    // matrix: URI scheme (MSC2312).
    if let Some(rest) = t.strip_prefix("matrix:r/") {
        let alias = rest.split('?').next().unwrap_or(rest);
        return Some(format!("#{}", alias.replacen('/', ":", 1)));
    }
    if let Some(rest) = t.strip_prefix("matrix:roomid/") {
        let id = rest.split('?').next().unwrap_or(rest);
        return Some(format!("!{}", id.replacen('/', ":", 1)));
    }
    // Bare room alias / ID (must contain a colon separating localpart from server).
    if (t.starts_with('#') || t.starts_with('!')) && t.contains(':') {
        return Some(t.to_string());
    }
    None
}

// ── bg_refresh coalescing helpers ────────────────────────────────────────────

/// Insert or replace the pending batch for `room_id`.
///
/// Returns `true` if this is the FIRST entry for the room (the caller should
/// schedule exactly one `idle_add_local_once`).  Returns `false` when an entry
/// already existed — the idle is already in flight and will consume the updated
/// batch when it fires, so no second idle is needed.
///
/// This is the core of the freeze fix: when 10 bg_refresh events arrive while
/// the window is unfocused, only the first call schedules an idle and all 10
/// calls update the slot with the latest data.  The idle fires once and calls
/// `set_messages()` exactly once with the most recent batch.
/// Returns true iff the bg_refresh batch for the currently-visible room should
/// be consumed synchronously instead of deferred to idle_add_local_once.
///
/// Synchronous processing is correct only when the loading spinner is shown:
/// the spinner's begin_updating() keeps a tick callback at GDK_PRIORITY_REDRAW
/// (120), which starves idle_add_local_once (priority 200) indefinitely.
/// When messages are already visible (is_loading = false), the spinner is off
/// and the idle fires promptly; calling synchronously there blocks the current
/// frame and causes visible scroll jank.
pub(crate) fn should_process_bg_refresh_sync(
    window_active: bool,
    view_is_loading: bool,
) -> bool {
    window_active && view_is_loading
}

fn bg_refresh_insert(
    map: &mut std::collections::HashMap<String, (Vec<crate::matrix::MessageInfo>, Option<String>)>,
    room_id: &str,
    messages: Vec<crate::matrix::MessageInfo>,
    token: Option<String>,
) -> bool {
    let had = map.contains_key(room_id);
    map.insert(room_id.to_owned(), (messages, token));
    had
}

/// Minimal percent-decode for %21 and %23 (common in matrix.to links).
fn percent_decode_simple(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""),
                16,
            ) {
                out.push(hex);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::{bg_refresh_insert, should_process_bg_refresh_sync};
    use crate::matrix::MessageInfo;
    use std::collections::HashMap;

    fn make_msg(event_id: &str) -> MessageInfo {
        MessageInfo {
            sender: "Alice".into(),
            sender_id: "@alice:example.com".into(),
            body: "hi".into(),
            formatted_body: None,
            timestamp: 1_000,
            event_id: event_id.into(),
            reply_to: None,
            reply_to_sender: None,
            thread_root: None,
            reactions: vec![],
            media: None,
            is_highlight: false,
            is_system_event: false,
        }
    }

    fn batch(ids: &[&str]) -> Vec<MessageInfo> {
        ids.iter().map(|id| make_msg(id)).collect()
    }

    // ── Core scheduling invariant ─────────────────────────────────────────────

    #[test]
    fn first_insert_returns_false_meaning_schedule_idle() {
        // had = false → !had_pending → idle is scheduled
        let mut map = HashMap::new();
        let had = bg_refresh_insert(&mut map, "!room:example.com", batch(&["$ev1"]), None);
        assert!(!had, "first insert should signal that idle must be scheduled");
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn second_insert_returns_true_meaning_no_new_idle() {
        // had = true → idle already in flight, caller must NOT schedule another
        let mut map = HashMap::new();
        bg_refresh_insert(&mut map, "!room:example.com", batch(&["$ev1"]), None);
        let had = bg_refresh_insert(&mut map, "!room:example.com", batch(&["$ev2"]), None);
        assert!(had, "second insert should signal idle already scheduled");
    }

    // ── Latest-data-wins ─────────────────────────────────────────────────────

    #[test]
    fn latest_batch_replaces_earlier_one() {
        let mut map = HashMap::new();
        bg_refresh_insert(&mut map, "!room:example.com", batch(&["$ev1"]), None);
        bg_refresh_insert(
            &mut map,
            "!room:example.com",
            batch(&["$ev1", "$ev2", "$ev3"]),
            Some("tok_abc".into()),
        );
        let (msgs, token) = map.remove("!room:example.com").unwrap();
        assert_eq!(msgs.len(), 3, "map must hold the latest (largest) batch");
        assert_eq!(token.as_deref(), Some("tok_abc"));
    }

    #[test]
    fn token_updated_on_second_insert() {
        let mut map = HashMap::new();
        bg_refresh_insert(&mut map, "!r:s", batch(&["$a"]), Some("old_tok".into()));
        bg_refresh_insert(&mut map, "!r:s", batch(&["$b"]), Some("new_tok".into()));
        let (_, token) = map.remove("!r:s").unwrap();
        assert_eq!(token.as_deref(), Some("new_tok"));
    }

    // ── Room isolation ───────────────────────────────────────────────────────

    #[test]
    fn different_rooms_tracked_independently() {
        let mut map = HashMap::new();
        // First events for two distinct rooms — both should say "schedule idle".
        let had_a = bg_refresh_insert(&mut map, "!room_a:example.com", batch(&["$a1"]), None);
        let had_b = bg_refresh_insert(&mut map, "!room_b:example.com", batch(&["$b1"]), None);
        assert!(!had_a, "room_a first insert should schedule idle");
        assert!(!had_b, "room_b first insert should schedule idle (independent)");
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn room_a_second_insert_does_not_affect_room_b_schedule() {
        let mut map = HashMap::new();
        bg_refresh_insert(&mut map, "!room_a:s", batch(&["$a1"]), None);
        bg_refresh_insert(&mut map, "!room_b:s", batch(&["$b1"]), None);
        // Second event for room_a — should return true (idle already pending for a).
        let had_a2 = bg_refresh_insert(&mut map, "!room_a:s", batch(&["$a2"]), None);
        // Second event for room_b — should also return true independently.
        let had_b2 = bg_refresh_insert(&mut map, "!room_b:s", batch(&["$b2"]), None);
        assert!(had_a2);
        assert!(had_b2);
        // Latest batches are correct for each room.
        assert_eq!(map["!room_a:s"].0[0].event_id, "$a2");
        assert_eq!(map["!room_b:s"].0[0].event_id, "$b2");
    }

    // ── After idle fires (remove), next insert re-schedules ──────────────────

    #[test]
    fn after_remove_next_insert_schedules_idle_again() {
        let mut map = HashMap::new();
        bg_refresh_insert(&mut map, "!r:s", batch(&["$ev1"]), None);
        // Simulate idle firing: it removes the entry.
        map.remove("!r:s");
        // New bg_refresh event arrives — must schedule a fresh idle.
        let had = bg_refresh_insert(&mut map, "!r:s", batch(&["$ev2"]), None);
        assert!(!had, "after idle consumed the entry, next insert must schedule a new idle");
    }

    // ── retain on room switch ────────────────────────────────────────────────

    #[test]
    fn retain_clears_other_rooms_keeps_current() {
        let mut map = HashMap::new();
        bg_refresh_insert(&mut map, "!current:s", batch(&["$c1"]), None);
        bg_refresh_insert(&mut map, "!other_a:s", batch(&["$a1"]), None);
        bg_refresh_insert(&mut map, "!other_b:s", batch(&["$b1"]), None);
        // Simulate the room-switch retain call.
        map.retain(|rid, _| rid == "!current:s");
        assert!(map.contains_key("!current:s"), "current room entry must survive retain");
        assert!(!map.contains_key("!other_a:s"), "stale room_a entry must be dropped");
        assert!(!map.contains_key("!other_b:s"), "stale room_b entry must be dropped");
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn retain_clears_all_when_no_room_matches() {
        let mut map = HashMap::new();
        bg_refresh_insert(&mut map, "!a:s", batch(&["$1"]), None);
        bg_refresh_insert(&mut map, "!b:s", batch(&["$2"]), None);
        // Switch to a room for which no bg_refresh is pending.
        map.retain(|rid, _| rid == "!new_room:s");
        assert!(map.is_empty());
    }

    // ── should_process_bg_refresh_sync ──────────────────────────────────────
    // Regression guard: we introduced scroll jank by processing synchronously
    // even when messages were already visible (is_loading=false).  The gate
    // must restrict sync processing to the loading-spinner state only.

    #[test]
    fn sync_only_when_active_and_loading() {
        // Nominal case: window has focus AND view is showing the loading spinner.
        assert!(should_process_bg_refresh_sync(true, true));
    }

    #[test]
    fn no_sync_when_messages_visible() {
        // Regression case: messages already shown (user scrolling) — must defer
        // to idle so we don't block the frame and cause scroll jank.
        assert!(!should_process_bg_refresh_sync(true, false));
    }

    #[test]
    fn no_sync_when_window_inactive() {
        // Unfocused window: even if spinner is showing, defer to idle;
        // the notify::is-active handler will drain pending_bg_refresh on focus.
        assert!(!should_process_bg_refresh_sync(false, true));
    }

    #[test]
    fn no_sync_when_inactive_and_not_loading() {
        assert!(!should_process_bg_refresh_sync(false, false));
    }

    // ── synchronous-consume then re-schedule scenario ────────────────────────
    // After the active+loading path removes the entry from pending_bg_refresh,
    // a subsequent bg_refresh for the same room must schedule a fresh idle
    // (had_pending = false) so the data is not silently dropped.

    #[test]
    fn sync_consume_then_idle_reschedule() {
        let mut map = HashMap::new();
        // First batch arrives while loading — it will be consumed synchronously
        // (not via idle), so no idle is scheduled.  The entry is inserted, then
        // immediately removed by the sync path.
        let had1 = bg_refresh_insert(&mut map, "!r:s", batch(&["$ev1"]), None);
        assert!(!had1, "first insert must not see a prior entry");
        // Simulate the sync path consuming the entry.
        map.remove("!r:s");
        // A follow-up batch (e.g. server push after first load) must reschedule
        // — had_pending must be false so the idle is not suppressed.
        let had2 = bg_refresh_insert(&mut map, "!r:s", batch(&["$ev2"]), None);
        assert!(!had2, "after sync consume, next insert must reschedule idle");
        assert_eq!(map["!r:s"].0[0].event_id, "$ev2");
    }
}
