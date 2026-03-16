// MxWindow — the main application window.
//
// Starts with a login page. After successful login, swaps to an
// AdwNavigationSplitView with the room list sidebar and message view.

mod imp {
    use adw::prelude::*;
    use adw::subclass::prelude::*;
    use gtk::glib;

    use async_channel::{Receiver, Sender};
    use std::cell::{OnceCell, RefCell};

    use crate::matrix::{MatrixCommand, MatrixEvent};
    use crate::widgets::LoginPage;
    use crate::widgets::MessageView;
    use crate::widgets::RoomListView;

    pub struct MxWindow {
        pub event_rx: OnceCell<Receiver<MatrixEvent>>,
        pub command_tx: OnceCell<Sender<MatrixCommand>>,
        pub login_page: LoginPage,
        pub room_list_view: RoomListView,
        pub message_view: MessageView,
        pub toast_overlay: adw::ToastOverlay,
        pub toolbar: adw::ToolbarView,
        pub loading_spinner: gtk::Spinner,
        pub verify_banner: adw::Banner,
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
        /// Info button in the content header (shown only when a room is selected).
        pub info_button: OnceCell<gtk::Button>,
        /// Bookmark toggle button.
        pub bookmark_button: OnceCell<gtk::Button>,
        /// Current user ID for deduplicating local echo.
        pub user_id: RefCell<String>,
        /// Media cache: mxc_url → local file path.
        pub media_cache: RefCell<std::collections::HashMap<String, String>>,
        /// Widget to anchor the next media preview popover to.
        pub media_preview_anchor: RefCell<Option<gtk::Widget>>,
        /// Shared media preview popover — reused across hovers.
        pub media_popover: gtk::Popover,
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
                login_page: LoginPage::new(),
                room_list_view: RoomListView::new(),
                message_view: MessageView::new(),
                toast_overlay: adw::ToastOverlay::new(),
                toolbar: adw::ToolbarView::new(),
                loading_spinner: gtk::Spinner::new(),
                verify_banner,
                current_room_id: RefCell::new(None),
                content_page: OnceCell::new(),
                verify_dialog: RefCell::new(None),
                current_room_meta: RefCell::new(None),
                details_revealer: gtk::Revealer::builder()
                    .transition_type(gtk::RevealerTransitionType::SlideLeft)
                    .reveal_child(false)
                    .build(),
                details_content: gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .width_request(280)
                    .build(),
                info_button: OnceCell::new(),
                bookmark_button: OnceCell::new(),
                user_id: RefCell::new(String::new()),
                media_cache: RefCell::new(std::collections::HashMap::new()),
                media_preview_anchor: RefCell::new(None),
                media_popover: {
                    let p = gtk::Popover::new();
                    p.set_autohide(true);
                    p.set_has_arrow(true);
                    p
                },
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

/// Show a simple toast message.
/// Show a media preview using a shared popover on the MxWindow.
/// Show a media preview in-app using a dialog with gtk::Picture or gtk::Video.
fn show_media_preview(window: &MxWindow, _anchor: &gtk::Widget, path: &str) {
    let path_lower = path.to_lowercase();

    let dialog = adw::Dialog::builder()
        .content_width(600)
        .content_height(500)
        .title("Media Preview")
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());

