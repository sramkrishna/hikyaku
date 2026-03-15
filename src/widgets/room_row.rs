// RoomRow — a single row in the room list sidebar.
//
// Shows a kind icon (person for DMs, hash for rooms), room name,
// badges (unread count, admin star, tombstone), and a lock icon
// for encrypted rooms.

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
        pub mention_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub unread_badge: TemplateChild<gtk::Label>,
        #[template_child]
        pub admin_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub tombstone_icon: TemplateChild<gtk::Image>,
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
            // Section header: show bold label, hide everything else.
            imp.name_label.set_label(&room.name());
            imp.name_label.add_css_class("heading");
            imp.kind_icon.set_visible(false);
            imp.lock_icon.set_visible(false);
            imp.mention_icon.set_visible(false);
            imp.unread_badge.set_visible(false);
            imp.admin_icon.set_visible(false);
            imp.tombstone_icon.set_visible(false);
        } else {
            // Normal room row.
            imp.name_label.set_label(&room.name());
            imp.name_label.remove_css_class("heading");
            imp.kind_icon.set_visible(true);

            use std::sync::LazyLock;
            static KIND_ICONS: LazyLock<std::collections::HashMap<&'static str, &'static str>> =
                LazyLock::new(|| {
                    [
                        ("dm", "avatar-default-symbolic"),
                        ("room", "system-users-symbolic"),
                        ("space", "view-grid-symbolic"),
                    ]
                    .into_iter()
                    .collect()
                });
            let icon_name = KIND_ICONS
                .get(room.kind().as_str())
                .unwrap_or(&"system-users-symbolic");
            imp.kind_icon.set_icon_name(Some(icon_name));

            imp.lock_icon.set_visible(room.is_encrypted());

            // Mention icon — show @ when you've been mentioned.
            let highlights = room.highlight_count();
            imp.mention_icon.set_visible(highlights > 0);

            // Unread badge — show count.
            let unread = room.unread_count();
            if unread > 0 || highlights > 0 {
                let count = if highlights > 0 { highlights } else { unread };
                let label = if count > 99 { "99+".to_string() } else { count.to_string() };
                imp.unread_badge.set_label(&label);
                imp.unread_badge.set_visible(true);
                if highlights > 0 {
                    imp.unread_badge.add_css_class("highlight-badge");
                    imp.unread_badge.remove_css_class("unread-badge");
                } else {
                    imp.unread_badge.add_css_class("unread-badge");
                    imp.unread_badge.remove_css_class("highlight-badge");
                }
            } else {
                imp.unread_badge.set_visible(false);
            }

            // Admin star.
            imp.admin_icon.set_visible(room.is_admin());

            // Tombstone indicator.
            imp.tombstone_icon.set_visible(room.is_tombstoned());

            // Dim tombstoned room names.
            if room.is_tombstoned() {
                imp.name_label.add_css_class("dim-label");
            } else {
                imp.name_label.remove_css_class("dim-label");
            }
        }
    }
}
