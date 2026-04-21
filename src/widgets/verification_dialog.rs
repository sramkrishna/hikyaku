// VerificationDialog — shows the SAS emoji verification flow.
//
// Displays a waiting dialog after clicking Verify, then transitions
// to showing 7 emojis for the user to compare with their other device.

use adw::prelude::*;
use gtk::glib;
use async_channel::Sender;
use crate::matrix::MatrixCommand;

/// Normalize a recovery key or passphrase before sending to the SDK.
///
/// Recovery keys are base58-encoded words separated by spaces or dashes
/// (e.g. "EsAC xxxx xxxx" or "EsAC-xxxx-xxxx").  The base58 alphabet
/// excludes I/O/l/0; scanners and some fonts confuse these with 1/0/1/0.
/// Passphrases (which may contain punctuation) are returned as-is after
/// trimming and whitespace-collapse.
pub fn normalize_recovery_key(raw: &str) -> String {
    let raw = raw.trim();
    // Collapse whitespace and convert dashes to spaces so key formats like
    // "EsAC-xxxx-xxxx" and "EsAC xxxx xxxx" are treated identically.
    let normalized: String = raw
        .chars()
        .map(|c| if c == '-' { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    // Only apply base58 lookalike fixes when every character is
    // alphanumeric or a space — this distinguishes keys from passphrases.
    if normalized.chars().all(|c| c == ' ' || c.is_ascii_alphanumeric()) {
        normalized.replace('I', "1").replace('O', "0").replace('l', "1")
    } else {
        normalized
    }
}

/// Show a "waiting for other device" dialog after the user clicks Verify.
/// Returns the dialog so we can dismiss it when emojis arrive.
pub fn show_waiting_dialog(
    parent: &impl IsA<gtk::Widget>,
    _command_tx: Sender<MatrixCommand>,
) -> adw::AlertDialog {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .margin_top(16)
        .margin_bottom(16)
        .halign(gtk::Align::Center)
        .build();

    let spinner = gtk::Spinner::builder()
        .spinning(true)
        .width_request(32)
        .height_request(32)
        .halign(gtk::Align::Center)
        .build();

    let label = gtk::Label::builder()
        .label("Open your other Matrix client (e.g. Element) and accept the verification request.")
        .wrap(true)
        .halign(gtk::Align::Center)
        .build();

    content.append(&spinner);
    content.append(&label);

    let dialog = adw::AlertDialog::builder()
        .heading("Waiting for Other Device")
        .extra_child(&content)
        .build();

    dialog.add_response("cancel", "Cancel");
    dialog.set_response_appearance("cancel", adw::ResponseAppearance::Destructive);

    dialog.present(Some(parent));
    dialog
}

/// Show a dialog to enter a recovery key or passphrase for decrypting old messages.
pub fn show_recovery_key_dialog(
    parent: &impl IsA<gtk::Widget>,
    command_tx: Sender<MatrixCommand>,
) {
    let entry = gtk::PasswordEntry::builder()
        .placeholder_text("Recovery key or passphrase")
        .show_peek_icon(true)
        .build();

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(8)
        .build();

    let label = gtk::Label::builder()
        .label("Enter your recovery key or passphrase to decrypt old messages. You can find this in your other Matrix client's security settings.")
        .wrap(true)
        .xalign(0.0)
        .build();

    content.append(&label);
    content.append(&entry);

    let dialog = adw::AlertDialog::builder()
        .heading("Recover Encryption Keys")
        .extra_child(&content)
        .build();

    dialog.add_response("cancel", "Cancel");
    dialog.add_response("recover", "Recover");
    dialog.set_response_appearance("recover", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("recover"));

    let tx = command_tx.clone();
    dialog.connect_response(None, move |_dialog, response| {
        if response == "recover" {
            let raw = entry.text().to_string();
            let raw = raw.trim();
            if !raw.is_empty() {
                let key = normalize_recovery_key(raw);
                let tx = tx.clone();
                glib::spawn_future_local(async move {
                    let _ = tx
                        .send(MatrixCommand::RecoverKeys { recovery_key: key })
                        .await;
                });
            }
        }
    });

    dialog.present(Some(parent));
}

/// Show a dialog asking the user to accept an incoming verification request.
pub fn show_verification_request(
    parent: &impl IsA<gtk::Widget>,
    flow_id: &str,
    other_user: &str,
    other_device: &str,
    command_tx: Sender<MatrixCommand>,
) {
    let dialog = adw::AlertDialog::builder()
        .heading("Verification Request")
        .body(format!(
            "{other_user} ({other_device}) wants to verify this device."
        ))
        .build();

    dialog.add_response("cancel", "Decline");
    dialog.add_response("accept", "Accept");
    dialog.set_response_appearance("accept", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("accept"));

    let fid = flow_id.to_string();
    let tx = command_tx.clone();
    dialog.connect_response(None, move |_dialog, response| {
        let tx = tx.clone();
        let fid = fid.clone();
        if response == "accept" {
            glib::spawn_future_local(async move {
                let _ = tx.send(MatrixCommand::AcceptVerification { flow_id: fid }).await;
            });
        } else {
            glib::spawn_future_local(async move {
                let _ = tx.send(MatrixCommand::CancelVerification { flow_id: fid }).await;
            });
        }
    });

    dialog.present(Some(parent));
}

/// Show the 7 SAS emojis for the user to confirm.
pub fn show_verification_emojis(
    parent: &impl IsA<gtk::Widget>,
    flow_id: &str,
    emojis: &[(String, String)],
    command_tx: Sender<MatrixCommand>,
) {
    let emoji_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .halign(gtk::Align::Center)
        .margin_top(16)
        .margin_bottom(16)
        .build();

    for (symbol, description) in emojis {
        let item = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .build();

        let symbol_label = gtk::Label::builder()
            .label(symbol)
            .css_classes(["title-1"])
            .build();

        let desc_label = gtk::Label::builder()
            .label(description)
            .css_classes(["caption"])
            .build();

        item.append(&symbol_label);
        item.append(&desc_label);
        emoji_box.append(&item);
    }

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .build();

    let instruction = gtk::Label::builder()
        .label("Compare these emojis with your other device:")
        .wrap(true)
        .build();

    content.append(&instruction);
    content.append(&emoji_box);

    let dialog = adw::AlertDialog::builder()
        .heading("Verify Device")
        .extra_child(&content)
        .build();

    dialog.add_response("cancel", "They Don't Match");
    dialog.add_response("confirm", "They Match");
    dialog.set_response_appearance("confirm", adw::ResponseAppearance::Suggested);
    dialog.set_response_appearance("cancel", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("confirm"));

    let fid = flow_id.to_string();
    let tx = command_tx.clone();
    dialog.connect_response(None, move |_dialog, response| {
        let tx = tx.clone();
        let fid = fid.clone();
        if response == "confirm" {
            glib::spawn_future_local(async move {
                let _ = tx.send(MatrixCommand::ConfirmVerification { flow_id: fid }).await;
            });
        } else {
            glib::spawn_future_local(async move {
                let _ = tx.send(MatrixCommand::CancelVerification { flow_id: fid }).await;
            });
        }
    });

    dialog.present(Some(parent));
}
