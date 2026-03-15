// RoomRow — a single row in the room list sidebar.
//
// Shows a kind icon (person for DMs, hash for rooms), room name,
// and a lock icon for encrypted rooms.

mod imp {
    use gtk::glib;
    use gtk::subclass::prelude::*;
    use gtk::CompositeTemplate;

    #[derive(CompositeTemplate, Default)]
    #[template(file = "src/widgets/room_row.blp")]
    pub struct RoomRow {
        #[template_child]
        pub kind_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub name_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub lock_icon: TemplateChild<gtk::Image>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RoomRow {
        const NAME: &'static str = "MxRoomRow";
        type Type = super::RoomRow;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for RoomRow {}
    impl WidgetImpl for RoomRow {}
    impl BoxImpl for RoomRow {}
}

use adw::prelude::*;
use gtk::glib;
use gtk::subclass::prelude::*;

use crate::models::RoomObject;

glib::wrapper! {
    pub struct RoomRow(ObjectSubclass<imp::RoomRow>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::Orientable;
}

impl RoomRow {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    /// Bind a RoomObject's properties to this row's widgets.
    pub fn bind_room(&self, room: &RoomObject) {
        let imp = self.imp();

        if room.is_header() {
            // Section header: show bold label, hide icons.
            imp.name_label.set_label(&room.name());
            imp.name_label.add_css_class("heading");
            imp.kind_icon.set_visible(false);
            imp.lock_icon.set_visible(false);
        } else {
            // Normal room row.
            imp.name_label.set_label(&room.name());
            imp.name_label.remove_css_class("heading");
            imp.kind_icon.set_visible(true);

            let icon_name = match room.kind().as_str() {
                "dm" => "avatar-default-symbolic",
                _ => "system-users-symbolic",
            };
            imp.kind_icon.set_icon_name(Some(icon_name));

            imp.lock_icon.set_visible(room.is_encrypted());
        }
    }
}
