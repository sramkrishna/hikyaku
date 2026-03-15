// LoginPage — a simple form with homeserver, username, and password fields.

mod imp {
    use adw::prelude::*;
    use gtk::glib;
    use gtk::subclass::prelude::*;
    use gtk::CompositeTemplate;
    use std::cell::RefCell;

    #[derive(CompositeTemplate, Default)]
    #[template(file = "src/widgets/login_page.blp")]
    pub struct LoginPage {
        #[template_child]
        pub homeserver_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub username_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub password_row: TemplateChild<adw::PasswordEntryRow>,
        #[template_child]
        pub login_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub login_spinner: TemplateChild<gtk::Spinner>,
        pub on_login: RefCell<Option<Box<dyn Fn(String, String, String)>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LoginPage {
        const NAME: &'static str = "MxLoginPage";
        type Type = super::LoginPage;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for LoginPage {
        fn constructed(&self) {
            self.parent_constructed();

            let obj = self.obj();

            let hs = self.homeserver_row.clone();
            let un = self.username_row.clone();
            let pw = self.password_row.clone();
            let page = obj.clone();
            let spinner = self.login_spinner.clone();
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

use adw::prelude::*;
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