    if path_lower.ends_with(".mp4")
        || path_lower.ends_with(".webm")
        || path_lower.ends_with(".mov")
    {
        let video = gtk::Video::for_filename(Some(path));
        video.set_autoplay(true);
        video.set_vexpand(true);
        video.set_hexpand(true);
        toolbar.set_content(Some(&video));
    } else if path_lower.ends_with(".gif") {
        let media_file = gtk::MediaFile::for_filename(path);
        media_file.set_loop(true);
        media_file.play();
        let video = gtk::Video::new();
        video.set_media_stream(Some(&media_file));
        video.set_vexpand(true);
        video.set_hexpand(true);
        toolbar.set_content(Some(&video));
    } else {
        // Image — use gio::File to handle paths with spaces.
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
}

/// Auto-dismiss the media preview popover after 15 seconds.
fn auto_dismiss_preview(win_weak: glib::WeakRef<MxWindow>) {
    glib::timeout_add_local_once(std::time::Duration::from_secs(15), move || {
        if let Some(win) = win_weak.upgrade() {
            win.imp().media_popover.popdown();
        }
    });
}

fn toast(overlay: &adw::ToastOverlay, msg: &str) {
    overlay.add_toast(adw::Toast::new(msg));
}

/// Show a toast with a formatted error message.
fn toast_error(overlay: &adw::ToastOverlay, prefix: &str, error: &str) {
    overlay.add_toast(adw::Toast::new(&format!("{prefix}: {error}")));
}

/// Apply font settings via a CSS provider on the default display.
fn apply_font_css(settings: &AppearanceSettings) {
    let css = if settings.font_family.is_empty() {
        format!(".mx-message-body {{ font-size: {}pt; }}", settings.font_size)
    } else {
        format!(
            ".mx-message-body {{ font-family: \"{}\"; font-size: {}pt; }}",
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
    ) -> Self {
        let window: Self = glib::Object::builder()
            .property("application", app)
            .build();

        let imp = window.imp();
        let _ = imp.event_rx.set(event_rx.clone());
        let _ = imp.command_tx.set(command_tx.clone());

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

        // Wire up room selection → send SelectRoom command.
        let cmd_tx = command_tx.clone();
        let window_weak = window.downgrade();
        let msg_view = imp.message_view.clone();
        let room_list = imp.room_list_view.clone();
        imp.room_list_view.connect_room_selected(move |room_id, room_name| {
            if let Some(window) = window_weak.upgrade() {
                window.imp().current_room_id.replace(Some(room_id.clone()));
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
                // Hide details sidebar when switching rooms.
                window.imp().details_revealer.set_reveal_child(false);
            }
            // Clear unread badge immediately — don't wait for server round-trip.
            room_list.clear_unread(&room_id);
            // Clear old messages while we fetch the new room's messages.
            msg_view.clear();
            let tx = cmd_tx.clone();
            let rid = room_id.clone();
            glib::spawn_future_local(async move {
                let _ = tx.send(MatrixCommand::SelectRoom { room_id: rid }).await;
            });
        });

        // Leave button is wired in setup_ui.

        // Wire up "Join Room" banner in space drill-down.
        let cmd_tx_browse = command_tx.clone();
        imp.room_list_view.connect_browse_space(move |space_id| {
            let tx = cmd_tx_browse.clone();
            let sid = space_id.clone();
            glib::spawn_future_local(async move {
                let _ = tx.send(MatrixCommand::BrowseSpaceRooms { space_id: sid }).await;
            });
        });

        // Wire up "Join Room" banner in Rooms tab — fetch public rooms.
        let cmd_tx_browse = command_tx.clone();
        imp.room_list_view.connect_browse_rooms(move || {
            let tx = cmd_tx_browse.clone();
            glib::spawn_future_local(async move {
                let _ = tx.send(MatrixCommand::BrowsePublicRooms { search_term: None }).await;
            });
        });

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
                    let _ = tx.send(MatrixCommand::EditMessage { room_id, event_id, new_body }).await;
                });
            }
        });

        let cmd_tx_edit2 = command_tx.clone();
        imp.message_view.connect_send_message(move |body, reply_to, quote_text| {
            let room_id = window_weak
                .upgrade()
                .and_then(|w| w.imp().current_room_id.borrow().clone());
            if let Some(room_id) = room_id {
                // Check if this is an edit (reply_to starts with "edit:").
                if let Some(ref rt) = reply_to {
                    if let Some(event_id) = rt.strip_prefix("edit:") {
                        // Update locally immediately.
                        msg_view_for_send.update_message_body(event_id, &body);

                        let tx = cmd_tx_edit2.clone();
                        let eid = event_id.to_string();
                        let new_body = body.clone();
                        glib::spawn_future_local(async move {
                            let _ = tx.send(MatrixCommand::EditMessage {
                                room_id, event_id: eid, new_body,
                            }).await;
                        });
                        return;
                    }
                }

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
                    body: body.clone(),
                    timestamp: now,
                    event_id: String::new(),
                    reply_to: reply_to.clone(),
                    thread_root: None,
                    reactions: Vec::new(),
                    media: None,
                };
                msg_view_for_send.append_message(&echo);

                let tx = cmd_tx.clone();
                glib::spawn_future_local(async move {
                    let _ = tx.send(MatrixCommand::SendMessage { room_id, body, reply_to, quote_text }).await;
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
                // Show reaction immediately — always add locally.
                // The server handles dedup/removal. Next sync corrects count.
                msg_view_react.add_reaction(&event_id, &emoji);

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
                    timestamp: now,
                    event_id: String::new(),
                    reply_to: None,
                    thread_root: None,
                    reactions: Vec::new(),
                    media: Some(crate::matrix::MediaInfo {
                        kind,
                        filename: filename.clone(),
                        size,
                        url: format!("file://{file_path}"),
                        source_json: String::new(),
                    }),
                };
                msg_view_attach.append_message(&echo);

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

        // Event loop.
        let toast_overlay = imp.toast_overlay.clone();
        let login_page = imp.login_page.clone();
        let room_list_view = imp.room_list_view.clone();
        let message_view = imp.message_view.clone();
        let window_weak = window.downgrade();
        glib::spawn_future_local(async move {
            while let Ok(event) = event_rx.recv().await {
                let Some(window) = window_weak.upgrade() else {
                    break;
                };
                match event {
                    MatrixEvent::LoginRequired => {
                        // No saved session — show the login page.
                        window.show_login();
                    }
                    MatrixEvent::LoginSuccess { display_name, user_id } => {
                        let msg = format!("Logged in as {display_name}");
                        toast(&toast_overlay, &msg);
                        login_page.stop_spinner();
                        tracing::info!("{msg}");
                        window.imp().user_id.replace(user_id.clone());
                        message_view.set_user_id(&user_id);
                        // Highlight the user's display name, localpart, and
                        // full user ID so mentions in any form get highlighted.
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
                        window.show_main_view();
                    }
                    MatrixEvent::LoginFailed { error } => {
                        toast_error(&toast_overlay, "Login failed", &error);
                        login_page.stop_spinner();
                        login_page.set_sensitive(true);
                        window.show_login();
                    }
                    MatrixEvent::SyncStarted => {
                        tracing::info!("Initial sync started…");
                    }
                    MatrixEvent::SyncError { error } => {
                        tracing::error!("Sync error: {error}");
                        toast_error(&toast_overlay, "Sync error", &error);
                    }
                    MatrixEvent::RoomListUpdated { rooms } => {
                        room_list_view.update_rooms(&rooms);
                    }
                    MatrixEvent::RoomMessages { room_id, messages, prev_batch_token, room_meta } => {
                        let current = window.imp().current_room_id.borrow().clone();
                        if current.as_deref() == Some(&room_id) {
                            window.imp().current_room_meta.replace(Some(room_meta.clone()));
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
                            message_view.set_messages(&messages, prev_batch_token);
                        }
                    }
                    MatrixEvent::OlderMessages { room_id, messages, prev_batch_token } => {
                        let current = window.imp().current_room_id.borrow().clone();
                        if current.as_deref() == Some(&room_id) {
                            message_view.prepend_messages(&messages, prev_batch_token);
                        }
                    }
                    MatrixEvent::NewMessage { room_id, room_name, sender_id, message, is_mention } => {
                        let current = window.imp().current_room_id.borrow().clone();
                        let my_id = window.imp().user_id.borrow().clone();
                        // Skip our own messages — already shown as local echo.
                        let is_self = sender_id == my_id;
                        if current.as_deref() == Some(&room_id) && !is_self {
                            message_view.append_message(&message);
                        }

                        // Desktop notification for mentions.
                        if is_mention {
                            let app = window.application().unwrap();
                            let notif = gio::Notification::new(&format!(
                                "Mentioned in {}", room_name
                            ));
                            notif.set_body(Some(&format!(
                                "{}: {}", message.sender, &message.body[..message.body.len().min(100)]
                            )));
                            notif.set_priority(gio::NotificationPriority::High);
                            app.send_notification(
                                Some(&format!("mention-{}", room_id)),
                                &notif,
                            );
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
                            .title("Device verified! Use menu → Recover Encryption Keys to decrypt old messages.")
                            .timeout(8)
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
                    MatrixEvent::DeviceUnverified => {
                        window.imp().verify_banner.set_revealed(true);
                    }
                    MatrixEvent::RecoveryComplete => {
                        toast(&toast_overlay, "Encryption keys recovered! Re-select a room to decrypt messages.");
                    }
                    MatrixEvent::RecoveryFailed { error } => {
                        toast_error(&toast_overlay, "Key recovery failed", &error);
                    }
                    MatrixEvent::MediaReady { url, path } => {
                        // Cache the downloaded path.
                        window.imp().media_cache.borrow_mut().insert(url, path.clone());

                        // Open with system viewer.
                        show_media_preview(&window, window.upcast_ref::<gtk::Widget>(), &path);
                    }
                    MatrixEvent::RoomJoined { room_id: _, room_name } => {
                        toast(&toast_overlay, &format!("Joined {room_name}"));
                    }
                    MatrixEvent::JoinFailed { error } => {
                        toast_error(&toast_overlay, "Failed to join", &error);
                    }
                    MatrixEvent::PublicRoomDirectory { rooms } => {
                        window.show_space_directory(&rooms);
                    }
                    MatrixEvent::SpaceDirectory { space_id: _, rooms } => {
                        window.show_space_directory(&rooms);
                    }
                    MatrixEvent::RoomLeft { room_id: _ } => {
                        // Room list will refresh on next sync.
                    }
                    MatrixEvent::LeaveFailed { error } => {
                        toast_error(&toast_overlay, "Failed to leave", &error);
                    }
                }
            }
        });

        window
    }

    fn show_login(&self) {
        let imp = self.imp();
        imp.toolbar.set_content(Some(&imp.login_page));
    }

    fn show_main_view(&self) {
        let imp = self.imp();

        // Register actions for the menu.
        self.setup_actions();

        // Sidebar header with hamburger menu.
        let sidebar_header = adw::HeaderBar::new();
        let menu = gio::Menu::new();
        let main_section = gio::Menu::new();
        main_section.append(Some("_Verify Device"), Some("win.verify"));
        main_section.append(Some("_Recover Encryption Keys"), Some("win.recover-keys"));
        main_section.append(Some("_Preferences"), Some("win.preferences"));
        menu.append_section(None, &main_section);
        let about_section = gio::Menu::new();
        about_section.append(Some("_About Matx"), Some("win.about"));
        menu.append_section(None, &about_section);
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .build();
        sidebar_header.pack_end(&menu_button);

        let sidebar_toolbar = adw::ToolbarView::new();
        sidebar_toolbar.add_top_bar(&sidebar_header);
        sidebar_toolbar.set_content(Some(&imp.room_list_view));

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

        // Store button references so room selection can show/hide them.
        let _ = imp.info_button.set(info_button.clone());
        let _ = imp.bookmark_button.set(bookmark_button.clone());

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
            } else {
                window.show_room_details();
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
        details_close_btn.connect_clicked(move |_| {
            revealer_for_close.set_reveal_child(false);
        });
        let details_wrapper = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        details_wrapper.append(&details_scroll);
        details_wrapper.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        details_wrapper.append(&details_close_btn);
        imp.details_revealer.set_child(Some(&details_wrapper));

        // Content area: message view + optional details sidebar.
        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .build();
        content_box.append(&imp.message_view);
        content_box.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        content_box.append(&imp.details_revealer);
        // Make message view expand, sidebar stays fixed width.
        imp.message_view.set_hexpand(true);

        let content_toolbar = adw::ToolbarView::new();
        content_toolbar.add_top_bar(&content_header);
        content_toolbar.set_content(Some(&content_box));

        let content_page = adw::NavigationPage::builder()
            .title("Matx")
            .child(&content_toolbar)
            .build();
        let _ = imp.content_page.set(content_page.clone());

        let split_view = adw::NavigationSplitView::new();
        split_view.set_sidebar(Some(&sidebar_page));
        split_view.set_content(Some(&content_page));
        split_view.set_min_sidebar_width(160.0);
        split_view.set_max_sidebar_width(240.0);
        split_view.set_sidebar_width_fraction(0.25);

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
                window.imp().verify_dialog.replace(Some(dialog));
                glib::spawn_future_local(async move {
                    let _ = tx
                        .send(MatrixCommand::RequestSelfVerification)
                        .await;
                });
            }
        });

        // Use a ToolbarView to place the banner above the split view,
        // spanning the full window width.
        let main_toolbar = adw::ToolbarView::new();
        main_toolbar.add_top_bar(&imp.verify_banner);
        main_toolbar.set_content(Some(&split_view));

        imp.toast_overlay.set_child(Some(&main_toolbar));

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

        // Apply font settings from config.
        apply_font_css(&crate::config::settings().appearance);

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
            .join-banner {
                background: #3584e4;
                color: white;
                border-radius: 0;
                border: none;
                font-weight: bold;
            }
            .join-banner:hover {
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
                transition: opacity 300ms ease-in-out;
            }
            .msg-action-bar-visible {
                opacity: 0.85;
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
            .mention-row {
                background: alpha(@accent_bg_color, 0.12);
                border-radius: 8px;
                border-left: 3px solid @accent_bg_color;
                padding: 6px 10px;
                margin: 2px 4px;
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

        let join_action = ActionEntryBuilder::new("join-room")
            .activate(|window: &Self, _, _| {
                window.show_join_bar();
            })
            .build();

        self.add_action_entries([about_action, preferences_action, verify_action, recover_action, join_action]);
    }

    fn show_join_bar(&self) {
        let imp = self.imp();
        let toast_overlay = imp.toast_overlay.clone();
        let tx = imp.command_tx.get().unwrap().clone();

        // Create an inline entry bar for the room ID/alias.
        let entry = gtk::Entry::builder()
            .placeholder_text("#room:server or !id:server")
            .hexpand(true)
            .build();
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

        // Use a Revealer to slide in the join bar.
        let revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .reveal_child(true)
            .child(&bar)
            .build();

        // Insert the revealer at the top of the room list.
        imp.room_list_view.prepend(&revealer);
        entry.grab_focus();

        // Join on Enter or button click.
        let tx2 = tx.clone();
        let entry2 = entry.clone();
        let revealer2 = revealer.clone();
        let toast2 = toast_overlay.clone();
        let do_join = move || {
            let text = entry2.text().to_string();
            if text.is_empty() {
                return;
            }
            let tx = tx2.clone();
            let revealer = revealer2.clone();
            let toast = toast2.clone();
            glib::spawn_future_local(async move {
                let _ = tx.send(MatrixCommand::JoinRoom { room_id_or_alias: text }).await;
                revealer.set_reveal_child(false);
                // Remove after animation.
                glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                    if let Some(parent) = revealer.parent() {
                        if let Some(b) = parent.downcast_ref::<gtk::Box>() {
                            b.remove(&revealer);
                        }
                    }
                });
            });
        };

        let join_fn = do_join.clone();
        join_btn.connect_clicked(move |_| join_fn());
        let join_fn = do_join;
        entry.connect_activate(move |_| join_fn());

        // Cancel hides the bar.
        let revealer3 = revealer.clone();
        cancel_btn.connect_clicked(move |_| {
            revealer3.set_reveal_child(false);
            let r = revealer3.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                if let Some(parent) = r.parent() {
                    if let Some(b) = parent.downcast_ref::<gtk::Box>() {
                        b.remove(&r);
                    }
                }
            });
        });
    }

    fn show_space_directory(&self, rooms: &[crate::matrix::SpaceDirectoryRoom]) {
        let dialog = adw::Dialog::builder()
            .title("Join a Room")
            .content_width(400)
            .content_height(500)
            .build();

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        toolbar.add_top_bar(&header);

        let list_box = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .margin_start(12)
            .margin_end(12)
            .margin_top(6)
            .margin_bottom(12)
            .build();

        for room in rooms {
            let row = adw::ActionRow::builder()
                .title(&room.name)
                .subtitle(&if room.topic.is_empty() {
                    format!("{} members", room.member_count)
                } else {
                    format!("{} — {} members", room.topic, room.member_count)
                })
                .activatable(true)
                .build();

            if room.already_joined {
                let badge = gtk::Label::builder()
                    .label("Joined")
                    .css_classes(["dim-label", "caption"])
                    .build();
                row.add_suffix(&badge);
            } else {
                let join_btn = gtk::Button::builder()
                    .label("Join")
                    .css_classes(["suggested-action"])
                    .valign(gtk::Align::Center)
                    .build();
                let tx = self.imp().command_tx.get().unwrap().clone();
                let rid = room.room_id.clone();
                let _toast_overlay = self.imp().toast_overlay.clone();
                join_btn.connect_clicked(move |btn| {
                    btn.set_sensitive(false);
                    btn.set_label("Joining…");
                    let tx = tx.clone();
                    let rid = rid.clone();
                    glib::spawn_future_local(async move {
                        let _ = tx.send(MatrixCommand::JoinRoom {
                            room_id_or_alias: rid,
                        }).await;
                    });
                });
                row.add_suffix(&join_btn);
            }

            list_box.append(&row);
        }

        if rooms.is_empty() {
            let empty = adw::StatusPage::builder()
                .icon_name("system-search-symbolic")
                .title("No Rooms Found")
                .description("This space doesn't have any discoverable rooms.")
                .build();
            toolbar.set_content(Some(&empty));
        } else {
            let scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&list_box)
                .build();
            toolbar.set_content(Some(&scroll));
        }

        dialog.set_child(Some(&toolbar));
        dialog.present(Some(self));
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

        // Members list.
        if !meta.members.is_empty() {
            container.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            let members_header = gtk::Label::builder()
                .label(&format!("Members ({})", meta.members.len()))
                .css_classes(["heading", "caption"])
                .halign(gtk::Align::Start)
                .build();
            container.append(&members_header);

            for (uid, name) in meta.members.iter().take(50) {
                let row = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .spacing(1)
                    .margin_top(2)
                    .margin_bottom(2)
                    .build();
                row.append(&gtk::Label::builder()
                    .label(name)
                    .halign(gtk::Align::Start)
                    .css_classes(["caption"])
                    .build());
                row.append(&gtk::Label::builder()
                    .label(uid)
                    .halign(gtk::Align::Start)
                    .css_classes(["caption", "dim-label"])
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .build());
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

    fn show_about_dialog(&self) {
        let dialog = adw::AboutDialog::builder()
            .application_name(crate::config::APP_NAME)
            .application_icon(crate::config::APP_ID)
            .developer_name("Matx Contributors")
            .version("0.1.0")
            .comments("A Matrix client built with Rust and libadwaita, designed around activity awareness.")
            .website("https://github.com/matx")
            .license_type(gtk::License::Gpl30)
            .build();

        dialog.present(Some(self));
    }

    fn show_preferences(&self) {
        use crate::config;

        let dialog = adw::PreferencesDialog::new();
        let cfg = config::settings().clone();

        // --- Rooms group ---
        let rooms_group = adw::PreferencesGroup::builder()
            .title("Rooms")
            .description("How many rooms to show in the sidebar")
            .build();

        let max_dms_row = adw::SpinRow::builder()
            .title("Max DMs")
            .subtitle("Maximum direct messages shown")
            .adjustment(&gtk::Adjustment::new(
                cfg.rooms.max_dms as f64, 5.0, 500.0, 5.0, 25.0, 0.0,
            ))
            .build();
        rooms_group.add(&max_dms_row);

        let max_rooms_row = adw::SpinRow::builder()
            .title("Max Rooms")
            .subtitle("Maximum rooms shown")
            .adjustment(&gtk::Adjustment::new(
                cfg.rooms.max_rooms as f64, 5.0, 1000.0, 10.0, 50.0, 0.0,
            ))
            .build();
        rooms_group.add(&max_rooms_row);

        // --- Sync group ---
        let sync_group = adw::PreferencesGroup::builder()
            .title("Sync")
            .description("Matrix sync settings (changes apply on restart)")
            .build();

        let timeline_row = adw::SpinRow::builder()
            .title("Timeline Limit")
            .subtitle("Events fetched per room during sync")
            .adjustment(&gtk::Adjustment::new(
                cfg.sync.timeline_limit as f64, 1.0, 50.0, 1.0, 5.0, 0.0,
            ))
            .build();
        sync_group.add(&timeline_row);

        let timeout_row = adw::SpinRow::builder()
            .title("Sync Timeout")
            .subtitle("Seconds to wait for sync response")
            .adjustment(&gtk::Adjustment::new(
                cfg.sync.timeout_secs as f64, 10.0, 300.0, 10.0, 30.0, 0.0,
            ))
            .build();
        sync_group.add(&timeout_row);

        // --- Appearance group ---
        let appearance_group = adw::PreferencesGroup::builder()
            .title("Appearance")
            .description("Font settings for message text")
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

        // --- Info group ---
        let info_group = adw::PreferencesGroup::builder()
            .title("Storage")
            .build();

        let config_path_row = adw::ActionRow::builder()
            .title("Config File")
            .subtitle("~/.config/matx/config.toml")
            .build();
        info_group.add(&config_path_row);

        let page = adw::PreferencesPage::builder()
            .icon_name("preferences-system-symbolic")
            .title("General")
            .build();
        page.add(&rooms_group);
        page.add(&sync_group);
        page.add(&appearance_group);
        page.add(&info_group);
        dialog.add(&page);

        // Save when values change (font is saved directly from the FontDialog callback).
        let save = {
            let max_dms_row = max_dms_row.clone();
            let max_rooms_row = max_rooms_row.clone();
            let timeline_row = timeline_row.clone();
            let timeout_row = timeout_row.clone();
            let cfg = cfg.clone();
            move || {
                let mut new_cfg = cfg.clone();
                new_cfg.rooms.max_dms = max_dms_row.value() as usize;
                new_cfg.rooms.max_rooms = max_rooms_row.value() as usize;
                new_cfg.sync.timeline_limit = timeline_row.value() as u32;
                new_cfg.sync.timeout_secs = timeout_row.value() as u64;
                if let Err(e) = config::save_settings(&new_cfg) {
                    tracing::error!("Failed to save settings: {e}");
                }
            }
        };

        let s = save.clone();
        max_dms_row.connect_value_notify(move |_| s());
        let s = save.clone();
        max_rooms_row.connect_value_notify(move |_| s());
        let s = save.clone();
        timeline_row.connect_value_notify(move |_| s());
        let s = save.clone();
        timeout_row.connect_value_notify(move |_| s());

        dialog.present(Some(self));
    }
}
