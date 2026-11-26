//! The print confirmation as a dialog of the operating system's toolkit.
//!
//! Guarantees:
//!
//! - **Only the confirming button confirms.** Closing the dialog, Escape, the Cancel
//!   button, a dialog that could not be shown and an answer that is not understood are all
//!   [`Answer::Cancel`].
//! - **A key press on a freshly opened dialog does not confirm.** Cancel is the default
//!   button and holds the keyboard focus, so Return and Space answer Cancel. The
//!   confirming button is reached on purpose, with the pointer or with Tab.
//! - **The confirming button does nothing for the first second** ([`GUARD`]), so a key or
//!   a click already on its way when the dialog appears cannot confirm.
//! - **The text is shown as written.** It is never a format string and never markup.
//! - **The buttons are on the screen.** A body too tall for the screen scrolls inside the
//!   dialog (GTK); the dialog does not grow past the screen.
//!
//! On Linux and the BSDs the dialog is a `gtk::MessageDialog` built here, and
//! `tests/confirm_dialog.sh` holds it to the points above under Xvfb. On Windows and macOS
//! it is `rfd`'s message dialog with Cancel as its first button, which both toolkits make
//! the default; closing the dialog there answers `Cancel`, never a button's label. That
//! path is type-checked on every platform and has not been run on Windows or macOS.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use tauri::Manager;

use crate::app::MAIN_WINDOW;
use crate::confirm::{Answer, ConfirmationText};

/// How long after the dialog appears its confirming button stays without effect.
pub const GUARD: Duration = Duration::from_millis(1000);

/// Shows the confirmation and waits for the answer. Blocks; call it from a blocking
/// thread, never from the event loop (the dialog runs on the event loop).
pub fn ask(app: &tauri::AppHandle, text: &ConfirmationText) -> Answer {
    // A dialog whose two buttons read the same cannot be answered knowingly.
    if text.confirm_label == text.cancel_label {
        return Answer::Cancel;
    }
    toolkit(app, text)
}

#[cfg(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
fn toolkit(app: &tauri::AppHandle, text: &ConfirmationText) -> Answer {
    gtk_dialog::ask(app, text)
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
)))]
fn toolkit(app: &tauri::AppHandle, text: &ConfirmationText) -> Answer {
    rfd_dialog::ask(app, text)
}

/// Text for a C string: a NUL would end it early.
fn c_text(text: &str) -> String {
    text.replace('\0', " ")
}

#[cfg(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
mod gtk_dialog {
    use std::cell::Cell;
    use std::rc::Rc;

    use gtk::prelude::*;

    use super::*;

    pub(super) fn ask(app: &tauri::AppHandle, text: &ConfirmationText) -> Answer {
        let (answer, answered) = mpsc::channel();
        let handle = app.clone();
        let text = text.clone();
        // GTK is used on the event loop's thread only.
        if app
            .run_on_main_thread(move || show(&handle, &text, answer))
            .is_err()
        {
            return Answer::Cancel;
        }
        // The sender is dropped without an answer when the dialog is destroyed any other
        // way (the application quits): that is a cancel.
        answered.recv().unwrap_or(Answer::Cancel)
    }

    /// The most height the body may take, in pixels: what the lowest screen leaves after
    /// the title, the heading, the buttons and the edges of the dialog.
    fn body_height() -> i32 {
        const AROUND: i32 = 260;
        let lowest = gtk::gdk::Display::default().and_then(|display| {
            (0..display.n_monitors())
                .filter_map(|at| display.monitor(at))
                .map(|monitor| monitor.workarea().height())
                .min()
        });
        (lowest.unwrap_or(600) - AROUND).clamp(160, 640)
    }

