// LoginPage — multi-step onboarding flow.
//
// Three adw::NavigationPage pages:
//   welcome        — app icon, tagline, Sign In / Create Account buttons
//   sign-in        — homeserver + username + password form
//   create-account — choose a homeserver, open registration in browser

mod imp {
    use adw::prelude::*;
    use gtk::glib;
    use gtk::subclass::prelude::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    pub struct LoginPage {
        pub nav_view: adw::NavigationView,
        // Sign-in form fields (kept as fields for public API access).
        pub homeserver_row: adw::EntryRow,
        pub username_row: adw::EntryRow,
        pub password_row: adw::PasswordEntryRow,
        pub login_button: gtk::Button,
        pub login_spinner: gtk::Spinner,
        pub on_login: Rc<RefCell<Option<Box<dyn Fn(String, String, String)>>>>,
    }

    impl Default for LoginPage {
        fn default() -> Self {
            Self {
                nav_view: adw::NavigationView::new(),
                homeserver_row: adw::EntryRow::builder()
                    .title("Homeserver")
                    .text("matrix.org")
                    .build(),
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

            // Push welcome as the root; add the others to the pool.
            self.nav_view.push(&welcome);
            self.nav_view.add(&sign_in);
            self.nav_view.add(&create);

            // Wire sign-in button callback (shared across on_login).
            let hs = self.homeserver_row.clone();
            let un = self.username_row.clone();
            let pw = self.password_row.clone();
            let spinner = self.login_spinner.clone();
            let on_login = self.on_login.clone();
            self.login_button.connect_clicked(move |_| {
                if let Some(ref cb) = *on_login.borrow() {
                    spinner.set_spinning(true);
                    cb(hs.text().to_string(), un.text().to_string(), pw.text().to_string());
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

    impl LoginPage {
        // ── Welcome page ────────────────────────────────────────────────────

        fn build_welcome_page(&self) -> adw::NavigationPage {
            let page = adw::NavigationPage::builder()
                .tag("welcome")
                .can_pop(false)
                .build();

            // No header bar on the root welcome page.
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

            // App icon.
            let icon = gtk::Image::builder()
                .icon_name(crate::config::APP_ID)
                .pixel_size(128)
                .margin_bottom(24)
                .build();
            content.append(&icon);

            // App name.
            let title = gtk::Label::builder()
                .label(crate::config::APP_NAME)
                .css_classes(["title-1"])
                .margin_bottom(8)
                .build();
            content.append(&title);

            // Tagline.
            let tagline = gtk::Label::builder()
                .label("A Matrix client for GNOME")
                .css_classes(["body", "dim-label"])
                .margin_bottom(48)
                .build();
            content.append(&tagline);

            // Buttons.
            let btn_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(12)
                .halign(gtk::Align::Center)
                .build();
            content.append(&btn_box);

            let sign_in_btn = gtk::Button::builder()
                .label("Sign In")
                .css_classes(["suggested-action", "pill"])
                .width_request(200)
                .build();
            let create_btn = gtk::Button::builder()
                .label("Create Account")
                .css_classes(["pill"])
                .width_request(200)
                .build();
            btn_box.append(&sign_in_btn);
            btn_box.append(&create_btn);

            // Navigate on click.
            let nav = self.nav_view.clone();
            sign_in_btn.connect_clicked(move |_| {
                nav.push_by_tag("sign-in");
            });
            let nav = self.nav_view.clone();
            create_btn.connect_clicked(move |_| {
                nav.push_by_tag("create-account");
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

            // Form.
            let group = adw::PreferencesGroup::new();
            group.add(&self.homeserver_row);
            group.add(&self.username_row);
            group.add(&self.password_row);
            vbox.append(&group);

            // Enter in password row triggers sign-in button.
            // connect_apply is unreliable on AdwPasswordEntryRow across libadwaita
            // versions; an EventControllerKey is guaranteed to fire on Return/KP_Enter.
            let btn = self.login_button.clone();
            let key_ctrl = gtk::EventControllerKey::new();
            // Capture phase: intercept before the inner GtkText consumes the event.
            key_ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
            key_ctrl.connect_key_pressed(move |_, key, _, _| {
                if key == gtk::gdk::Key::Return || key == gtk::gdk::Key::KP_Enter {
                    btn.emit_clicked();
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
            self.password_row.add_controller(key_ctrl);

            // Sign-in button + spinner.
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

            // Homeserver list.
            // (name, description, registration URL)
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
    }

    impl WidgetImpl for LoginPage {}
    impl BoxImpl for LoginPage {}
}

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
        imp.homeserver_row.set_sensitive(sensitive);
        imp.username_row.set_sensitive(sensitive);
        imp.password_row.set_sensitive(sensitive);
        imp.login_button.set_sensitive(sensitive);
    }
}
