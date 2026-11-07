// tab_notify.rs
//
// Copyright 2023 Christian Hergert <christian@sourceandstack.com>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! Telling the user that a command finished in a tab they are not looking
//! at (`src/ptyxis-tab-notify.h`).
//!
//! Shells set up by `vte.sh` report each command through the
//! `vte.shell.preexec` and `vte.shell.precmd` termprops. A command that
//! took long enough flags its tab when the tab is in the background of the
//! focused window, and also sends a desktop notification when the window
//! is not focused. Activating the notification focuses the tab through
//! `app.focus-tab-by-uuid`; focusing the tab withdraws it.

use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::gio;

use crate::app;
use crate::tab;

/// Commands quicker than this are not worth a notification
/// (`TIME_WATERMARK_MSEC`).
pub const TIME_WATERMARK: Duration = Duration::from_millis(250);

/// Title of the desktop notification.
const NOTIFICATION_TITLE: &str = "Command completed";
/// Application action that focuses a tab given its uuid.
pub const FOCUS_TAB_BY_UUID_ACTION: &str = "app.focus-tab-by-uuid";

/// Tracks the command between `shell-preexec` and `shell-precmd`
/// (`PtyxisTabNotify`).
#[derive(Debug, Default)]
pub struct TabNotify {
    /// When the shell last started a command (`command_start_time`).
    command_start: Option<Instant>,
    /// The command line found after it started (`current_cmdline`).
    current_cmdline: Option<String>,
}

impl TabNotify {
    /// The shell is about to run a command
    /// (`ptyxis_tab_notify_shell_preexec_cb()`). The caller then checks the
    /// foreground and hands the command line to [`TabNotify::polled`].
    pub fn preexec(&mut self, now: Instant) {
        self.command_start = Some(now);
        self.current_cmdline = None;
    }

    /// The foreground check after [`TabNotify::preexec`] found `cmdline`
    /// (`ptyxis_tab_notify_shell_preexec_poll_cb()`).
    pub fn polled(&mut self, cmdline: String) {
        self.current_cmdline = Some(cmdline);
    }

    /// The shell shows its prompt again (`ptyxis_tab_notify_shell_precmd_cb()`).
    /// Returns the command line to notify about, if a command took at least
    /// [`TIME_WATERMARK`]. A command that ended before its command line was
    /// found is too quick to matter.
    pub fn precmd(&mut self, now: Instant) -> Option<String> {
        let cmdline = self.current_cmdline.take()?;
        // An unset start counts as long ago, like the C code's zero.
        let elapsed = self
            .command_start
            .map_or(Duration::MAX, |start| now.saturating_duration_since(start));
        (elapsed >= TIME_WATERMARK).then_some(cmdline)
    }
}

/// How to tell the user about a finished command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// The user is looking at the tab, or it is in no window.
    Nothing,
    /// Flag the tab in its focused window.
    Attention,
    /// Flag the tab and send a desktop notification.
    Notification,
}

/// Decides like `ptyxis_tab_notify_show_notification()`.
pub fn delivery(in_window: bool, window_active: bool, tab_selected: bool) -> Delivery {
    match (in_window, window_active, tab_selected) {
        (false, _, _) | (true, true, true) => Delivery::Nothing,
        (true, true, false) => Delivery::Attention,
        (true, false, _) => Delivery::Notification,
    }
}

/// Tells the user that `cmdline` finished in the tab rooted at `tab_root`,
/// whose notification is identified by `uuid`
/// (`ptyxis_tab_notify_show_notification()`).
pub fn show(tab_root: &gtk::Box, uuid: &str, cmdline: &str) {
    let window = tab_root.root().and_downcast::<gtk::Window>();
    let page = tab::find_page(tab_root);
    let tab_selected = page
        .as_ref()
        .is_some_and(|(tab_view, page)| tab_view.selected_page().as_ref() == Some(page));
    let delivery = delivery(
        window.is_some(),
        window.as_ref().is_some_and(|w| w.is_active()),
        tab_selected,
    );
    if delivery == Delivery::Nothing {
        return;
    }
    if delivery == Delivery::Notification {
        if let Some(window) = &window {
            set_urgency_hint(window, true);
        }
        send_notification(uuid, cmdline);
    }
    if let Some((_, page)) = page {
        page.set_needs_attention(true);
    }
}

