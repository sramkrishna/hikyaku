// LoginPage — multi-step onboarding flow.
//
// Navigation pages:
//   welcome        — app icon, tagline, Sign In / Create Account buttons
//   sign-in        — homeserver + username + password form
//   create-account — choose a homeserver (browser links) + "Register directly" button
//   register-form  — in-app registration form
//   recovery-key   — show the generated recovery key for new accounts
//   local-spaces   — public spaces from the user's homeserver (live query)
//   more-spaces    — curated cross-server spaces to join
//   ai-setup       — opt-in AI assistant with model picker
//   get-started    — final screen with Matrix resources links
//   verify-device  — offer device verification options after login

mod imp {
    use adw::prelude::*;

    // In debug builds default to the local Conduit instance so we can test the
    // wizard without touching any real homeserver.  Release builds default to
    // matrix.org as a familiar starting point the user can change.
    #[cfg(debug_assertions)]
    const DEFAULT_HOMESERVER: &str = "http://127.0.0.1:6167";
    #[cfg(not(debug_assertions))]
    const DEFAULT_HOMESERVER: &str = "matrix.org";
    use gtk::glib;
    use gtk::subclass::prelude::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    // Preset homeserver list — shown in ComboRow on both sign-in and register forms.
    // Index 0 is matrix.org (the default). The final "Other…" entry reveals a free-text row.
    const PRESET_SERVERS: &[&str] = &["matrix.org", "Other…"];

    pub struct LoginPage {
        pub nav_view: adw::NavigationView,
        // Sign-in form fields.
        pub homeserver_combo: adw::ComboRow,
        pub homeserver_custom_row: adw::EntryRow,
        pub username_row: adw::EntryRow,
        pub password_row: adw::PasswordEntryRow,
        pub login_button: gtk::Button,
        pub login_spinner: gtk::Spinner,
        pub on_login: Rc<RefCell<Option<Box<dyn Fn(String, String, String)>>>>,

        // Register form fields.
        pub register_hs_combo: adw::ComboRow,
        pub register_hs_custom_row: adw::EntryRow,
        pub register_username_row: adw::EntryRow,
        pub register_password_row: adw::PasswordEntryRow,
        pub register_confirm_row: adw::PasswordEntryRow,
        pub register_display_name_row: adw::EntryRow,
        pub register_email_row: adw::EntryRow,
        pub register_button: gtk::Button,
        pub register_spinner: gtk::Spinner,
        pub register_error: gtk::Label,
        pub on_register: Rc<RefCell<Option<Box<dyn Fn(String, String, String, String, String)>>>>,

        // Recovery key page fields.
        pub recovery_key_label: gtk::Label,
        pub recovery_key_continue_btn: gtk::Button,
        pub on_save_recovery_key: Rc<RefCell<Option<Box<dyn Fn(String)>>>>,
        pub on_recovery_key_confirmed: Rc<RefCell<Option<Box<dyn Fn()>>>>,

        // local-spaces page — spinner + dynamic list container.
        pub local_spaces_spinner: gtk::Spinner,
        pub local_spaces_status_label: gtk::Label,
        pub local_spaces_list: adw::PreferencesGroup,
        // Stores (room_id, CheckButton) for local-spaces rows added dynamically.
        pub local_space_checks: Rc<RefCell<Vec<(String, gtk::CheckButton)>>>,

        // Join-rooms callback — fired for both local-spaces and more-spaces.
        pub on_join_rooms: Rc<RefCell<Option<Box<dyn Fn(Vec<String>)>>>>,

        // AI setup.
        pub on_ai_setup: Rc<RefCell<Option<Box<dyn Fn(bool, String)>>>>,
        pub ai_selected_model: Rc<RefCell<String>>,

        // Resources / get-started page — called when the user clicks "Start Chatting".
        pub on_finish: Rc<RefCell<Option<Box<dyn Fn()>>>>,

        // Verification page callbacks.
        pub on_verify_with_device: Rc<RefCell<Option<Box<dyn Fn()>>>>,
        pub on_skip_verification: Rc<RefCell<Option<Box<dyn Fn()>>>>,

        // Recover-key-entry page.
        pub recover_key_entry_row: adw::PasswordEntryRow,
        pub on_recover_with_key: Rc<RefCell<Option<Box<dyn Fn(String)>>>>,
    }

