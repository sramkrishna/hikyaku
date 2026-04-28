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
        pub avatar: TemplateChild<adw::Avatar>,
        #[template_child]
        pub name_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub typing_label: TemplateChild<gtk::Label>,
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
        #[template_child]
        pub motd_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub watch_alert_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub health_dot: TemplateChild<gtk::Box>,
        /// GObject property bindings — auto-disconnect on unbind.
        pub signal_handlers: RefCell<Vec<glib::Binding>>,
        /// Signal handler IDs for `connect_notify_local` that need manual disconnect.
        pub signal_ids: RefCell<Vec<glib::SignalHandlerId>>,
        /// Weak reference to the currently bound room, needed to disconnect signal_ids.
        pub bound_room: RefCell<Option<crate::models::RoomObject>>,
        /// Room ID of the currently bound room.
        pub current_room_id: RefCell<Option<String>>,
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

thread_local! {
    /// Decoded-avatar cache: path → Texture. Avoids synchronous re-decode +
    /// GPU upload on every row rebind (scroll recycles rows, notify::avatar-path
    /// fires on every refresh). Keyed by absolute file path, so when the SDK
    /// rewrites the avatar file, the path changes and the stale entry is never
    /// hit again (it just lingers in memory — bounded by total avatars).
    static AVATAR_TEXTURE_CACHE: std::cell::RefCell<
        std::collections::HashMap<String, gtk::gdk::Texture>
    > = std::cell::RefCell::new(std::collections::HashMap::new());
}

fn load_avatar_texture(path: &str) -> Option<gtk::gdk::Texture> {
    if path.is_empty() {
        return None;
    }
    if let Some(t) = AVATAR_TEXTURE_CACHE.with(|c| c.borrow().get(path).cloned()) {
        return Some(t);
    }
    match gtk::gdk::Texture::from_filename(path) {
        Ok(texture) => {
            AVATAR_TEXTURE_CACHE.with(|c| {
                c.borrow_mut().insert(path.to_string(), texture.clone())
            });
            Some(texture)
        }
        Err(_) => None,
    }
}

impl RoomRow {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    /// Disconnect property bindings and signal handlers from the previous bind.
    pub fn unbind_room(&self) {
        let imp = self.imp();
        for binding in imp.signal_handlers.take() {
            binding.unbind();
        }
        if let Some(room) = imp.bound_room.take() {
            for id in imp.signal_ids.take() {
                room.disconnect(id);
            }
        }
        imp.avatar.set_custom_image(None::<&gtk::gdk::Paintable>);
        self.remove_css_class("active-room-row");
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
            imp.avatar.set_visible(false);
            imp.lock_icon.set_visible(false);
            imp.mention_icon.set_visible(false);
            imp.unread_badge.set_visible(false);
            imp.admin_icon.set_visible(false);
            imp.tombstone_icon.set_visible(false);
            imp.motd_icon.set_visible(false);
            imp.watch_alert_icon.set_visible(false);
            imp.health_dot.set_visible(false);
            return;
        }

        // Normal room row — set static properties.
        *imp.current_room_id.borrow_mut() = Some(room.room_id());
        *imp.bound_room.borrow_mut() = Some(room.clone());
        imp.name_label.set_label(&room.name());
        imp.name_label.remove_css_class("heading");
        imp.avatar.set_visible(true);
        imp.avatar.set_text(Some(&room.name()));

        // Avatar: cache hits set immediately (no file I/O, no decode). Cache
        // misses defer to idle so decode+GPU-upload doesn't block the rebind.
        let path = room.avatar_path();
        if let Some(texture) = AVATAR_TEXTURE_CACHE.with(|c| c.borrow().get(&path).cloned()) {
            imp.avatar.set_custom_image(Some(&texture));
        } else {
            imp.avatar.set_custom_image(None::<&gtk::gdk::Paintable>);
            if !path.is_empty() {
                let row_weak = self.downgrade();
                glib::idle_add_local_once(move || {
                    let Some(row) = row_weak.upgrade() else { return };
                    if let Some(texture) = load_avatar_texture(&path) {
                        row.imp().avatar.set_custom_image(Some(&texture));
                    }
                });
            }
        }

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

