// LoginPage — a simple form with homeserver, username, and password fields.
//
// This is a GObject widget subclass. It uses a callback (closure) pattern
// to notify the parent when the user clicks "Log in", rather than GObject
// signals — simpler for our purposes.

mod imp {
    use adw::prelude::*;
    use gtk::glib;
    use gtk::subclass::prelude::*;
    use std::cell::RefCell;

    pub struct LoginPage {
        pub login_button: gtk::Button,
        pub spinner: gtk::Spinner,
        pub on_login: RefCell<Option<Box<dyn Fn(String, String, String)>>>,
    }

    impl Default for LoginPage {
        fn default() -> Self {
            Self {
                login_button: gtk::Button::builder()
                    .label("Log in")
                    .css_classes(["suggested-action", "pill"])
                    .build(),
                spinner: gtk::Spinner::new(),
                on_login: RefCell::new(None),
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
            obj.set_spacing(0);
            obj.set_valign(gtk::Align::Center);
            obj.set_halign(gtk::Align::Center);
            obj.set_width_request(320);
            obj.set_margin_start(24);
            obj.set_margin_end(24);

            let title = gtk::Label::builder()
                .label("Matx")
                .css_classes(["title-1"])
                .margin_bottom(8)
                .build();

            let subtitle = gtk::Label::builder()
                .label("Sign in to Matrix")
                .css_classes(["dim-label"])
                .margin_bottom(24)
                .build();

            let group = adw::PreferencesGroup::new();
            let homeserver_row = adw::EntryRow::builder()
                .title("Homeserver")
                .text("matrix.org")
                .build();
            let username_row = adw::EntryRow::builder()
                .title("Username")
                .build();
            let password_row = adw::PasswordEntryRow::builder()
                .title("Password")
                .activates_default(true)
                .build();

            group.add(&homeserver_row);
            group.add(&username_row);
            group.add(&password_row);

            let button_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(8)
                .margin_top(24)
                .halign(gtk::Align::Center)
                .build();
            button_box.append(&self.login_button);
            button_box.append(&self.spinner);

            obj.append(&title);
            obj.append(&subtitle);
            obj.append(&group);
            obj.append(&button_box);

            let hs = homeserver_row.clone();
            let un = username_row.clone();
            let pw = password_row.clone();
            let page = obj.clone();
            let spinner = self.spinner.clone();
            let login_button = self.login_button.clone();
            self.login_button.connect_clicked(move |_btn| {
                let imp = page.imp();
                if let Some(ref cb) = *imp.on_login.borrow() {
                    spinner.set_spinning(true);
                    cb(
                        hs.text().to_string(),
                        un.text().to_string(),
                        pw.text().to_string(),
                    );
                }
            });

            // Mark the login button as the default widget so that
            // pressing Enter in AdwPasswordEntryRow activates it.
            login_button.add_css_class("default");
            login_button.set_receives_default(true);

            // Once the window is available, set it as the default widget.
            let btn = login_button.clone();
            obj.connect_realize(move |widget| {
                if let Some(root) = widget.root() {
                    if let Some(window) = root.downcast_ref::<gtk::Window>() {
                        window.set_default_widget(Some(&btn));
                    }
                }
            });
        }
    }

    impl WidgetImpl for LoginPage {}
    impl BoxImpl for LoginPage {}
}

use gtk::glib;
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
        self.imp().spinner.set_spinning(false);
    }
}