    impl Default for LoginPage {
        fn default() -> Self {
            let register_error = gtk::Label::builder()
                .wrap(true)
                .halign(gtk::Align::Center)
                .visible(false)
                .css_classes(["error"])
                .build();
            let recovery_key_continue_btn = gtk::Button::builder()
                .label("Continue")
                .css_classes(["suggested-action", "pill"])
                .sensitive(false)
                .build();
            let local_spaces_spinner = gtk::Spinner::builder()
                .spinning(true)
                .halign(gtk::Align::Center)
                .build();
            let local_spaces_status_label = gtk::Label::builder()
                .label("Loading public spaces…")
                .halign(gtk::Align::Center)
                .css_classes(["body", "dim-label"])
                .build();
            let local_spaces_list = adw::PreferencesGroup::builder()
                .title("Public Spaces")
                .description("Select spaces to join on your homeserver")
                .visible(false)
                .build();
            // Build the homeserver combo (shared shape for sign-in and register).
            let hs_model = gtk::StringList::new(PRESET_SERVERS);
            let homeserver_combo = adw::ComboRow::builder()
                .title("Homeserver")
                .model(&hs_model)
                .build();
            // In debug builds default to "Other…" and pre-fill the custom URL.
            #[cfg(debug_assertions)]
            homeserver_combo.set_selected(1);
            let homeserver_custom_row = adw::EntryRow::builder()
                .title("Custom homeserver URL")
                .text(DEFAULT_HOMESERVER)
                .visible(cfg!(debug_assertions))
                .build();

            let rhs_model = gtk::StringList::new(PRESET_SERVERS);
            let register_hs_combo = adw::ComboRow::builder()
                .title("Homeserver")
                .model(&rhs_model)
                .build();
            #[cfg(debug_assertions)]
            register_hs_combo.set_selected(1);
            let register_hs_custom_row = adw::EntryRow::builder()
                .title("Custom homeserver URL")
                .text(DEFAULT_HOMESERVER)
                .visible(cfg!(debug_assertions))
                .build();

            Self {
                nav_view: adw::NavigationView::new(),
                homeserver_combo,
                homeserver_custom_row,
                username_row: adw::EntryRow::builder()
                    .title("Username")
                    .build(),
                password_row: adw::PasswordEntryRow::builder()
                    .title("Password")
                    .build(),
                login_button: gtk::Button::builder()
                    .label("Sign In")
                    .css_classes(["suggested-action", "pill"])
                    .build(),
                login_spinner: gtk::Spinner::new(),
                on_login: Rc::new(RefCell::new(None)),

                register_hs_combo,
                register_hs_custom_row,
                register_username_row: adw::EntryRow::builder()
                    .title("Username")
                    .build(),
                register_password_row: adw::PasswordEntryRow::builder()
                    .title("Password")
                    .build(),
                register_confirm_row: adw::PasswordEntryRow::builder()
                    .title("Confirm Password")
                    .build(),
                register_display_name_row: adw::EntryRow::builder()
                    .title("Display Name (optional)")
                    .build(),
                register_email_row: adw::EntryRow::builder()
                    .title("Email (optional)")
                    .build(),
                register_button: gtk::Button::builder()
                    .label("Create Account")
                    .css_classes(["suggested-action", "pill"])
                    .build(),
                register_spinner: gtk::Spinner::new(),
                register_error,

                on_register: Rc::new(RefCell::new(None)),

                recovery_key_label: gtk::Label::builder()
                    .selectable(true)
                    .wrap(true)
                    .halign(gtk::Align::Center)
                    .justify(gtk::Justification::Center)
                    .css_classes(["monospace", "title-3"])
                    .build(),
                recovery_key_continue_btn,
                on_save_recovery_key: Rc::new(RefCell::new(None)),
                on_recovery_key_confirmed: Rc::new(RefCell::new(None)),

                local_spaces_spinner,
                local_spaces_status_label,
                local_spaces_list,
                local_space_checks: Rc::new(RefCell::new(Vec::new())),

                on_join_rooms: Rc::new(RefCell::new(None)),

                on_ai_setup: Rc::new(RefCell::new(None)),
                ai_selected_model: Rc::new(RefCell::new("qwen2.5:3b".to_string())),

                on_finish: Rc::new(RefCell::new(None)),

                on_verify_with_device: Rc::new(RefCell::new(None)),
                on_skip_verification: Rc::new(RefCell::new(None)),

                recover_key_entry_row: adw::PasswordEntryRow::builder()
                    .title("Recovery Key or Passphrase")
                    .build(),
                on_recover_with_key: Rc::new(RefCell::new(None)),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LoginPage {
        const NAME: &'static str = "MxLoginPage";
        type Type = super::LoginPage;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for LoginPage {
        fn constructed(&self) {
            self.parent_constructed();

            let obj = self.obj();
            obj.set_orientation(gtk::Orientation::Vertical);
            obj.set_vexpand(true);
            obj.set_hexpand(true);
            obj.append(&self.nav_view);
            self.nav_view.set_vexpand(true);
            self.nav_view.set_hexpand(true);

            let welcome = self.build_welcome_page();
            let sign_in = self.build_sign_in_page();
            let create = self.build_create_account_page();
            let register_form = self.build_register_form_page();
            let recovery_key = self.build_recovery_key_page();
            let local_spaces = self.build_local_spaces_page();
            let more_spaces = self.build_more_spaces_page();
            let ai_setup = self.build_ai_setup_page();
            let get_started = self.build_get_started_page();
            let verify_device = self.build_verify_device_page();
            let recover_key_entry = self.build_recover_key_entry_page();

            // Push welcome as the root; add the others to the pool.
            self.nav_view.push(&welcome);
            self.nav_view.add(&sign_in);
            self.nav_view.add(&create);
            self.nav_view.add(&register_form);
            self.nav_view.add(&recovery_key);
            self.nav_view.add(&local_spaces);
            self.nav_view.add(&more_spaces);
            self.nav_view.add(&ai_setup);
            self.nav_view.add(&get_started);
            self.nav_view.add(&verify_device);
            self.nav_view.add(&recover_key_entry);

            // Wire sign-in button callback.
            let hs_combo = self.homeserver_combo.clone();
            let hs_custom = self.homeserver_custom_row.clone();
            let un = self.username_row.clone();
            let pw = self.password_row.clone();
            let spinner = self.login_spinner.clone();
            let on_login = self.on_login.clone();
            self.login_button.connect_clicked(move |_| {
                if let Some(ref cb) = *on_login.borrow() {
                    spinner.set_spinning(true);
                    let hs = selected_homeserver(&hs_combo, &hs_custom);
                    cb(hs, un.text().to_string(), pw.text().to_string());
                }
            });

            // Wire register button callback.
            let hs = self.register_hs_combo.clone();
            let hs_custom_reg = self.register_hs_custom_row.clone();
            let un = self.register_username_row.clone();
            let pw = self.register_password_row.clone();
            let confirm = self.register_confirm_row.clone();
            let dn = self.register_display_name_row.clone();
            let email = self.register_email_row.clone();
            let spinner = self.register_spinner.clone();
            let err_label = self.register_error.clone();
            let on_register = self.on_register.clone();
            self.register_button.connect_clicked(move |_| {
                let password = pw.text().to_string();
                let confirm_pass = confirm.text().to_string();
                if password != confirm_pass {
                    err_label.set_label("Passwords do not match.");
                    err_label.set_visible(true);
                    return;
                }
                err_label.set_visible(false);
                if let Some(ref cb) = *on_register.borrow() {
                    spinner.set_spinning(true);
                    cb(
                        selected_homeserver(&hs, &hs_custom_reg),
                        un.text().to_string(),
                        password,
                        dn.text().to_string(),
                        email.text().to_string(),
                    );
                }
            });

            // Set default widget when realized so Enter submits the form.
            let btn = self.login_button.clone();
            obj.connect_realize(move |widget| {
                if let Some(window) = widget.root()
                    .and_then(|r| r.downcast::<gtk::Window>().ok())
                {
                    window.set_default_widget(Some(&btn));
                }
            });
        }
    }

    fn selected_homeserver(combo: &adw::ComboRow, custom: &adw::EntryRow) -> String {
        if combo.selected() as usize == PRESET_SERVERS.len() - 1 {
            custom.text().to_string()
        } else {
            PRESET_SERVERS[combo.selected() as usize].to_string()
        }
    }

    impl LoginPage {
        // ── Welcome page ────────────────────────────────────────────────────

        fn build_welcome_page(&self) -> adw::NavigationPage {
            let page = adw::NavigationPage::builder()
                .tag("welcome")
                .can_pop(false)
                .build();

            let content = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(0)
                .vexpand(true)
                .valign(gtk::Align::Center)
                .halign(gtk::Align::Center)
                .margin_start(48)
                .margin_end(48)
                .margin_bottom(48)
                .build();
            page.set_child(Some(&content));

            let icon = gtk::Image::builder()
                .icon_name(crate::config::APP_ID)
                .pixel_size(128)
                .margin_bottom(24)
                .build();
            content.append(&icon);

            let title = gtk::Label::builder()
                .label(crate::config::APP_NAME)
                .css_classes(["title-1"])
                .margin_bottom(8)
                .build();
            content.append(&title);

            let tagline = gtk::Label::builder()
                .label("A Matrix client for GNOME")
                .css_classes(["body", "dim-label"])
                .margin_bottom(48)
                .build();
            content.append(&tagline);

            let btn_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(12)
                .halign(gtk::Align::Center)
                .build();
            content.append(&btn_box);

            let get_started_btn = gtk::Button::builder()
                .label("Sign In or Create Account")
                .css_classes(["suggested-action", "pill"])
                .width_request(240)
                .build();
            btn_box.append(&get_started_btn);

            let nav = self.nav_view.clone();
            get_started_btn.connect_clicked(move |_| {
                nav.push_by_tag("sign-in");
            });

            page
        }

        // ── Sign-in page ─────────────────────────────────────────────────────

        fn build_sign_in_page(&self) -> adw::NavigationPage {
            let page = adw::NavigationPage::builder()
                .tag("sign-in")
                .title("Sign In")
                .build();

            let toolbar = adw::ToolbarView::new();
            toolbar.add_top_bar(&adw::HeaderBar::new());
            page.set_child(Some(&toolbar));

            let clamp = adw::Clamp::builder()
                .maximum_size(400)
                .vexpand(true)
                .valign(gtk::Align::Center)
                .margin_start(24)
                .margin_end(24)
                .margin_bottom(48)
                .build();
            toolbar.set_content(Some(&clamp));

            let vbox = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(16)
                .build();
            clamp.set_child(Some(&vbox));

            let group = adw::PreferencesGroup::new();
            group.add(&self.homeserver_combo);
            group.add(&self.homeserver_custom_row);
            group.add(&self.username_row);
            group.add(&self.password_row);
            vbox.append(&group);

            // Show custom URL row only when "Other…" is selected.
            let custom = self.homeserver_custom_row.clone();
            self.homeserver_combo.connect_notify_local(Some("selected"), move |combo, _| {
                custom.set_visible(combo.selected() as usize == PRESET_SERVERS.len() - 1);
            });

            let btn = self.login_button.clone();
            let key_ctrl = gtk::EventControllerKey::new();
            key_ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
            key_ctrl.connect_key_pressed(move |_, key, _, _| {
                if key == gtk::gdk::Key::Return || key == gtk::gdk::Key::KP_Enter {
                    btn.emit_clicked();
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
            self.password_row.add_controller(key_ctrl);

            let btn_row = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(8)
                .halign(gtk::Align::Center)
                .build();
            btn_row.append(&self.login_button);
            btn_row.append(&self.login_spinner);
            vbox.append(&btn_row);

            self.login_button.add_css_class("default");
            self.login_button.set_receives_default(true);

            let create_btn = gtk::Button::builder()
                .label("New to Matrix? Create an Account")
                .css_classes(["flat"])
                .halign(gtk::Align::Center)
                .build();
            let nav = self.nav_view.clone();
            create_btn.connect_clicked(move |_| {
                nav.push_by_tag("create-account");
            });
            vbox.append(&create_btn);

            page
        }

        // ── Create-account page ───────────────────────────────────────────────

        fn build_create_account_page(&self) -> adw::NavigationPage {
            let page = adw::NavigationPage::builder()
                .tag("create-account")
                .title("Create Account")
                .build();

            let toolbar = adw::ToolbarView::new();
            toolbar.add_top_bar(&adw::HeaderBar::new());
            page.set_child(Some(&toolbar));

            let clamp = adw::Clamp::builder()
                .maximum_size(480)
                .vexpand(true)
                .valign(gtk::Align::Center)
                .margin_start(24)
                .margin_end(24)
                .margin_bottom(48)
                .build();
            toolbar.set_content(Some(&clamp));

            let vbox = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(20)
                .build();
            clamp.set_child(Some(&vbox));

            // "Register directly" button at the top.
            let register_btn = gtk::Button::builder()
                .label("Register Directly")
                .css_classes(["suggested-action", "pill"])
                .halign(gtk::Align::Center)
                .build();
            vbox.append(&register_btn);
            let nav = self.nav_view.clone();
            register_btn.connect_clicked(move |_| {
                nav.push_by_tag("register-form");
            });

            // Explanation.
            let desc = gtk::Label::builder()
                .label("Matrix is decentralized — your account lives on a server \
                        of your choice. Pick one below to register, then come back \
                        and sign in.")
                .wrap(true)
                .halign(gtk::Align::Start)
                .css_classes(["body"])
                .build();
            vbox.append(&desc);

            const SERVERS: &[(&str, &str, &str)] = &[
                ("matrix.org",
                 "The largest public server · good for getting started",
                 "https://app.element.io/#/register"),
                ("kde.org",
                 "For KDE contributors",
                 "https://community.kde.org/Matrix"),
                ("gnome.org",
                 "For GNOME contributors (invite-only)",
                 "https://wiki.gnome.org/Initiatives/Matrix"),
                ("libera.chat",
                 "For open source and free software communities",
                 "https://libera.chat/guides/registration"),
            ];

            let group = adw::PreferencesGroup::builder()
                .title("Choose a Server")
                .build();
            vbox.append(&group);

            for (name, desc, url) in SERVERS {
                let row = adw::ActionRow::builder()
                    .title(*name)
                    .subtitle(*desc)
                    .build();
                let link_btn = gtk::Button::builder()
                    .icon_name("adw-external-link-symbolic")
                    .tooltip_text("Open registration page")
                    .valign(gtk::Align::Center)
                    .css_classes(["flat", "circular"])
                    .build();
                let url_str = url.to_string();
                link_btn.connect_clicked(move |btn| {
                    let launcher = gtk::UriLauncher::new(&url_str);
                    let root = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
                    launcher.launch(root.as_ref(), gio::Cancellable::NONE, |_| {});
                });
                row.add_suffix(&link_btn);
                row.set_activatable_widget(Some(&link_btn));
                group.add(&row);
            }

            // "Already have an account?" link back to sign-in.
            let already_btn = gtk::Button::builder()
                .label("Already have an account? Sign In")
                .css_classes(["flat"])
                .halign(gtk::Align::Center)
                .build();
            let nav = self.nav_view.clone();
            already_btn.connect_clicked(move |_| {
                nav.push_by_tag("sign-in");
            });
            vbox.append(&already_btn);

            page
        }

        // ── Register form page ────────────────────────────────────────────────

        fn build_register_form_page(&self) -> adw::NavigationPage {
            let page = adw::NavigationPage::builder()
                .tag("register-form")
                .title("Create Account")
                .build();

            let toolbar = adw::ToolbarView::new();
            toolbar.add_top_bar(&adw::HeaderBar::new());
            page.set_child(Some(&toolbar));

            let clamp = adw::Clamp::builder()
                .maximum_size(400)
                .vexpand(true)
                .valign(gtk::Align::Center)
                .margin_start(24)
                .margin_end(24)
                .margin_bottom(48)
                .build();
            toolbar.set_content(Some(&clamp));

            let vbox = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(16)
                .build();
            clamp.set_child(Some(&vbox));

            let group = adw::PreferencesGroup::new();
            group.add(&self.register_hs_combo);
            group.add(&self.register_hs_custom_row);
            group.add(&self.register_username_row);

            let custom = self.register_hs_custom_row.clone();
            self.register_hs_combo.connect_notify_local(Some("selected"), move |combo, _| {
                custom.set_visible(combo.selected() as usize == PRESET_SERVERS.len() - 1);
            });
            group.add(&self.register_password_row);
            group.add(&self.register_confirm_row);
            group.add(&self.register_display_name_row);
            group.add(&self.register_email_row);
            vbox.append(&group);

            vbox.append(&self.register_error);

            let btn_row = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(8)
                .halign(gtk::Align::Center)
                .build();
            btn_row.append(&self.register_button);
            btn_row.append(&self.register_spinner);
            vbox.append(&btn_row);

            page
        }

        // ── Recovery key page ─────────────────────────────────────────────────

        fn build_recovery_key_page(&self) -> adw::NavigationPage {
            let page = adw::NavigationPage::builder()
                .tag("recovery-key")
                .title("Save Your Recovery Key")
                .can_pop(false)
                .build();

            let toolbar = adw::ToolbarView::new();
            toolbar.add_top_bar(&adw::HeaderBar::new());
            page.set_child(Some(&toolbar));

            let clamp = adw::Clamp::builder()
                .maximum_size(480)
                .vexpand(true)
                .valign(gtk::Align::Center)
                .margin_start(24)
                .margin_end(24)
                .margin_bottom(48)
                .build();
            toolbar.set_content(Some(&clamp));

            let vbox = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(20)
                .build();
            clamp.set_child(Some(&vbox));

            let body = gtk::Label::builder()
                .label("This key is the only way to recover your encrypted messages if you lose \
                        access to all your devices. Store it somewhere safe — a password manager \
                        is ideal.")
                .wrap(true)
                .halign(gtk::Align::Center)
                .justify(gtk::Justification::Center)
                .css_classes(["body"])
                .build();
            vbox.append(&body);

            // Key display box with copy button.
            let key_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(8)
                .halign(gtk::Align::Center)
                .build();
            key_box.append(&self.recovery_key_label);

            let copy_btn = gtk::Button::builder()
                .icon_name("edit-copy-symbolic")
                .tooltip_text("Copy to clipboard")
                .css_classes(["flat", "circular"])
                .valign(gtk::Align::Center)
                .build();
            let key_label_ref = self.recovery_key_label.clone();
            copy_btn.connect_clicked(move |btn| {
                let text = key_label_ref.label().to_string();
                if let Some(display) = btn.display().downcast::<gtk::gdk::Display>().ok() {
                    display.clipboard().set_text(&text);
                }
            });
            key_box.append(&copy_btn);
            vbox.append(&key_box);

            // "Save to Password Manager" button.
            let save_btn = gtk::Button::builder()
                .label("Save to Password Manager")
                .css_classes(["suggested-action", "pill"])
                .halign(gtk::Align::Center)
                .build();
            let key_label_save = self.recovery_key_label.clone();
            let on_save = self.on_save_recovery_key.clone();
            save_btn.connect_clicked(move |_| {
                let key = key_label_save.label().to_string();
                if let Some(ref cb) = *on_save.borrow() {
                    cb(key);
                }
            });
            vbox.append(&save_btn);

            // Checkbox to enable Continue.
            let check = gtk::CheckButton::builder()
                .label("I've saved my key")
                .halign(gtk::Align::Center)
                .build();
            let continue_btn = self.recovery_key_continue_btn.clone();
            check.connect_toggled(move |c| {
                continue_btn.set_sensitive(c.is_active());
            });
            vbox.append(&check);

            // Continue button — navigate to local-spaces and fire callback so
            // window.rs sends the BrowsePublicRooms command.
            let on_confirmed = self.on_recovery_key_confirmed.clone();
            let nav_ref = self.nav_view.clone();
            self.recovery_key_continue_btn.connect_clicked(move |_| {
                nav_ref.push_by_tag("local-spaces");
                if let Some(ref cb) = *on_confirmed.borrow() {
                    cb();
                }
            });
            vbox.append(&self.recovery_key_continue_btn);

            page
        }

        // ── Local-spaces page ─────────────────────────────────────────────────

        fn build_local_spaces_page(&self) -> adw::NavigationPage {
            let page = adw::NavigationPage::builder()
                .tag("local-spaces")
                .title("Spaces on Your Server")
                .can_pop(false)
                .build();

            let toolbar = adw::ToolbarView::new();
            toolbar.add_top_bar(&adw::HeaderBar::new());
            page.set_child(Some(&toolbar));

            let clamp = adw::Clamp::builder()
                .maximum_size(520)
                .vexpand(true)
                .valign(gtk::Align::Fill)
                .margin_start(24)
                .margin_end(24)
                .margin_top(12)
                .margin_bottom(24)
                .build();
            toolbar.set_content(Some(&clamp));

            let scroll = gtk::ScrolledWindow::builder()
                .vexpand(true)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .build();
            clamp.set_child(Some(&scroll));

            let vbox = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(16)
                .margin_top(8)
                .build();
            scroll.set_child(Some(&vbox));

            let subtitle = gtk::Label::builder()
                .label("Join a Space to browse and discover rooms on your homeserver. \
                        You can always join more rooms later.")
                .wrap(true)
                .halign(gtk::Align::Start)
                .css_classes(["body", "dim-label"])
                .build();
            vbox.append(&subtitle);

            // Spinner + status label shown while rooms are loading.
            let spinner_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(8)
                .halign(gtk::Align::Center)
                .margin_top(24)
                .build();
            spinner_box.append(&self.local_spaces_spinner);
            spinner_box.append(&self.local_spaces_status_label);
            vbox.append(&spinner_box);

            // List group — hidden until show_local_spaces() is called.
            vbox.append(&self.local_spaces_list);

            let on_join = self.on_join_rooms.clone();
            let checks_ref = self.local_space_checks.clone();
            let nav_ref = self.nav_view.clone();

            let continue_btn = gtk::Button::builder()
                .label("Continue")
                .css_classes(["suggested-action", "pill"])
                .halign(gtk::Align::Center)
                .margin_top(8)
                .build();
            continue_btn.connect_clicked(move |_| {
                let selected: Vec<String> = checks_ref.borrow().iter()
                    .filter(|(_, btn)| btn.is_active())
                    .map(|(id, _)| id.clone())
                    .collect();
                if let Some(ref cb) = *on_join.borrow() {
                    cb(selected);
                }
                nav_ref.push_by_tag("more-spaces");
            });
            vbox.append(&continue_btn);

            page
        }

        // ── More-spaces page ──────────────────────────────────────────────────

        fn build_more_spaces_page(&self) -> adw::NavigationPage {
            let page = adw::NavigationPage::builder()
                .tag("more-spaces")
                .title("Join Communities")
                .can_pop(false)
                .build();

            let toolbar = adw::ToolbarView::new();
            toolbar.add_top_bar(&adw::HeaderBar::new());
            page.set_child(Some(&toolbar));

            let clamp = adw::Clamp::builder()
                .maximum_size(520)
                .vexpand(true)
                .valign(gtk::Align::Fill)
                .margin_start(24)
                .margin_end(24)
                .margin_top(12)
                .margin_bottom(24)
                .build();
            toolbar.set_content(Some(&clamp));

            let scroll = gtk::ScrolledWindow::builder()
                .vexpand(true)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .build();
            clamp.set_child(Some(&scroll));

            let vbox = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(16)
                .margin_top(8)
                .build();
            scroll.set_child(Some(&vbox));

            let subtitle = gtk::Label::builder()
                .label("These cross-server communities are available to everyone on Matrix. \
                        Select any you'd like to join.")
                .wrap(true)
                .halign(gtk::Align::Start)
                .css_classes(["body", "dim-label"])
                .build();
            vbox.append(&subtitle);

            const SPACES: &[(&str, &str, &str)] = &[
                ("#gnome:gnome.org",
                 "GNOME",
                 "The GNOME community Space — apps, design, infrastructure, and more"),
                ("#fedora-space:fedoraproject.org",
                 "Fedora",
                 "The Fedora Project Space — SIGs, releases, and community"),
                ("#kde:kde.org",
                 "KDE",
                 "The KDE community Space — Plasma, apps, and more"),
                ("#ubuntu:ubuntu.com",
                 "Ubuntu",
                 "The Ubuntu community Space"),
            ];

            let mut checks: Vec<(String, gtk::CheckButton)> = Vec::new();

            let spaces_group = adw::PreferencesGroup::builder()
                .title("Spaces")
                .description("Select the communities you'd like to join")
                .build();
            for (alias, name, desc) in SPACES {
                let (row, btn) = Self::make_check_row(alias, name, desc);
                spaces_group.add(&row);
                checks.push((alias.to_string(), btn));
            }
            vbox.append(&spaces_group);

            let btn_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(8)
                .halign(gtk::Align::Center)
                .margin_top(8)
                .margin_bottom(8)
                .build();

            let join_btn = gtk::Button::builder()
                .label("Join Selected")
                .css_classes(["suggested-action", "pill"])
                .build();
            let skip_btn = gtk::Button::builder()
                .label("Skip")
                .css_classes(["flat"])
                .build();
            btn_box.append(&join_btn);
            btn_box.append(&skip_btn);
            vbox.append(&btn_box);

            let nav_ref = self.nav_view.clone();
            let on_join = self.on_join_rooms.clone();
            let checks_ref = checks.clone();
            join_btn.connect_clicked(move |_| {
                let selected: Vec<String> = checks_ref.iter()
                    .filter(|(_, btn)| btn.is_active())
                    .map(|(alias, _)| alias.clone())
                    .collect();
                if let Some(ref cb) = *on_join.borrow() {
                    cb(selected);
                }
                nav_ref.push_by_tag("ai-setup");
            });

            let nav_ref2 = self.nav_view.clone();
            let on_skip = self.on_join_rooms.clone();
            skip_btn.connect_clicked(move |_| {
                if let Some(ref cb) = *on_skip.borrow() {
                    cb(Vec::new());
                }
                nav_ref2.push_by_tag("ai-setup");
            });

            page
        }

        fn make_check_row(id: &str, name: &str, desc: &str) -> (adw::ActionRow, gtk::CheckButton) {
            let check = gtk::CheckButton::builder()
                .valign(gtk::Align::Center)
                .build();
            let row = adw::ActionRow::builder()
                .title(name)
                .subtitle(desc)
                .activatable_widget(&check)
                .build();
            row.set_widget_name(id);
            row.add_prefix(&check);
            (row, check)
        }

        // ── AI setup page ─────────────────────────────────────────────────────

        fn build_ai_setup_page(&self) -> adw::NavigationPage {
            let page = adw::NavigationPage::builder()
                .tag("ai-setup")
                .title("Help Assistant")
                .can_pop(false)
                .build();

            let toolbar = adw::ToolbarView::new();
            toolbar.add_top_bar(&adw::HeaderBar::new());
            page.set_child(Some(&toolbar));

            let clamp = adw::Clamp::builder()
                .maximum_size(480)
                .vexpand(true)
                .valign(gtk::Align::Center)
                .margin_start(24)
                .margin_end(24)
                .margin_bottom(48)
                .build();
            toolbar.set_content(Some(&clamp));

            let vbox = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(20)
                .build();
            clamp.set_child(Some(&vbox));

            // Detect GPU and pick a suitable default model.
            let gpu = crate::intelligence::gpu_detect::detect_gpu();
            let default_model = crate::intelligence::gpu_detect::suggested_model(gpu.as_ref());
            let gpu_reason = crate::intelligence::gpu_detect::suggestion_reason(gpu.as_ref());
            *self.ai_selected_model.borrow_mut() = default_model.to_string();

            // SwitchRow to enable the help assistant.
            let ai_switch = adw::SwitchRow::builder()
                .title("Enable Help Assistant")
                .subtitle("Answer Matrix questions and summarize room conversations — runs locally, nothing leaves your device")
                .build();

            let cfg = crate::config::settings();
            ai_switch.set_active(cfg.ollama.enabled);

            let ai_group = adw::PreferencesGroup::new();
            ai_group.add(&ai_switch);
            vbox.append(&ai_group);

            // Model picker — only visible when switch is on.
            const MODELS: &[(&str, &str)] = &[
                ("qwen2.5:3b",   "Best balance of accuracy and size — recommended"),
                ("llama3.2:3b",  "Meta's 3B model, good general-purpose chat"),
                ("mistral:7b",   "Larger 7B model, higher accuracy, needs more RAM"),
                ("phi4-mini:3.8b", "Smallest model, lowest RAM usage"),
                ("gemma3:4b",    "Google's open model, 4B variant"),
            ];

            let model_list = gtk::StringList::new(&[]);
            for (id, _) in MODELS {
                model_list.append(id);
            }

            // Find the index of the GPU-suggested default.
            let default_idx = MODELS.iter()
                .position(|(id, _)| *id == default_model)
                .unwrap_or(0) as u32;

            let model_row = adw::ComboRow::builder()
                .title("Model")
                .subtitle(&gpu_reason)
                .model(&model_list)
                .selected(default_idx)
                .build();

            let model_group = adw::PreferencesGroup::builder()
                .title("Model")
                .description("You can change this in Settings later")
                .build();
            model_group.add(&model_row);
            model_group.set_visible(ai_switch.is_active());
            vbox.append(&model_group);

            // Warning label.
            let warning = gtk::Label::builder()
                .label("Enabling the Help Assistant will download Ollama (~50 MB) and \
                        the selected model (1–4 GB).")
                .wrap(true)
                .halign(gtk::Align::Center)
                .justify(gtk::Justification::Center)
                .css_classes(["body", "dim-label"])
                .build();
            vbox.append(&warning);

            // Wire switch → show/hide model group.
            let mg = model_group.clone();
            ai_switch.connect_active_notify(move |sw| {
                mg.set_visible(sw.is_active());
            });

            // Track selected model.
            let selected_model = self.ai_selected_model.clone();
            model_row.connect_selected_notify(move |row| {
                let idx = row.selected() as usize;
                if let Some((id, _)) = MODELS.get(idx) {
                    *selected_model.borrow_mut() = id.to_string();
                }
            });

            // Continue button.
            let continue_btn = gtk::Button::builder()
                .label("Continue")
                .css_classes(["suggested-action", "pill"])
                .halign(gtk::Align::Center)
                .margin_top(8)
                .build();

            let nav_ref = self.nav_view.clone();
            let on_ai = self.on_ai_setup.clone();
            let model_ref = self.ai_selected_model.clone();
            continue_btn.connect_clicked(move |_| {
                let enabled = ai_switch.is_active();
                let model = model_ref.borrow().clone();
                if let Some(ref cb) = *on_ai.borrow() {
                    cb(enabled, model);
                }
                nav_ref.push_by_tag("get-started");
            });
            vbox.append(&continue_btn);

            page
        }

        // ── Get-started page ──────────────────────────────────────────────────

        fn build_get_started_page(&self) -> adw::NavigationPage {
            let page = adw::NavigationPage::builder()
                .tag("get-started")
                .title("You're All Set")
                .can_pop(false)
                .build();

            let toolbar = adw::ToolbarView::new();
            toolbar.add_top_bar(&adw::HeaderBar::new());
            page.set_child(Some(&toolbar));

            let clamp = adw::Clamp::builder()
                .maximum_size(520)
                .vexpand(true)
                .valign(gtk::Align::Fill)
                .margin_start(24)
                .margin_end(24)
                .margin_top(12)
                .margin_bottom(24)
                .build();
            toolbar.set_content(Some(&clamp));

            let scroll = gtk::ScrolledWindow::builder()
                .vexpand(true)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .build();
            clamp.set_child(Some(&scroll));

            let vbox = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(20)
                .margin_top(8)
                .build();
            scroll.set_child(Some(&vbox));

            let intro = gtk::Label::builder()
                .label("A few things worth knowing before you start chatting.")
                .wrap(true)
                .halign(gtk::Align::Start)
                .css_classes(["body", "dim-label"])
                .build();
            vbox.append(&intro);

            // Matrix FAQ.
            let faq_group = adw::PreferencesGroup::builder()
                .title("Learn More")
                .build();
            let faq_row = adw::ActionRow::builder()
                .title("Matrix FAQ")
                .subtitle("Answers to common questions about Matrix")
                .activatable(true)
                .build();
            faq_row.add_suffix(&gtk::Image::builder()
                .icon_name("adw-external-link-symbolic")
                .valign(gtk::Align::Center)
                .build());
            faq_row.connect_activated(|row| {
                let launcher = gtk::UriLauncher::new("https://matrix.org/faq/");
                let root = row.root().and_then(|r| r.downcast::<gtk::Window>().ok());
                launcher.launch(root.as_ref(), gio::Cancellable::NONE, |_| {});
            });
            faq_group.add(&faq_row);
            vbox.append(&faq_group);

            // Mobile clients section.
            let mobile_group = adw::PreferencesGroup::builder()
                .title("Mobile Clients")
                .description("Matrix works on iOS and Android. Here are some good options.")
                .build();

            const CLIENTS: &[(&str, &str, &str)] = &[
                ("Element (iOS & Android)",
                 "The reference Matrix client — available on App Store and Play Store",
                 "https://element.io/download"),
                ("FluffyChat (iOS & Android)",
                 "A friendly, simple Matrix client",
                 "https://fluffychat.im"),
                ("All Matrix Clients",
                 "Browse the full list of Matrix clients at matrix.org",
                 "https://matrix.org/ecosystem/clients/"),
            ];

            for (name, desc, url) in CLIENTS {
                let row = adw::ActionRow::builder()
                    .title(*name)
                    .subtitle(*desc)
                    .activatable(true)
                    .build();
                row.add_suffix(&gtk::Image::builder()
                    .icon_name("adw-external-link-symbolic")
                    .valign(gtk::Align::Center)
                    .build());
                let url_str = url.to_string();
                row.connect_activated(move |row| {
                    let launcher = gtk::UriLauncher::new(&url_str);
                    let root = row.root().and_then(|r| r.downcast::<gtk::Window>().ok());
                    launcher.launch(root.as_ref(), gio::Cancellable::NONE, |_| {});
                });
                mobile_group.add(&row);
            }
            vbox.append(&mobile_group);

            // "Start Chatting" button.
            let start_btn = gtk::Button::builder()
                .label("Start Chatting")
                .css_classes(["suggested-action", "pill"])
                .halign(gtk::Align::Center)
                .margin_top(8)
                .margin_bottom(8)
                .build();
            let on_finish = self.on_finish.clone();
            start_btn.connect_clicked(move |_| {
                if let Some(ref cb) = *on_finish.borrow() {
                    cb();
                }
            });
            vbox.append(&start_btn);

            page
        }

        // ── Verify device page ────────────────────────────────────────────────

        fn build_verify_device_page(&self) -> adw::NavigationPage {
            let page = adw::NavigationPage::builder()
                .tag("verify-device")
                .title("Verify This Device")
                .build();

            let toolbar = adw::ToolbarView::new();
            toolbar.add_top_bar(&adw::HeaderBar::new());
            page.set_child(Some(&toolbar));

            let clamp = adw::Clamp::builder()
                .maximum_size(480)
                .vexpand(true)
                .valign(gtk::Align::Center)
                .margin_start(24)
                .margin_end(24)
                .margin_bottom(48)
                .build();
            toolbar.set_content(Some(&clamp));

            let vbox = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(20)
                .build();
            clamp.set_child(Some(&vbox));

            let subtitle = gtk::Label::builder()
                .label("Verifying this device proves your identity and allows you to read \
                        encrypted messages. Without verification, encrypted rooms will appear blank.")
                .wrap(true)
                .halign(gtk::Align::Center)
                .justify(gtk::Justification::Center)
                .css_classes(["body"])
                .build();
            vbox.append(&subtitle);

            let group = adw::PreferencesGroup::new();

            let device_row = adw::ActionRow::builder()
                .title("Verify with Another Device")
                .subtitle("Open your other Matrix client and confirm")
                .activatable(true)
                .build();
            let device_icon = gtk::Image::builder()
                .icon_name("computer-symbolic")
                .build();
            device_row.add_prefix(&device_icon);
            let on_device = self.on_verify_with_device.clone();
            device_row.connect_activated(move |_| {
                if let Some(ref cb) = *on_device.borrow() {
                    cb();
                }
            });
            group.add(&device_row);

            let key_row = adw::ActionRow::builder()
                .title("Use Recovery Key")
                .subtitle("Enter your recovery key or passphrase")
                .activatable(true)
                .build();
            let key_icon = gtk::Image::builder()
                .icon_name("key-symbolic")
                .build();
            key_row.add_prefix(&key_icon);
            let nav_for_key = self.nav_view.clone();
            key_row.connect_activated(move |_| {
                nav_for_key.push_by_tag("recover-key-entry");
            });
            group.add(&key_row);

            vbox.append(&group);

            let skip_btn = gtk::Button::builder()
                .label("Skip for now")
                .css_classes(["flat"])
                .halign(gtk::Align::Center)
                .build();
            let on_skip = self.on_skip_verification.clone();
            skip_btn.connect_clicked(move |_| {
                if let Some(ref cb) = *on_skip.borrow() {
                    cb();
                }
            });
            vbox.append(&skip_btn);

            page
        }

        // ── Recover-key-entry page ────────────────────────────────────────────

        fn build_recover_key_entry_page(&self) -> adw::NavigationPage {
            let page = adw::NavigationPage::builder()
                .tag("recover-key-entry")
                .title("Enter Recovery Key")
                .build();

            let toolbar = adw::ToolbarView::new();
            toolbar.add_top_bar(&adw::HeaderBar::new());
            page.set_child(Some(&toolbar));

            let clamp = adw::Clamp::builder()
                .maximum_size(480)
                .vexpand(true)
                .valign(gtk::Align::Center)
                .margin_start(24)
                .margin_end(24)
                .margin_bottom(48)
                .build();
            toolbar.set_content(Some(&clamp));

            let vbox = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(20)
                .build();
            clamp.set_child(Some(&vbox));

            let desc = gtk::Label::builder()
                .label("Enter the recovery key you saved when setting up this account. \
                        It looks like: EsTJ QmYs XkFd …")
                .wrap(true)
                .halign(gtk::Align::Center)
                .justify(gtk::Justification::Center)
                .css_classes(["body"])
                .build();
            vbox.append(&desc);

            let group = adw::PreferencesGroup::new();
            group.add(&self.recover_key_entry_row);
            vbox.append(&group);

            let recover_btn = gtk::Button::builder()
                .label("Recover Keys")
                .css_classes(["suggested-action", "pill"])
                .halign(gtk::Align::Center)
                .build();

            let entry = self.recover_key_entry_row.clone();
            let on_recover = self.on_recover_with_key.clone();
            recover_btn.connect_clicked(move |_| {
                let key = entry.text().to_string();
                if key.is_empty() { return; }
                if let Some(ref cb) = *on_recover.borrow() {
                    cb(key);
                }
            });
            vbox.append(&recover_btn);

            page
        }
    }

    impl WidgetImpl for LoginPage {}
    impl BoxImpl for LoginPage {}
}

use adw::prelude::*;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

glib::wrapper! {
    pub struct LoginPage(ObjectSubclass<imp::LoginPage>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::Orientable;
}

impl LoginPage {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    pub fn connect_login_requested<F: Fn(String, String, String) + 'static>(&self, f: F) {
        self.imp().on_login.replace(Some(Box::new(f)));
    }

    pub fn stop_spinner(&self) {
        self.imp().login_spinner.set_spinning(false);
    }

    pub fn set_sensitive(&self, sensitive: bool) {
        let imp = self.imp();
        imp.homeserver_combo.set_sensitive(sensitive);
        imp.homeserver_custom_row.set_sensitive(sensitive);
        imp.username_row.set_sensitive(sensitive);
        imp.password_row.set_sensitive(sensitive);
        imp.login_button.set_sensitive(sensitive);
    }

    // ── Register form ─────────────────────────────────────────────────────────

    pub fn connect_register_requested<F: Fn(String, String, String, String, String) + 'static>(
        &self,
        f: F,
    ) {
        self.imp().on_register.replace(Some(Box::new(f)));
    }

    pub fn stop_register_spinner(&self) {
        self.imp().register_spinner.set_spinning(false);
    }

    pub fn show_register_error(&self, msg: &str) {
        let imp = self.imp();
        imp.register_error.set_label(msg);
        imp.register_error.set_visible(true);
    }

    // ── Recovery key page ─────────────────────────────────────────────────────

    pub fn show_recovery_key(&self, key: &str) {
        let imp = self.imp();
        imp.recovery_key_label.set_label(key);
        imp.nav_view.push_by_tag("recovery-key");
    }

    pub fn connect_save_recovery_key<F: Fn(String) + 'static>(&self, f: F) {
        self.imp().on_save_recovery_key.replace(Some(Box::new(f)));
    }

    pub fn connect_recovery_key_confirmed<F: Fn() + 'static>(&self, f: F) {
        self.imp().on_recovery_key_confirmed.replace(Some(Box::new(f)));
    }

    // ── Local-spaces page ─────────────────────────────────────────────────────

    /// Populate the local-spaces list with rooms from the homeserver.
    /// Called by window.rs when MatrixEvent::PublicRoomDirectory arrives.
    /// `rooms` is a list of (room_id, name, topic) tuples.
    pub fn show_local_spaces(&self, rooms: Vec<(String, String, String)>) {
        let imp = self.imp();
        // Hide spinner.
        imp.local_spaces_spinner.set_spinning(false);
        imp.local_spaces_spinner.set_visible(false);
        imp.local_spaces_status_label.set_visible(false);

        let mut checks = imp.local_space_checks.borrow_mut();
        checks.clear();

        if rooms.is_empty() {
            let empty_label = gtk::Label::builder()
                .label("No public spaces found on this server")
                .halign(gtk::Align::Center)
                .css_classes(["body", "dim-label"])
                .build();
            imp.local_spaces_list.add(&empty_label);
        } else {
            for (room_id, name, topic) in rooms {
                let check = gtk::CheckButton::builder()
                    .valign(gtk::Align::Center)
                    .build();
                let row = adw::ActionRow::builder()
                    .title(&name)
                    .subtitle(&topic)
                    .activatable_widget(&check)
                    .build();
                row.add_prefix(&check);
                imp.local_spaces_list.add(&row);
                checks.push((room_id, check));
            }
        }

        imp.local_spaces_list.set_visible(true);
    }

    // ── Join rooms ────────────────────────────────────────────────────────────

    /// Fired for both local-spaces and more-spaces confirmation.
    pub fn connect_join_rooms<F: Fn(Vec<String>) + 'static>(&self, f: F) {
        self.imp().on_join_rooms.replace(Some(Box::new(f)));
    }

    // ── AI setup ──────────────────────────────────────────────────────────────

    /// Fires with (enabled, model_id) when ai-setup Continue is clicked.
    pub fn connect_ai_setup<F: Fn(bool, String) + 'static>(&self, f: F) {
        self.imp().on_ai_setup.replace(Some(Box::new(f)));
    }

    // ── Get-started / finish ──────────────────────────────────────────────────

    pub fn connect_finish<F: Fn() + 'static>(&self, f: F) {
        self.imp().on_finish.replace(Some(Box::new(f)));
    }

    // ── Verify device page ────────────────────────────────────────────────────

    pub fn show_verify_device(&self) {
        self.imp().nav_view.push_by_tag("verify-device");
    }

    pub fn connect_verify_with_device<F: Fn() + 'static>(&self, f: F) {
        self.imp().on_verify_with_device.replace(Some(Box::new(f)));
    }

    pub fn connect_recover_with_key<F: Fn(String) + 'static>(&self, f: F) {
        self.imp().on_recover_with_key.replace(Some(Box::new(f)));
    }

    pub fn connect_skip_verification<F: Fn() + 'static>(&self, f: F) {
        self.imp().on_skip_verification.replace(Some(Box::new(f)));
    }
}