        let b4 = room.bind_property("topic-changed", &imp.motd_icon.get(), "visible")
            .sync_create()
            .build();
        let b5 = room.bind_property("is-typing", &imp.typing_label.get(), "visible")
            .sync_create()
            .build();
        let b6 = room.bind_property("watch-alert", &imp.watch_alert_icon.get(), "visible")
            .sync_create()
            .build();

        imp.signal_handlers.borrow_mut().push(b1);
        imp.signal_handlers.borrow_mut().push(b2);
        imp.signal_handlers.borrow_mut().push(b3);
        imp.signal_handlers.borrow_mut().push(b4);
        imp.signal_handlers.borrow_mut().push(b5);
        imp.signal_handlers.borrow_mut().push(b6);

        // Apply initial active state and watch for changes.
        if room.is_active() {
            self.add_css_class("active-room-row");
        }
        let row_weak = self.downgrade();
        let id = room.connect_notify_local(Some("is-active"), move |obj, _| {
            let Some(row) = row_weak.upgrade() else { return };
            if obj.is_active() {
                row.add_css_class("active-room-row");
            } else {
                row.remove_css_class("active-room-row");
            }
        });
        imp.signal_ids.borrow_mut().push(id);

        // React to community health score changes.
        #[cfg(feature = "community-health")]
        {
            let row_weak = self.downgrade();
            let id = room.connect_notify_local(Some("health-alert"), move |obj, _| {
                let Some(row) = row_weak.upgrade() else { return };
                row.set_health(obj.health_alert());
            });
            imp.signal_ids.borrow_mut().push(id);
        }

        // Watch avatar-path: when the tokio thread downloads the avatar and
        // sets the path on the RoomObject, load the texture and display it.
        let row_weak = self.downgrade();
        let id = room.connect_notify_local(Some("avatar-path"), move |obj, _| {
            let Some(row) = row_weak.upgrade() else { return };
            let path = obj.avatar_path();
            if let Some(texture) = load_avatar_texture(&path) {
                row.imp().avatar.set_custom_image(Some(&texture));
            }
        });
        imp.signal_ids.borrow_mut().push(id);
    }

    /// Update the community health dot colour and visibility.
    /// `alert`: 0 = hide, 1 = none (green), 2 = watch (amber), 3 = warning (red).
    #[cfg(feature = "community-health")]
    pub fn set_health(&self, alert: u8) {
        let dot = self.imp().health_dot.get();
        dot.remove_css_class("health-none");
        dot.remove_css_class("health-watch");
        dot.remove_css_class("health-warning");
        dot.set_tooltip_text(Some(health_tooltip(alert)));
        match alert {
            1 => { dot.add_css_class("health-none"); dot.set_visible(true); }
            2 => { dot.add_css_class("health-watch"); dot.set_visible(true); }
            3 => { dot.add_css_class("health-warning"); dot.set_visible(true); }
            _ => { dot.set_visible(false); }
        }
    }
}

/// Tooltip text for the community-health dot. Pure — kept outside
/// `RoomRow` so it can be unit-tested without a GTK harness.
#[cfg(feature = "community-health")]
pub(crate) fn health_tooltip(alert: u8) -> &'static str {
    match alert {
        1 => "Community health: healthy — no tension detected in recent messages",
        2 => "Community health: watch — sentiment elevated",
        3 => "Community health: warning — sustained tension detected",
        _ => "",
    }
}

#[cfg(all(test, feature = "community-health"))]
mod tests {
    use super::health_tooltip;

    #[test]
    fn tooltip_distinguishes_three_alert_levels() {
        let g = health_tooltip(1);
        let a = health_tooltip(2);
        let r = health_tooltip(3);
        assert!(g.contains("healthy"));
        assert!(a.contains("watch") || a.contains("elevated"));
        assert!(r.contains("warning") || r.contains("tension"));
        assert_ne!(g, a);
        assert_ne!(a, r);
    }

    #[test]
    fn tooltip_empty_when_hidden() {
        // alert == 0 hides the dot; empty tooltip prevents stale text
        // from a prior visible state lingering when GTK shows it.
        assert_eq!(health_tooltip(0), "");
    }

    #[test]
    fn tooltip_unknown_alert_falls_back_to_empty() {
        // Future-proofing: unrecognised alert numbers should never
        // panic, just suppress the tooltip.
        assert_eq!(health_tooltip(7), "");
        assert_eq!(health_tooltip(255), "");
    }
}
