// MessageRow — a single message bubble in the message view.

mod imp {
    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use gtk::CompositeTemplate;

    #[derive(CompositeTemplate, Default)]
    #[template(file = "src/widgets/message_row.blp")]
    pub struct MessageRow {
        /// Current event ID — updated on each bind for action buttons.
        /// Uses Rc<RefCell> so closures in constructed() share the same cell.
        pub event_id: std::rc::Rc<std::cell::RefCell<String>>,
        pub sender_text: std::rc::Rc<std::cell::RefCell<String>>,
        pub body_text: std::rc::Rc<std::cell::RefCell<String>>,
        /// Callback: reply clicked → (event_id, sender, body).
        pub on_reply: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(String, String, String)>>>>,
        /// Callback: reaction emoji picked → (event_id, emoji).
        pub on_react: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(String, String)>>>>,
        /// Emoji chooser for reactions (created once per row).
        pub react_chooser: gtk::EmojiChooser,
        #[template_child]
        pub sender_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub timestamp_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub body_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub reply_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub reply_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub thread_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub reactions_box: TemplateChild<gtk::Box>,
        /// Floating action bar (not in template — created programmatically).
        pub action_bar: gtk::Box,
        pub reply_button: gtk::Button,
        pub react_button: gtk::Button,
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

    impl ObjectImpl for MessageRow {
        fn constructed(&self) {
            self.parent_constructed();

            // Build floating action bar — appears on hover without
            // affecting layout. Uses a Popover for zero-layout-impact.
            self.reply_button.set_icon_name("mail-reply-sender-symbolic");
            self.reply_button.set_tooltip_text(Some("Reply"));
            self.reply_button.add_css_class("flat");
            self.reply_button.add_css_class("circular");

            self.react_button.set_icon_name("face-smile-symbolic");
            self.react_button.set_tooltip_text(Some("React"));
            self.react_button.add_css_class("flat");
            self.react_button.add_css_class("circular");

            self.action_bar.set_orientation(gtk::Orientation::Horizontal);
            self.action_bar.set_spacing(2);
            self.action_bar.append(&self.reply_button);
            self.action_bar.append(&self.react_button);

            // Add action bar inside the vertical content box, below the
            // message body. Uses CSS class toggle for gentle fade on hover.
            self.action_bar.add_css_class("msg-action-bar");
            if let Some(content_box) = self.obj().first_child() {
                if let Some(vbox) = content_box.downcast_ref::<gtk::Box>() {
                    vbox.append(&self.action_bar);
                }
            }

            // Toggle visibility via CSS class on hover.
            let action_bar = self.action_bar.clone();
            let hover = gtk::EventControllerMotion::new();
            let ab_enter = action_bar.clone();
            hover.connect_enter(move |_, _, _| {
                ab_enter.add_css_class("msg-action-bar-visible");
            });
            let ab_leave = action_bar;
            hover.connect_leave(move |_| {
                ab_leave.remove_css_class("msg-action-bar-visible");
            });
            self.obj().add_controller(hover);

            // Reply button — reads current event_id/sender/body from row state.
            let event_id = self.event_id.clone();
            let sender_text = self.sender_text.clone();
            let body_text = self.body_text.clone();
            let on_reply_ref = self.on_reply.clone();
            self.reply_button.connect_clicked(move |_| {
                let eid = event_id.borrow().clone();
                let sender = sender_text.borrow().clone();
                let body = body_text.borrow().clone();
                // Read callback at call time, not capture time.
                if let Some(ref cb) = *on_reply_ref.borrow() {
                    cb(eid, sender, body);
                }
            });

            // React button — use a MenuButton with the EmojiChooser as popover.
            let react_menu_btn = gtk::MenuButton::builder()
                .icon_name("face-smile-symbolic")
                .tooltip_text("React")
                .popover(&self.react_chooser)
                .build();
            react_menu_btn.add_css_class("flat");
            react_menu_btn.add_css_class("circular");
            // Replace the plain button with the menu button in the action bar.
            self.action_bar.remove(&self.react_button);
            self.action_bar.append(&react_menu_btn);

            let event_id = self.event_id.clone();
            let on_react_ref = self.on_react.clone();
            self.react_chooser.connect_emoji_picked(move |_chooser, emoji| {
                let eid = event_id.borrow().clone();
                if let Some(ref cb) = *on_react_ref.borrow() {
                    cb(eid, emoji.to_string());
                }
            });
        }
    }
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

    let secs_ago = now.duration_since(event_time).unwrap_or_default().as_secs();
    let same_day = dt.year() == today.year()
        && dt.day_of_year() == today.day_of_year();

