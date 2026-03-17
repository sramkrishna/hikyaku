// RoomRow — a single row in the room list sidebar.
//
// Shows a kind icon (person for DMs, hash for rooms), room name,
// badges (unread count, admin star, tombstone), and a lock icon
// for encrypted rooms.

mod imp {
    use gtk::glib;
    use gtk::subclass::prelude::*;
    use gtk::CompositeTemplate;
    use std::cell::RefCell;

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
        /// GObject property bindings — auto-disconnect on unbind.
        pub signal_handlers: RefCell<Vec<glib::Binding>>,
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

    /// Disconnect property bindings from the previous bind.
    pub fn unbind_room(&self) {
        let old = self.imp().signal_handlers.take();
        for binding in old {
            binding.unbind();
        }
    }

    /// Bind a RoomObject's properties to this row's widgets.
    /// Uses GObject property bindings for automatic badge updates.
    pub fn bind_room(&self, room: &RoomObject) {
        let imp = self.imp();

        // Disconnect old bindings.
        let old = imp.signal_handlers.take();
        for binding in old {
            binding.unbind();
        }

        if room.is_header() {
            imp.name_label.set_label(&room.name());
            imp.name_label.add_css_class("heading");
            imp.kind_icon.set_visible(false);
            imp.lock_icon.set_visible(false);
            imp.mention_icon.set_visible(false);
            imp.unread_badge.set_visible(false);
            imp.admin_icon.set_visible(false);
            imp.tombstone_icon.set_visible(false);
            return;
        }

        // Normal room row — set static properties.
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
        imp.admin_icon.set_visible(room.is_admin());
        imp.tombstone_icon.set_visible(room.is_tombstoned());
        if room.is_tombstoned() {
            imp.name_label.add_css_class("dim-label");
        } else {
            imp.name_label.remove_css_class("dim-label");
        }

        // Bind unread_count → badge label + visibility using GObject
        // property bindings. These auto-update and auto-disconnect.
        let badge_widget: gtk::Label = imp.unread_badge.get();
        let mention_widget: gtk::Image = imp.mention_icon.get();
        let b1 = room.bind_property("unread-count", &badge_widget, "visible")
            .transform_to(|_, count: u32| Some(count > 0))
            .sync_create()
            .build();
        let b2 = room.bind_property("unread-count", &badge_widget, "label")
            .transform_to(|_, count: u32| {
                Some(if count > 99 { "99+".to_string() } else { count.to_string() })
            })
            .sync_create()
            .build();
        let b3 = room.bind_property("highlight-count", &mention_widget, "visible")
            .transform_to(|_, count: u32| Some(count > 0))
            .sync_create()
            .build();

        imp.signal_handlers.borrow_mut().push(b1);
        imp.signal_handlers.borrow_mut().push(b2);
        imp.signal_handlers.borrow_mut().push(b3);
    }
}