/// Sets or clears the X11 urgency hint of `window`, through which X11
/// window managers flag it; other backends have no such hint. The window
/// clears it again once it becomes active
/// (`ptyxis_window_notify_is_active_cb()`).
pub fn set_urgency_hint(window: &gtk::Window, urgent: bool) {
    #[cfg(feature = "x11")]
    if let Some(surface) = window.surface().and_downcast::<gdk_x11::X11Surface>() {
        // Deprecated with the rest of the X11 backend in GTK 4.18, which
        // Ptyxis still uses.
        #[allow(deprecated)]
        surface.set_urgency_hint(urgent);
    }
    #[cfg(not(feature = "x11"))]
    let _ = (window, urgent);
}

fn send_notification(uuid: &str, cmdline: &str) {
    let Some(application) = gio::Application::default() else {
        return;
    };
    let notification = gio::Notification::new(NOTIFICATION_TITLE);
    notification.set_body(Some(cmdline));
    // Ptyxis names it after APP_ID, but tty-rex's icon keeps the default
    // id, so take the window's; GIO falls back to the non-symbolic name.
    notification.set_icon(&gio::ThemedIcon::new(&format!(
        "{}-symbolic",
        app::icon_name()
    )));
    notification
        .set_default_action_and_target_value(FOCUS_TAB_BY_UUID_ACTION, Some(&uuid.to_variant()));
    application.send_notification(Some(uuid), &notification);
}

/// Withdraws the tab's notification once the user has seen the tab
/// (`ptyxis_tab_notify_contains_focus_cb()`).
pub fn withdraw(uuid: &str) {
    if let Some(application) = gio::Application::default() {
        application.withdraw_notification(uuid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deliveries() {
        let cases = [
            (false, false, false, Delivery::Nothing),
            (false, true, true, Delivery::Nothing),
            (true, true, true, Delivery::Nothing),
            (true, true, false, Delivery::Attention),
            (true, false, true, Delivery::Notification),
            (true, false, false, Delivery::Notification),
        ];
        for (in_window, active, selected, expected) in cases {
            assert_eq!(
                delivery(in_window, active, selected),
                expected,
                "{in_window} {active} {selected}"
            );
        }
    }

    /// A step of the shell's reporting, at a time in milliseconds.
    #[derive(Debug, Clone, Copy)]
    enum Step {
        Preexec(u64),
        Polled(&'static str),
        Precmd(u64, Option<&'static str>),
    }

    #[test]
    fn command_sequences() {
        use Step::*;
        let cases: &[&[Step]] = &[
            // Long enough.
            &[Preexec(0), Polled("make"), Precmd(250, Some("make"))],
            &[Preexec(0), Polled("make"), Precmd(5000, Some("make"))],
            // Too quick.
            &[Preexec(0), Polled("ls"), Precmd(249, None)],
            // The prompt came back before the command line was found.
            &[Preexec(0), Precmd(1000, None)],
            // A prompt without a command (e.g. an empty line).
            &[Precmd(1000, None)],
            // Each prompt reports the command once.
            &[
                Preexec(0),
                Polled("sleep 1"),
                Precmd(1000, Some("sleep 1")),
                Precmd(2000, None),
            ],
            // A new command forgets the previous one's command line.
            &[Preexec(0), Polled("a"), Preexec(100), Precmd(1000, None)],
            // As in Ptyxis, a command line found after the prompt came back
            // is reported at the next prompt.
            &[
                Preexec(0),
                Precmd(10, None),
                Polled("true"),
                Precmd(1000, Some("true")),
            ],
            // An empty command line, as the agent reports when it has none.
            &[Preexec(0), Polled(""), Precmd(300, Some(""))],
        ];
        for steps in cases {
            let base = Instant::now();
            let at = |ms: u64| base + Duration::from_millis(ms);
            let mut notify = TabNotify::default();
            for step in *steps {
                match *step {
                    Preexec(ms) => notify.preexec(at(ms)),
                    Polled(cmdline) => notify.polled(cmdline.to_owned()),
                    Precmd(ms, expected) => {
                        assert_eq!(notify.precmd(at(ms)).as_deref(), expected, "{steps:?}")
                    }
                }
            }
        }
    }
}
