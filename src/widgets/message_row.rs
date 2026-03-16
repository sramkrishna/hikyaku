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
        /// Callback: edit clicked → (event_id, body).
        pub on_edit: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(String, String)>>>>,
        /// Callback: delete clicked → (event_id).
        pub on_delete: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(String)>>>>,
        /// Callback: media hover → (mxc_url, filename, callback for file path).
        pub on_media_click: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(String, String, String)>>>>,
        /// Current media URL, filename, and source JSON.
        pub media_url: std::rc::Rc<std::cell::RefCell<String>>,
        pub media_filename: std::rc::Rc<std::cell::RefCell<String>>,
        pub media_source_json: std::rc::Rc<std::cell::RefCell<String>>,
        /// Cached local file path after download.
        pub media_cached_path: std::rc::Rc<std::cell::RefCell<Option<String>>>,
        /// Edit and delete buttons — visibility toggled per message.
        pub edit_button: std::cell::RefCell<Option<gtk::Button>>,
        pub delete_button: std::cell::RefCell<Option<gtk::Button>>,
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
        pub media_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub media_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub media_label: TemplateChild<gtk::Label>,
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

            let edit_button = gtk::Button::builder()
                .icon_name("document-edit-symbolic")
                .tooltip_text("Edit")
                .build();
            edit_button.add_css_class("flat");
            edit_button.add_css_class("circular");

            let delete_button = gtk::Button::builder()
                .icon_name("edit-delete-symbolic")
                .tooltip_text("Delete")
                .build();
            delete_button.add_css_class("flat");
            delete_button.add_css_class("circular");

            self.action_bar.set_orientation(gtk::Orientation::Horizontal);
            self.action_bar.set_spacing(2);
            self.action_bar.append(&self.reply_button);
            self.action_bar.append(&self.react_button);
            self.action_bar.append(&edit_button);
            self.action_bar.append(&delete_button);
            self.edit_button.replace(Some(edit_button.clone()));
            self.delete_button.replace(Some(delete_button.clone()));

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

            // Edit button — enter edit mode with current body.
            let event_id = self.event_id.clone();
            let body_text = self.body_text.clone();
            let on_edit = self.on_edit.clone();
            edit_button.connect_clicked(move |_| {
                let eid = event_id.borrow().clone();
                let body = body_text.borrow().clone();
                if let Some(ref cb) = *on_edit.borrow() {
                    cb(eid, body);
                }
            });

            // Delete button — redact the message.
            let event_id = self.event_id.clone();
            let on_delete = self.on_delete.clone();
            delete_button.connect_clicked(move |_| {
                let eid = event_id.borrow().clone();
                if let Some(ref cb) = *on_delete.borrow() {
                    cb(eid);
                }
            });

            // Reaction pill click — toggle via gesture on the reactions box.
            let event_id = self.event_id.clone();
            let on_react = self.on_react.clone();
            let gesture = gtk::GestureClick::new();
            gesture.connect_released(move |gesture, _, x, y| {
                let Some(widget) = gesture.widget() else { return };
                if let Some(child) = widget.pick(x, y, gtk::PickFlags::DEFAULT) {
                    // Walk up to find a Label (the reaction pill).
                    let mut w: Option<gtk::Widget> = Some(child);
                    while let Some(ref current) = w {
                        if let Ok(label) = current.clone().downcast::<gtk::Label>() {
                            let text = label.text().to_string();
                            let emoji = text.split_whitespace().next()
                                .unwrap_or(&text).to_string();
                            let eid = event_id.borrow().clone();
                            if let Some(ref cb) = *on_react.borrow() {
                                cb(eid, emoji);
                            }
                            break;
                        }
                        w = current.parent();
                    }
                }
            });
            self.reactions_box.add_controller(gesture);

            // Media button click — download and show preview.
            let media_url = self.media_url.clone();
            let media_filename = self.media_filename.clone();
            let media_src = self.media_source_json.clone();
            let on_media = self.on_media_click.clone();
            self.media_button.connect_clicked(move |_| {
                let url = media_url.borrow().clone();
                let filename = media_filename.borrow().clone();
                let source_json = media_src.borrow().clone();
                if !url.is_empty() {
                    if let Some(ref cb) = *on_media.borrow() {
                        cb(url, filename, source_json);
                    }
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

/// Extract an image/gif URL from message body text, if present.
/// Converts Giphy page URLs to direct media URLs.
fn extract_image_url(body: &str) -> Option<String> {
    let body_trimmed = body.trim();
    for word in body_trimmed.split_whitespace() {
        if !(word.starts_with("https://") || word.starts_with("http://")) {
            continue;
        }
        // Strip query params/fragments for extension check.
        let lower = word.to_lowercase();
        let path_part = lower.split('?').next().unwrap_or(&lower);
        let path_part = path_part.split('#').next().unwrap_or(path_part);
        // Any URL ending in an image extension.
        if path_part.ends_with(".gif")
            || path_part.ends_with(".png")
            || path_part.ends_with(".jpg")
            || path_part.ends_with(".jpeg")
            || path_part.ends_with(".webp")
        {
            return Some(word.to_string());
        }
        // Giphy page URLs → convert to direct media URL.
        // https://giphy.com/gifs/NAME-ID → https://media.giphy.com/media/ID/giphy.gif
        if lower.contains("giphy.com/gifs/") {
            if let Some(slug) = word.rsplit('/').next() {
                // The ID is the last part after the last dash, or the whole slug.
                let id = slug.rsplit('-').next().unwrap_or(slug);
                return Some(format!("https://media.giphy.com/media/{id}/giphy.gif"));
            }
        }
        // media.giphy.com URLs are already direct.
        if lower.contains("media.giphy.com") {
            return Some(word.to_string());
        }
        // Tenor media URLs — media.tenor.com serves GIFs/videos directly.
        if lower.contains("media.tenor.com") || lower.contains("c.tenor.com") {
            return Some(word.to_string());
        }
        // Tenor page URLs.
        if lower.contains("tenor.com/view/") {
            return Some(word.to_string());
        }
    }
    None
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

    pub fn set_on_edit<F: Fn(String, String) + 'static>(&self, f: F) {
        self.imp().on_edit.borrow_mut().replace(Box::new(f));
    }

    pub fn set_on_delete<F: Fn(String) + 'static>(&self, f: F) {
        self.imp().on_delete.borrow_mut().replace(Box::new(f));
    }

    pub fn set_on_media_click<F: Fn(String, String, String) + 'static>(&self, f: F) {
        self.imp().on_media_click.borrow_mut().replace(Box::new(f));
    }

    /// Bind a MessageObject to this row — sets all visual elements.
    pub fn bind_message_object(
        &self,
        msg: &crate::models::MessageObject,
        highlight_names: &[String],
        my_user_id: &str,
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

        // Reply indicator — extract original sender from reply fallback.
        if !reply_to.is_empty() {
            // Try to extract the sender from reply fallback format: "> <@user:server>"
            let reply_sender = body.lines()
                .find(|l| l.starts_with("> <@"))
                .and_then(|l| l.strip_prefix("> <"))
                .and_then(|l| l.split('>').next())
                .and_then(|uid| uid.strip_prefix('@'))
                .and_then(|uid| uid.split(':').next())
                .map(|local| format!("Replying to {local}"))
                .unwrap_or_else(|| "Reply to message".to_string());
            imp.reply_label.set_label(&reply_sender);
            imp.reply_box.set_visible(true);
        } else {
            imp.reply_box.set_visible(false);
        }

        // Thread indicator.
        imp.thread_icon.set_visible(!thread_root.is_empty());

        // Show edit/delete only on own messages.
        let msg_sender_id = msg.sender_id();
        let is_own = !my_user_id.is_empty() && msg_sender_id == my_user_id;
        if !msg_sender_id.is_empty() {
            tracing::debug!("Edit check: sender_id='{}' my_id='{}' is_own={}", msg_sender_id, my_user_id, is_own);
        }
        if let Some(ref btn) = *imp.edit_button.borrow() {
            btn.set_visible(is_own);
        }
        if let Some(ref btn) = *imp.delete_button.borrow() {
            btn.set_visible(is_own);
        }

        // Media attachment.
        let media_json = msg.media_json();
        if !media_json.is_empty() {
            if let Ok(media) = serde_json::from_str::<crate::matrix::MediaInfo>(&media_json) {
                use std::sync::LazyLock;
                static MEDIA_ICONS: LazyLock<std::collections::HashMap<&'static str, &'static str>> =
                    LazyLock::new(|| {
                        [
                            ("Image", "image-x-generic-symbolic"),
                            ("Video", "video-x-generic-symbolic"),
                            ("Audio", "audio-x-generic-symbolic"),
                            ("File", "text-x-generic-symbolic"),
                        ].into_iter().collect()
                    });
                let kind_str = match media.kind {
                    crate::matrix::MediaKind::Image => "Image",
                    crate::matrix::MediaKind::Video => "Video",
                    crate::matrix::MediaKind::Audio => "Audio",
                    crate::matrix::MediaKind::File => "File",
                };
                let icon = MEDIA_ICONS.get(kind_str).unwrap_or(&"text-x-generic-symbolic");
                imp.media_icon.set_icon_name(Some(icon));
                let size_str = media.size
                    .map(|s| {
                        if s > 1_048_576 { format!(" ({:.1} MB)", s as f64 / 1_048_576.0) }
                        else if s > 1024 { format!(" ({:.0} KB)", s as f64 / 1024.0) }
                        else { format!(" ({s} B)") }
                    })
                    .unwrap_or_default();
                imp.media_label.set_label(&format!("{}{size_str}", media.filename));
                imp.media_button.set_visible(true);

                imp.media_url.replace(media.url.clone());
                imp.media_filename.replace(media.filename.clone());
                imp.media_source_json.replace(media.source_json.clone());
            } else {
                imp.media_button.set_visible(false);
            }
        } else {
            // Check if body contains an image/gif URL — show as media placeholder.
            if let Some(url) = extract_image_url(&body) {
                imp.media_icon.set_icon_name(Some("image-x-generic-symbolic"));
                let display = if url.contains("giphy.com") {
                    "GIF".to_string()
                } else {
                    url.split('/').last().unwrap_or("image").to_string()
                };
                imp.media_label.set_label(&display);
                imp.media_button.set_visible(true);
                imp.media_url.replace(url.clone());
                imp.media_filename.replace(display);
            } else {
                imp.media_button.set_visible(false);
            }
        }

        // Reactions — use Labels instead of Buttons to avoid closure
        // reference cycles that cause double-free on rebind. Click handling
        // is done via a GestureClick on the reactions_box itself.
        while let Some(child) = imp.reactions_box.first_child() {
            imp.reactions_box.remove(&child);
        }
        if let Ok(reactions) = serde_json::from_str::<Vec<(String, u64, Vec<String>)>>(&reactions_json) {
            if !reactions.is_empty() {
                for (emoji, count, names) in &reactions {
                    let label = if *count > 1 {
                        format!("{emoji} {count}")
                    } else {
                        emoji.clone()
                    };
                    // Tooltip shows who reacted.
                    let tooltip = names.join(", ");
                    let pill = gtk::Label::builder()
                        .label(&label)
                        .tooltip_text(&tooltip)
                        .css_classes(["reaction-pill"])
                        .build();
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
        // Also highlight if the message is marked as a reply-to-us.
        let force_highlight = msg.is_highlight();
        self.render_body(&sender, &body, timestamp, highlight_names, force_highlight);
    }

    fn render_body(
        &self,
        sender: &str,
        body: &str,
        timestamp: u64,
        highlight_names: &[String],
        force_highlight: bool,
    ) {
        let imp = self.imp();
        imp.sender_label.set_label(sender);

        // Check if any highlight name appears in the body,
        // or if this message is flagged as a reply-to-us.
        let body_lower = body.to_lowercase();
        let has_highlight = force_highlight || highlight_names
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
