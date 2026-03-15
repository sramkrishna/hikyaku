// MessageRow — a single message bubble in the message view.

mod imp {
    use gtk::glib;
    use gtk::subclass::prelude::*;
    use gtk::CompositeTemplate;

    #[derive(CompositeTemplate, Default)]
    #[template(file = "src/widgets/message_row.blp")]
    pub struct MessageRow {
        #[template_child]
        pub sender_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub body_label: TemplateChild<gtk::Label>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageRow {
        const NAME: &'static str = "MxMessageRow";
        type Type = super::MessageRow;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MessageRow {}
    impl WidgetImpl for MessageRow {}
    impl BoxImpl for MessageRow {}
}

use gtk::glib;
use gtk::subclass::prelude::*;

glib::wrapper! {
    pub struct MessageRow(ObjectSubclass<imp::MessageRow>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::Orientable;
}

impl MessageRow {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    pub fn set_message(&self, sender: &str, body: &str) {
        let imp = self.imp();
        imp.sender_label.set_label(sender);
        imp.body_label.set_label(body);
    }
}