    fn show(app: &tauri::AppHandle, text: &ConfirmationText, answer: mpsc::Sender<Answer>) {
        let parent = app
            .get_webview_window(MAIN_WINDOW)
            .and_then(|window| window.gtk_window().ok());
        // The heading is passed behind "%s" by `MessageDialog::new`, and the body is the
        // text of a label. Neither is read as a format or as markup.
        let dialog = gtk::MessageDialog::new(
            parent.as_ref(),
            gtk::DialogFlags::MODAL,
            gtk::MessageType::Warning,
            gtk::ButtonsType::None,
            &c_text(&text.title),
        );
        dialog.set_title(&c_text(&text.title));
        // The body is a grid in a scrolling area that is never taller than the screen
        // allows, so the buttons under it are always on screen. Each line is a row: its
        // label of the dialog's own words in the first column, the rest beside it, where a
        // long value wraps within its own cell and is never shortened. Text from outside is
        // only ever a value (`confirm::rows`), so however it wraps it cannot reach the
        // label column and pass for a line of the dialog's. A line
        // without a label is one of the dialog's own sentences and spans both columns.
        let body = gtk::Grid::new();
        body.set_column_spacing(12);
        let value_label = |text: &str| {
            let label = gtk::Label::new(Some(&c_text(if text.is_empty() { " " } else { text })));
            label.set_use_markup(false);
            label.set_line_wrap(true);
            label.set_line_wrap_mode(gtk::pango::WrapMode::WordChar);
            label.set_max_width_chars(64);
            label.set_xalign(0.0);
            label.set_yalign(0.0);
            label
        };
        for (row, (label, value)) in (0_i32..).zip(crate::confirm::rows(&text.body)) {
            if let Some(label) = label {
                let name = gtk::Label::new(Some(&c_text(&format!("{label}:"))));
                name.set_use_markup(false);
                name.set_xalign(0.0);
                name.set_yalign(0.0);
                body.attach(&name, 0, row, 1, 1);
                body.attach(&value_label(value), 1, row, 1, 1);
            } else {
                body.attach(&value_label(value), 0, row, 2, 1);
            }
        }
        let scroll = gtk::ScrolledWindow::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_overlay_scrolling(false);
        scroll.set_propagate_natural_width(true);
        scroll.set_propagate_natural_height(true);
        scroll.set_max_content_height(body_height());
        scroll.add(&body);
        if let Some(area) = dialog.message_area().downcast_ref::<gtk::Box>() {
            // The heading in bold, as the toolkit draws it over a second label of its own.
            let bold = gtk::pango::AttrList::new();
            bold.insert(gtk::pango::AttrInt::new_weight(gtk::pango::Weight::Bold));
            for child in area.children() {
                if let Some(heading) = child.downcast_ref::<gtk::Label>() {
                    heading.set_attributes(Some(&bold));
                }
            }
            area.pack_start(&scroll, true, true, 0);
        } else {
            // No area to put it in: the toolkit's own second label, as plain text.
            dialog.set_secondary_use_markup(false);
            dialog.set_secondary_text(Some(&c_text(&text.body)));
        }
        let cancel = dialog.add_button(&c_text(&text.cancel_label), gtk::ResponseType::Cancel);
        let confirm = dialog.add_button(&c_text(&text.confirm_label), gtk::ResponseType::Accept);
        dialog.set_default_response(gtk::ResponseType::Cancel);
        dialog.set_focus(Some(&cancel));

        // The confirming button wakes up one guard after the dialog is on screen.
        confirm.set_sensitive(false);
        let on_screen: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));
        dialog.connect_map({
            let on_screen = Rc::clone(&on_screen);
            let confirm = confirm.clone();
            move |_| {
                if on_screen.get().is_some() {
                    return;
                }
                on_screen.set(Some(Instant::now()));
                let confirm = confirm.clone();
                gtk::glib::timeout_add_local_once(GUARD, move || confirm.set_sensitive(true));
            }
        });

        let done = Cell::new(false);
        dialog.connect_response(move |dialog, response| {
            if done.get() {
                return;
            }
            let answered = if response == gtk::ResponseType::Accept {
                let armed = on_screen
                    .get()
                    .is_some_and(|since| since.elapsed() >= GUARD);
                if !armed {
                    // Too early to be an answer to what the dialog says: it stays open.
                    return;
                }
                Answer::Confirm
            } else {
                Answer::Cancel
            };
            done.set(true);
            // A receiver that is gone has stopped waiting; there is nobody to tell.
            let _ = answer.send(answered);
            dialog.close();
        });
        dialog.show_all();
    }
}