    // Select format string via computed time range — no if-else chain.
    let fmt = match () {
        _ if same_day => "%H:%M",
        _ if secs_ago < 86400 * 2 => "Yesterday %H:%M",
        _ if dt.year() == today.year() => "%b %e %H:%M",
        _ => "%b %e, %Y %H:%M",
    };
    dt.format(fmt)
        .map(|s: glib::GString| s.to_string())
        .unwrap_or_default()
}

/// Convert URLs in already-escaped markup text into clickable <a> links.
fn linkify_urls(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("http") {
        // Check it's https:// or http://
        let candidate = &rest[start..];
        if !candidate.starts_with("https://") && !candidate.starts_with("http://") {
            result.push_str(&rest[..start + 4]);
            rest = &rest[start + 4..];
            continue;
        }
        result.push_str(&rest[..start]);
        // Find end of URL — stop at whitespace, >, or end of string.
        let url_end = candidate
            .find(|c: char| c.is_whitespace() || c == '<' || c == '>')
            .unwrap_or(candidate.len());
        let url = &candidate[..url_end];
        result.push_str(&format!("<a href=\"{url}\">{url}</a>"));
        rest = &candidate[url_end..];
    }
    result.push_str(rest);
    result
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

    pub fn set_on_reply<F: Fn(String, String, String) + 'static>(&self, f: F) {
        self.imp().on_reply.borrow_mut().replace(Box::new(f));
    }

    pub fn set_on_react<F: Fn(String, String) + 'static>(&self, f: F) {
        self.imp().on_react.borrow_mut().replace(Box::new(f));
    }

    /// Bind a MessageObject to this row — sets all visual elements.
    pub fn bind_message_object(
        &self,
        msg: &crate::models::MessageObject,
        highlight_names: &[String],
    ) {
        let sender = msg.sender();
        let body = msg.body();
        let timestamp = msg.timestamp();
        let reply_to = msg.reply_to();
        let thread_root = msg.thread_root();
        let reactions_json = msg.reactions_json();
        let imp = self.imp();

        // Store current message data for action buttons.
        imp.event_id.replace(msg.event_id());
        imp.sender_text.replace(sender.clone());
        imp.body_text.replace(body.clone());

        // Reply indicator.
        if !reply_to.is_empty() {
            imp.reply_label.set_label(&format!("Reply to message"));
            imp.reply_box.set_visible(true);
        } else {
            imp.reply_box.set_visible(false);
        }

        // Thread indicator.
        imp.thread_icon.set_visible(!thread_root.is_empty());

        // Reactions.
        // Clear old reactions.
        while let Some(child) = imp.reactions_box.first_child() {
            imp.reactions_box.remove(&child);
        }
        if let Ok(reactions) = serde_json::from_str::<Vec<(String, u64)>>(&reactions_json) {
            if !reactions.is_empty() {
                for (emoji, count) in &reactions {
                    let label = if *count > 1 {
                        format!("{emoji} {count}")
                    } else {
                        emoji.clone()
                    };
                    let pill = gtk::Button::builder()
                        .label(&label)
                        .css_classes(["reaction-pill", "flat"])
                        .build();
                    // Click to toggle — use the same on_react callback as
                    // the emoji picker MenuButton.
                    let emoji_str = emoji.clone();
                    let event_id_rc = imp.event_id.clone();
                    let on_react_rc = imp.on_react.clone();
                    pill.connect_clicked(move |_| {
                        let eid = event_id_rc.borrow().clone();
                        tracing::warn!("Pill clicked: emoji={emoji_str} eid={eid}");
                        if let Some(ref cb) = *on_react_rc.borrow() {
                            cb(eid, emoji_str.clone());
                        }
                    });
                    imp.reactions_box.append(&pill);
                }
                imp.reactions_box.set_visible(true);
            } else {
                imp.reactions_box.set_visible(false);
            }
        } else {
            imp.reactions_box.set_visible(false);
        }

        // Delegate to text rendering with highlights.
        self.render_body(&sender, &body, timestamp, highlight_names);
    }

    fn render_body(
        &self,
        sender: &str,
        body: &str,
        timestamp: u64,
        highlight_names: &[String],
    ) {
        let imp = self.imp();
        imp.sender_label.set_label(sender);

        // Check if any highlight name appears in the body.
        let body_lower = body.to_lowercase();
        let has_highlight = highlight_names
            .iter()
            .any(|n| !n.is_empty() && body_lower.contains(&n.to_lowercase()));

        // Escape markup, linkify URLs, then apply highlights.
        let mut escaped = glib::markup_escape_text(body).to_string();

        // Linkify URLs — find http(s):// patterns and wrap in <a> tags.
        escaped = linkify_urls(&escaped);

        if has_highlight {
            self.add_css_class("mention-row");
            for name in highlight_names {
                if name.is_empty() {
                    continue;
                }
                let lower = escaped.to_lowercase();
                let name_lower = name.to_lowercase();
                let mut result = String::new();
                let mut pos = 0;
                while let Some(idx) = lower[pos..].find(&name_lower) {
                    result.push_str(&escaped[pos..pos + idx]);
                    let end = pos + idx + name_lower.len();
                    result.push_str("<b>");
                    result.push_str(&escaped[pos + idx..end]);
                    result.push_str("</b>");
                    pos = end;
                }
                result.push_str(&escaped[pos..]);
                escaped = result;
            }
        } else {
            self.remove_css_class("mention-row");
        }
        imp.body_label.set_markup(&escaped);

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
