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

use crate::matrix::{MatrixCommand, MatrixEvent};

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
        imp.room_list_view.connect_room_selected(move |room_id, room_name| {
            if let Some(window) = window_weak.upgrade() {
                window.imp().current_room_id.replace(Some(room_id.clone()));
                if let Some(page) = window.imp().content_page.get() {
                    page.set_title(&room_name);
                }
            }
            // Clear old messages while we fetch the new room's messages.
            msg_view.clear();
            let tx = cmd_tx.clone();
            let rid = room_id.clone();
            glib::spawn_future_local(async move {
                let _ = tx.send(MatrixCommand::SelectRoom { room_id: rid }).await;
            });
        });

        // Wire up send message → send SendMessage command.
        let cmd_tx = command_tx.clone();
        let window_weak = window.downgrade();
        imp.message_view.connect_send_message(move |body| {
            let room_id = window_weak
                .upgrade()
                .and_then(|w| w.imp().current_room_id.borrow().clone());
            if let Some(room_id) = room_id {
                let tx = cmd_tx.clone();
                glib::spawn_future_local(async move {
                    let _ = tx.send(MatrixCommand::SendMessage { room_id, body }).await;
                });
            }
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
                    MatrixEvent::LoginSuccess { display_name } => {
                        let msg = format!("Logged in as {display_name}");
                        toast_overlay.add_toast(adw::Toast::new(&msg));
                        login_page.stop_spinner();
                        tracing::info!("{msg}");
                        window.show_main_view();
                    }
                    MatrixEvent::LoginFailed { error } => {
                        toast_overlay.add_toast(adw::Toast::new(&format!("Login failed: {error}")));
                        login_page.stop_spinner();
                        login_page.set_sensitive(true);
                        window.show_login();
                    }
                    MatrixEvent::SyncStarted => {
                        tracing::info!("Initial sync started…");
                    }
                    MatrixEvent::SyncError { error } => {
                        tracing::error!("Sync error: {error}");
                        toast_overlay.add_toast(adw::Toast::new(&format!("Sync error: {error}")));
                    }
                    MatrixEvent::RoomListUpdated { rooms } => {
                        room_list_view.update_rooms(&rooms);
                    }
                    MatrixEvent::RoomMessages { room_id, messages } => {
                        // Only update if this is still the selected room.
                        let current = window.imp().current_room_id.borrow().clone();
                        if current.as_deref() == Some(&room_id) {
                            let msgs: Vec<(String, String, u64)> = messages
                                .into_iter()
                                .map(|m| (m.sender, m.body, m.timestamp))
                                .collect();
                            message_view.set_messages(&msgs);
                        }
                    }
                    MatrixEvent::NewMessage { room_id, message } => {
                        let current = window.imp().current_room_id.borrow().clone();
                        if current.as_deref() == Some(&room_id) {
                            message_view.append_message(
                                &message.sender,
                                &message.body,
                                message.timestamp,
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
                        window.imp().verify_banner.set_revealed(false);
                        toast_overlay.add_toast(adw::Toast::new("Device verified successfully!"));
                    }
                    MatrixEvent::VerificationCancelled { reason, .. } => {
                        toast_overlay.add_toast(adw::Toast::new(
                            &format!("Verification cancelled: {reason}"),
                        ));
                    }
                    MatrixEvent::DeviceUnverified => {
                        window.imp().verify_banner.set_revealed(true);
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

        // Content: message view.
        let content_toolbar = adw::ToolbarView::new();
        content_toolbar.add_top_bar(&adw::HeaderBar::new());
        content_toolbar.set_content(Some(&imp.message_view));

        let content_page = adw::NavigationPage::builder()
            .title("Matx")
            .child(&content_toolbar)
            .build();
        let _ = imp.content_page.set(content_page.clone());

        let split_view = adw::NavigationSplitView::new();
        split_view.set_sidebar(Some(&sidebar_page));
        split_view.set_content(Some(&content_page));

        // Banner + split view in a vertical box.
        let main_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        main_box.append(&imp.verify_banner);
        main_box.append(&split_view);

        // Wire up the banner's Verify button.
        let tx = imp.command_tx.get().unwrap().clone();
        let banner = imp.verify_banner.clone();
        imp.verify_banner.connect_button_clicked(move |_| {
            banner.set_revealed(false);
            let tx = tx.clone();
            glib::spawn_future_local(async move {
                let _ = tx
                    .send(MatrixCommand::RequestSelfVerification)
                    .await;
            });
        });

        imp.toast_overlay.set_child(Some(&main_box));
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
                let tx = window.imp().command_tx.get().unwrap().clone();
                glib::spawn_future_local(async move {
                    let _ = tx
                        .send(MatrixCommand::RequestSelfVerification)
                        .await;
                });
            })
            .build();

        self.add_action_entries([about_action, preferences_action, verify_action]);
    }

    fn show_about_dialog(&self) {
        let dialog = adw::AboutDialog::builder()
            .application_name(crate::config::APP_NAME)
            .application_icon("mail-send-receive-symbolic")
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
        page.add(&info_group);
        dialog.add(&page);

        // Save when values change.
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
