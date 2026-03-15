// MessageRow — a single message bubble in the message view.

mod imp {
    use gtk::glib;
    use gtk::subclass::prelude::*;
    use adw::prelude::*;

    pub struct MessageRow {
        pub sender_label: gtk::Label,
        pub body_label: gtk::Label,
    }

    impl Default for MessageRow {
        fn default() -> Self {
            Self {
                sender_label: gtk::Label::builder()
                    .halign(gtk::Align::Start)
                    .css_classes(["caption-heading"])
                    .build(),
                body_label: gtk::Label::builder()
                    .halign(gtk::Align::Start)
                    .wrap(true)
                    .wrap_mode(gtk::pango::WrapMode::WordChar)
                    .xalign(0.0)
                    .selectable(true)
                    .build(),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageRow {
        const NAME: &'static str = "MxMessageRow";
        type Type = super::MessageRow;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for MessageRow {
        fn constructed(&self) {
            self.parent_constructed();

            let obj = self.obj();
            obj.set_orientation(gtk::Orientation::Vertical);
            obj.set_spacing(2);
            obj.set_margin_top(4);
            obj.set_margin_bottom(4);
            obj.set_margin_start(12);
            obj.set_margin_end(12);

            obj.append(&self.sender_label);
            obj.append(&self.body_label);
        }
    }

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
