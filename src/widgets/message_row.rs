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
        pub timestamp_label: TemplateChild<gtk::Label>,
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

use adw::prelude::*;
use gtk::glib;
use gtk::subclass::prelude::*;

/// Format a Unix timestamp (seconds) into a human-readable string.
/// Shows "HH:MM" for today, "Yesterday HH:MM", or "Mon DD HH:MM" for older.
pub(crate) fn format_timestamp(ts: u64) -> String {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    let event_time = UNIX_EPOCH + Duration::from_secs(ts);
    let now = SystemTime::now();

    let Ok(dt) = glib::DateTime::from_unix_local(ts as i64) else {
        return String::new();
    };

    let Ok(today) = glib::DateTime::now_local() else {
        return dt.format("%H:%M").map(|s: glib::GString| s.to_string()).unwrap_or_default();
    };

    let same_day = dt.year() == today.year()
        && dt.day_of_year() == today.day_of_year();

    if same_day {
        dt.format("%H:%M")
    } else {
        let secs_ago = now.duration_since(event_time).unwrap_or_default().as_secs();
        if secs_ago < 86400 * 2 {
            dt.format("Yesterday %H:%M")
        } else {
            dt.format("%b %e %H:%M")
        }
    }
    .map(|s: glib::GString| s.to_string())
    .unwrap_or_default()
}

glib::wrapper! {
    pub struct MessageRow(ObjectSubclass<imp::MessageRow>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::Orientable;
}

impl MessageRow {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    pub fn set_message(&self, sender: &str, body: &str, timestamp: u64) {
        let imp = self.imp();
        imp.sender_label.set_label(sender);
        imp.body_label.set_label(body);

        if timestamp > 0 {
            imp.timestamp_label.set_label(&format_timestamp(timestamp));
            imp.timestamp_label.set_visible(true);
        } else {
            imp.timestamp_label.set_visible(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timestamp_zero_epoch() {
        let result = format_timestamp(0);
        // 1970-01-01 — should produce a date format, not crash.
        assert!(!result.is_empty());
        assert!(result.contains(':'), "should contain HH:MM, got: {result}");
    }

    #[test]
    fn test_timestamp_recent_shows_time_only() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // 30 seconds ago — same day.
        let result = format_timestamp(now - 30);
        // Should be "HH:MM" format (5 chars), no date prefix.
        assert!(
            result.len() <= 5,
            "same-day timestamp should be short, got: {result}"
        );
        assert!(!result.contains("Yesterday"));
    }

    #[test]
    fn test_timestamp_yesterday() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // 25 hours ago.
        let result = format_timestamp(now - 25 * 3600);
        // Might say "Yesterday" or a date depending on exact time-of-day,
        // but should not be just "HH:MM".
        assert!(result.len() > 5, "yesterday should have a prefix, got: {result}");
    }

    #[test]
    fn test_timestamp_old_shows_date() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // 10 days ago.
        let result = format_timestamp(now - 10 * 86400);
        assert!(result.len() > 5, "old date should include month/day, got: {result}");
        assert!(!result.contains("Yesterday"));
    }
}
