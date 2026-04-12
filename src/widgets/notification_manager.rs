// NotificationManager — centralises in-app banner + desktop notifications.
//
// Policy:
//   window focused + current room  → suppress everything
//   window focused + other room    → in-app banner only
//   window unfocused               → in-app banner + desktop (D-Bus) notification

use std::cell::RefCell;
use glib::subclass::prelude::*;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct NotificationManager {
        pub banner: RefCell<Option<adw::Banner>>,
        pub banner_room_id: RefCell<Option<String>>,
        /// Weak ref to the root window — used to check is_active() and get application().
        pub window: RefCell<Option<glib::WeakRef<gtk::Window>>>,
        pub current_room_id: RefCell<Option<String>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for NotificationManager {
        const NAME: &'static str = "MxNotificationManager";
        type Type = super::NotificationManager;
        type ParentType = glib::Object;
    }
    impl ObjectImpl for NotificationManager {}
}

glib::wrapper! {
    pub struct NotificationManager(ObjectSubclass<imp::NotificationManager>);
}

impl NotificationManager {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Set the banner widget (called once the window imp is built).
    pub fn set_banner(&self, banner: adw::Banner) {
        *self.imp().banner.borrow_mut() = Some(banner);
    }

    /// Set the window weak ref (called once the window GObject exists).
    pub fn set_window(&self, window: glib::WeakRef<gtk::Window>) {
        *self.imp().window.borrow_mut() = Some(window);
    }

    /// Call whenever the user switches rooms (pass `None` to clear).
    pub fn set_current_room(&self, room_id: Option<&str>) {
        *self.imp().current_room_id.borrow_mut() = room_id.map(str::to_string);
    }

    /// Returns the room ID stored in the banner, for the "Jump" button.
    pub fn banner_room_id(&self) -> Option<String> {
        self.imp().banner_room_id.borrow().clone()
    }

    /// Hide the banner.
    pub fn dismiss_banner(&self) {
        if let Some(b) = &*self.imp().banner.borrow() {
            b.set_revealed(false);
        }
    }

    /// Push a notification. Decides what to show based on current UX state.
    pub fn push(
        &self,
        room_id: &str,
        room_name: &str,
        sender: &str,
        body: &str,
        is_dm: bool,
    ) {
        let imp = self.imp();
        use gtk::prelude::GtkWindowExt;
        use gio::prelude::ApplicationExt;
        let window = imp.window.borrow().as_ref().and_then(|w| w.upgrade());
        let window_active = window.as_ref().map(|w| w.is_active()).unwrap_or(false);
        let is_current = imp.current_room_id.borrow().as_deref() == Some(room_id);

        // Suppress everything for the room the user currently has open —
        // whether the window is focused or not. No point notifying for a
        // room you're already in.
        if is_current {
            return;
        }

        let preview_end = body.char_indices().nth(60).map(|(i, _)| i).unwrap_or(body.len());
        let preview = &body[..preview_end];

        // In-app banner — shown for messages in a different room.
        if !is_current {
            if let Some(banner) = &*imp.banner.borrow() {
                let title = if is_dm {
                    format!("{sender}: {preview}")
                } else {
                    format!("{sender} in {room_name}: {preview}")
                };
                banner.set_title(&title);
                banner.set_revealed(true);
                *imp.banner_room_id.borrow_mut() = Some(room_id.to_string());
                let banner2 = banner.clone();
                glib::timeout_add_seconds_local_once(8, move || {
                    banner2.set_revealed(false);
                });
            }
        }

        // Desktop notification — when window is unfocused (user is elsewhere).
        if !window_active {
            // gtk::Window::application() returns gio::Application.
            let app: Option<gtk::Application> = window
                .as_ref()
                .and_then(|w| GtkWindowExt::application(w));
            if let Some(app) = app {
                let title = if is_dm {
                    format!("Message from {sender}")
                } else {
                    format!("Mentioned in {room_name}")
                };
                let body_end = body.char_indices().nth(100).map(|(i, _)| i).unwrap_or(body.len());
                let notif = gio::Notification::new(&title);
                notif.set_body(Some(&format!("{sender}: {}", &body[..body_end])));
                notif.set_priority(gio::NotificationPriority::High);
                notif.set_default_action_and_target_value(
                    "app.open-room",
                    Some(&glib::Variant::from(room_id)),
                );
                app.send_notification(Some(&format!("msg-{room_id}")), &notif);
                tracing::debug!("Desktop notification sent: {title}");
            }
        }
    }
}