/// The dialog where GTK is not the toolkit (Windows, macOS), drawn by `rfd`. It is compiled
/// on every platform, so that it is type-checked where the tests run.
#[cfg_attr(
    any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    ),
    allow(dead_code)
)]
mod rfd_dialog {
    use super::*;

    /// What an answer of `rfd`'s dialog means when its buttons were given as
    /// `OkCancelCustom(cancel_label, confirm_label)`: a confirmation only for the
    /// confirming button's own label, given no earlier than one guard after the dialog
    /// was shown. A closed dialog answers `Cancel`, which is no button's label.
    pub(super) fn meaning(
        result: &rfd::MessageDialogResult,
        text: &ConfirmationText,
        open_for: Duration,
    ) -> Answer {
        match result {
            rfd::MessageDialogResult::Custom(label)
                if *label == text.confirm_label
                    && text.confirm_label != text.cancel_label
                    && open_for >= GUARD =>
            {
                Answer::Confirm
            }
            _ => Answer::Cancel,
        }
    }

    pub(super) fn ask(app: &tauri::AppHandle, text: &ConfirmationText) -> Answer {
        let (answer, answered) = mpsc::channel();
        let handle = app.clone();
        let shown = text.clone();
        let started = app.run_on_main_thread(move || {
            // Cancel first: the first button is the default one in a task dialog and in an
            // alert, and the one Return answers.
            let mut dialog = rfd::AsyncMessageDialog::new()
                .set_title(c_text(&shown.title))
                .set_description(c_text(&shown.body))
                .set_level(rfd::MessageLevel::Warning)
                .set_buttons(rfd::MessageButtons::OkCancelCustom(
                    c_text(&shown.cancel_label),
                    c_text(&shown.confirm_label),
                ));
            if let Some(window) = handle.get_webview_window(MAIN_WINDOW) {
                dialog = dialog.set_parent(&window);
            }
            let opened = Instant::now();
            let waiting = dialog.show();
            std::thread::spawn(move || {
                let result = tauri::async_runtime::block_on(waiting);
                let _ = answer.send((result, opened.elapsed()));
            });
        });
        if started.is_err() {
            return Answer::Cancel;
        }
        match answered.recv() {
            Ok((result, open_for)) => meaning(&result, text, open_for),
            Err(_) => Answer::Cancel,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::rfd_dialog::meaning;
    use super::*;

    fn text() -> ConfirmationText {
        ConfirmationText {
            title: "Start this print?".to_owned(),
            body: "Cable box, version 1, plate 1".to_owned(),
            confirm_label: "Start print".to_owned(),
            cancel_label: "Cancel".to_owned(),
        }
    }

    #[test]
    fn only_the_confirming_buttons_own_label_after_the_guard_confirms() {
        use rfd::MessageDialogResult as R;
        let late = GUARD + Duration::from_millis(1);
        assert_eq!(
            meaning(&R::Custom("Start print".to_owned()), &text(), late),
            Answer::Confirm
        );
        // Closed, the system's own buttons, the other label, an unknown label.
        for other in [
            R::Cancel,
            R::Ok,
            R::Yes,
            R::No,
            R::Custom("Cancel".to_owned()),
            R::Custom("start print".to_owned()),
            R::Custom(String::new()),
        ] {
            assert_eq!(meaning(&other, &text(), late), Answer::Cancel, "{other:?}");
        }
        // Too early to be an answer to what the dialog says.
        assert_eq!(
            meaning(
                &R::Custom("Start print".to_owned()),
                &text(),
                Duration::from_millis(300)
            ),
            Answer::Cancel
        );
        // Two buttons that read the same.
        let same = ConfirmationText {
            cancel_label: "Start print".to_owned(),
            ..text()
        };
        assert_eq!(
            meaning(&R::Custom("Start print".to_owned()), &same, late),
            Answer::Cancel
        );
    }

    #[test]
    fn text_for_the_toolkit_has_no_nul_in_it() {
        assert_eq!(c_text("a\0b 50% %s%n"), "a b 50% %s%n");
    }
}
