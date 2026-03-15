// RoomRow — a single row in the room list sidebar.
//
// Shows a kind icon (person for DMs, hash for rooms), room name,
// and a lock icon for encrypted rooms.

mod imp {
    use adw::prelude::*;
    use gtk::glib;
    use gtk::subclass::prelude::*;

    pub struct RoomRow {
        pub kind_icon: gtk::Image,
        pub name_label: gtk::Label,
        pub lock_icon: gtk::Image,
    }

    impl Default for RoomRow {
        fn default() -> Self {
            Self {
                kind_icon: gtk::Image::builder()
                    .icon_name("chat-message-new-symbolic")
                    .pixel_size(20)
                    .build(),
                name_label: gtk::Label::builder()
                    .halign(gtk::Align::Start)
                    .hexpand(true)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .build(),
                lock_icon: gtk::Image::builder()
                    .icon_name("channel-secure-symbolic")
                    .pixel_size(16)
                    .visible(false)
                    .css_classes(["dim-label"])
                    .build(),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RoomRow {
        const NAME: &'static str = "MxRoomRow";
        type Type = super::RoomRow;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for RoomRow {
        fn constructed(&self) {
            self.parent_constructed();

            let obj = self.obj();
            obj.set_orientation(gtk::Orientation::Horizontal);
            obj.set_spacing(8);
            obj.set_margin_top(6);
            obj.set_margin_bottom(6);
            obj.set_margin_start(6);
            obj.set_margin_end(6);

            obj.append(&self.kind_icon);
            obj.append(&self.name_label);
            obj.append(&self.lock_icon);
        }
    }

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
