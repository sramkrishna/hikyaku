use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, CompositeTemplate};

mod imp {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(CompositeTemplate)]
    #[template(file = "src/widgets/onboarding_page.blp")]
    pub struct OnboardingPage {
        #[template_child]
        pub get_started_button: TemplateChild<gtk::Button>,

        pub on_get_started: Rc<RefCell<Option<Box<dyn Fn()>>>>,
    }

    impl Default for OnboardingPage {
        fn default() -> Self {
            Self {
                get_started_button: TemplateChild::default(),
                on_get_started: Rc::new(RefCell::new(None)),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for OnboardingPage {
        const NAME: &'static str = "MxOnboardingPage";
        type Type = super::OnboardingPage;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for OnboardingPage {
        fn constructed(&self) {
            self.parent_constructed();
            let on_get_started = self.on_get_started.clone();
            self.get_started_button.connect_clicked(move |_| {
                if let Some(ref cb) = *on_get_started.borrow() {
                    cb();
                }
            });
        }
    }

    impl WidgetImpl for OnboardingPage {}
    impl BoxImpl for OnboardingPage {}
}

glib::wrapper! {
    pub struct OnboardingPage(ObjectSubclass<imp::OnboardingPage>)
        @extends gtk::Box, gtk::Widget;
}

impl OnboardingPage {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn connect_get_started<F: Fn() + 'static>(&self, f: F) {
        *self.imp().on_get_started.borrow_mut() = Some(Box::new(f));
    }

}
