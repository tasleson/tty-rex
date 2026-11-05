// preferences/accel_dialog.rs
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

//! The "Set Shortcut" dialog (`src/ptyxis-shortcut-accel-dialog.c` / `.ui`).

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use relm4::prelude::*;

const ENTER_SHORTCUT_IMAGE: &str = "/org/gnome/Ptyxis/icons/enter-keyboard-shortcut.svg";
const SELECTION_PAGE: &str = "selection";
const DISPLAY_PAGE: &str = "display";
/// MacBook "fn" key, ignored like in the C dialog.
const FN_KEYCODE: u32 = 0x01D8;

/// Outcome of a key press while capturing a shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capture {
    /// A lone Escape: close without changes.
    Cancel,
    /// A lone Backspace: disable the shortcut.
    Disable,
    /// A new accelerator.
    Accelerator(gdk::Key, gdk::ModifierType),
}

fn is_arrow(key: gdk::Key) -> bool {
    matches!(
        key,
        gdk::Key::Left | gdk::Key::Right | gdk::Key::Up | gdk::Key::Down
    )
}

/// Whether Shift only selected the keyval and should not be part of the
/// accelerator (`should_drop_shift()`); Shift+arrows are kept.
fn should_drop_shift(keyval_was: gdk::Key, keyval_is: gdk::Key) -> bool {
    !is_arrow(keyval_was) && !is_arrow(keyval_is) && keyval_was == keyval_is
}

/// Interprets a non-modifier key press the way
/// `ptyxis_shortcut_accel_dialog_key_pressed()` does. `default_mask` is
/// `gtk_accelerator_get_default_mod_mask()`.
pub fn capture(
    keyval: gdk::Key,
    state: gdk::ModifierType,
    default_mask: gdk::ModifierType,
) -> Capture {
    let mut real_mask = state & default_mask;
    let mut keyval_lower = keyval.to_lower();
    if keyval_lower == gdk::Key::ISO_Left_Tab {
        keyval_lower = gdk::Key::Tab;
    }
    // Put shift back if it changed the case of the key.
    if keyval_lower != keyval {
        real_mask |= gdk::ModifierType::SHIFT_MASK;
    }
    // Keep Alt+Print rather than SysRq.
    if keyval_lower == gdk::Key::Sys_Req && real_mask.contains(gdk::ModifierType::ALT_MASK) {
        keyval_lower = gdk::Key::Print;
    }
    if real_mask.is_empty() && keyval_lower == gdk::Key::Escape {
        return Capture::Cancel;
    }
    if real_mask.is_empty() && keyval_lower == gdk::Key::BackSpace {
        return Capture::Disable;
    }

    let mut key = keyval.to_lower();
    let mut modifier = state & default_mask & !gdk::ModifierType::LOCK_MASK;
    if state.contains(gdk::ModifierType::SHIFT_MASK) && should_drop_shift(key, keyval) {
        modifier.remove(gdk::ModifierType::SHIFT_MASK);
    }
    if !state.contains(gdk::ModifierType::LOCK_MASK) && key != keyval {
        modifier |= gdk::ModifierType::SHIFT_MASK;
    }
    if key == gdk::Key::ISO_Left_Tab && modifier == gdk::ModifierType::CONTROL_MASK {
        key = gdk::Key::Tab;
        modifier = gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK;
    }
    Capture::Accelerator(key, modifier)
}

/// Whether a key release ends capturing (`..._key_released()`): any
/// release when the chord has no modifier, or releasing the modifier the
/// chord started with.
pub fn release_ends_capture(
    modifier: gdk::ModifierType,
    released_is_modifier: bool,
    have_key: bool,
    first_modifier: Option<gdk::Key>,
    released: gdk::Key,
) -> bool {
    modifier.is_empty() || (released_is_modifier && have_key && first_modifier == Some(released))
}

#[derive(Debug)]
struct CaptureState {
    editing: bool,
    key: Option<gdk::Key>,
    modifier: gdk::ModifierType,
    first_modifier: Option<gdk::Key>,
}

pub struct AccelDialogInit {
    /// Row title, shown in bold in the prompt.
    pub title: String,
    pub settings: gio::Settings,
    pub key: &'static str,
}

#[derive(Debug)]
pub enum AccelDialogInput {
    /// Capture state changed; refresh the stack and buttons.
    Refresh,
    Set,
    Reset,
}

pub struct AccelDialog {
    settings: gio::Settings,
    key: &'static str,
    default_accelerator: Option<String>,
    state: Rc<RefCell<CaptureState>>,
    stack: gtk::Stack,
    shortcut_label: gtk::ShortcutLabel,
    reset_button: gtk::Button,
    set_action: gio::SimpleAction,
}

impl AccelDialog {
    fn accelerator(&self) -> Option<String> {
        let state = self.state.borrow();
        state
            .key
            .map(|key| gtk::accelerator_name(key, state.modifier).to_string())
    }

    fn equals_default(&self, current: Option<&str>) -> bool {
        let parse = |accel: &str| gtk::accelerator_parse(accel);
        match (current, self.default_accelerator.as_deref()) {
            (None, None) => true,
            (Some(a), Some(b)) => parse(a).is_some() && parse(a) == parse(b),
            _ => false,
        }
    }

    fn refresh(&self) {
        let editing = self.state.borrow().editing;
        self.stack.set_visible_child_name(if editing {
            SELECTION_PAGE
        } else {
            DISPLAY_PAGE
        });
        self.set_action.set_enabled(!editing);
        let current = self.accelerator();
        self.shortcut_label
            .set_accelerator(current.as_deref().unwrap_or_default());
        self.reset_button
            .set_visible(editing && !self.equals_default(current.as_deref()));
    }

    fn save(&self, accelerator: &str, root: &adw::Dialog) {
        if let Err(e) = self.settings.set_string(self.key, accelerator) {
            glib::g_warning!("tty-rex", "Failed to save shortcut {}: {e}", self.key);
        }
        root.close();
    }
}

fn prompt_label(markup: &str) -> gtk::Label {
    gtk::Label::builder()
        .xalign(0.5)
        .use_markup(true)
        .label(markup)
        .build()
}

impl Component for AccelDialog {
    type Init = AccelDialogInit;
    type Input = AccelDialogInput;
    type Output = ();
    type CommandOutput = ();
    type Root = adw::Dialog;
    type Widgets = ();

    fn init_root() -> Self::Root {
        adw::Dialog::builder()
            .title("Set Shortcut")
            .content_width(360)
            .width_request(360)
            .content_height(300)
            .build()
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let prompt = format!(
            "Enter new shortcut to change <b>{}</b>.",
            glib::markup_escape_text(&init.title)
        );

        let cancel = gtk::Button::builder()
            .label("Cancel")
            .action_name("window.close")
            .build();
        let reset_button = gtk::Button::builder()
            .label("Reset")
            .action_name("shortcut.reset")
            .visible(false)
            .build();
        let accept = gtk::Button::builder()
            .label("Set")
            .action_name("shortcut.set")
            .css_classes(["suggested-action"])
            .build();
        let sizes = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
        for button in [&cancel, &reset_button, &accept] {
            sizes.add_widget(button);
        }
        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .build();
        header.pack_start(&cancel);
        header.pack_start(&reset_button);
        header.pack_end(&accept);

        let selection = gtk::Box::new(gtk::Orientation::Vertical, 18);
        selection.append(&prompt_label(&prompt));
        let image = gtk::Image::builder().hexpand(true).vexpand(true).build();
        image.set_resource(Some(ENTER_SHORTCUT_IMAGE));
        selection.append(&image);
        selection.append(
            &gtk::Label::builder()
                .xalign(0.5)
                .label("Press Escape to cancel or Backspace to disable.")
                .css_classes(["dim-label"])
                .build(),
        );
        let shortcut_label = gtk::ShortcutLabel::builder()
            .halign(gtk::Align::Center)
            .css_classes(["dim-label"])
            .build();
        let display = gtk::Box::new(gtk::Orientation::Vertical, 18);
        display.append(&prompt_label(&prompt));
        display.append(&shortcut_label);
        let stack = gtk::Stack::builder()
            .hhomogeneous(true)
            .vhomogeneous(true)
            .margin_top(14)
            .margin_bottom(14)
            .margin_start(14)
            .margin_end(14)
            .build();
        stack.add_named(&selection, Some(SELECTION_PAGE));
        stack.add_named(&display, Some(DISPLAY_PAGE));

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&stack));
        root.set_child(Some(&toolbar));

        let actions = gio::SimpleActionGroup::new();
        let set_action = gio::SimpleAction::new("set", None);
        let input = sender.input_sender().clone();
        set_action.connect_activate(move |_, _| input.emit(AccelDialogInput::Set));
        actions.add_action(&set_action);
        let reset_action = gio::SimpleAction::new("reset", None);
        let input = sender.input_sender().clone();
        reset_action.connect_activate(move |_, _| input.emit(AccelDialogInput::Reset));
        actions.add_action(&reset_action);
        root.insert_action_group("shortcut", Some(&actions));

        let current = init.settings.string(init.key);
        let parsed = (!current.is_empty())
            .then(|| gtk::accelerator_parse(current.as_str()))
            .flatten();
        let state = Rc::new(RefCell::new(CaptureState {
            editing: true,
            key: parsed.map(|(key, _)| key),
            modifier: parsed.map_or(gdk::ModifierType::empty(), |(_, mods)| mods),
            first_modifier: None,
        }));

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let pressed_state = Rc::clone(&state);
        let input = sender.input_sender().clone();
        let dialog = root.downgrade();
        keys.connect_key_pressed(move |controller, keyval, keycode, state| {
            if keycode == FN_KEYCODE || !pressed_state.borrow().editing {
                return glib::Propagation::Proceed;
            }
            let is_modifier = controller
                .current_event()
                .and_then(|event| event.downcast::<gdk::KeyEvent>().ok())
                .is_some_and(|event| event.is_modifier());
            let mut capture_state = pressed_state.borrow_mut();
            if is_modifier {
                if capture_state.key.is_none() && capture_state.modifier.is_empty() {
                    capture_state.first_modifier = Some(keyval);
                }
                return glib::Propagation::Proceed;
            }
            match capture(keyval, state, gtk::accelerator_get_default_mod_mask()) {
                Capture::Cancel => {
                    if let Some(dialog) = dialog.upgrade() {
                        dialog.close();
                    }
                }
                Capture::Disable => {
                    capture_state.key = None;
                    capture_state.modifier = gdk::ModifierType::empty();
                    drop(capture_state);
                    input.emit(AccelDialogInput::Set);
                }
                Capture::Accelerator(key, modifier) => {
                    capture_state.key = Some(key);
                    capture_state.modifier = modifier;
                    capture_state.editing = false;
                    input.emit(AccelDialogInput::Refresh);
                }
            }
            glib::Propagation::Stop
        });
        let released_state = Rc::clone(&state);
        let input = sender.input_sender().clone();
        keys.connect_key_released(move |controller, keyval, keycode, _| {
            if keycode == FN_KEYCODE {
                return;
            }
            let mut state = released_state.borrow_mut();
            if !state.editing {
                return;
            }
            let is_modifier = controller
                .current_event()
                .and_then(|event| event.downcast::<gdk::KeyEvent>().ok())
                .is_some_and(|event| event.is_modifier());
            if release_ends_capture(
                state.modifier,
                is_modifier,
                state.key.is_some(),
                state.first_modifier,
                keyval,
            ) {
                state.editing = false;
                state.first_modifier = None;
                input.emit(AccelDialogInput::Refresh);
            }
        });
        root.add_controller(keys);

        let model = AccelDialog {
            default_accelerator: crate::shortcuts::default_accelerator(init.key),
            settings: init.settings,
            key: init.key,
            state,
            stack,
            shortcut_label,
            reset_button,
            set_action,
        };
        model.refresh();
        // Focus the accept button once a shortcut is captured, as in C.
        let accept_ref = accept.downgrade();
        model.stack.connect_visible_child_name_notify(move |stack| {
            if stack.visible_child_name().as_deref() == Some(DISPLAY_PAGE)
                && let Some(accept) = accept_ref.upgrade()
            {
                accept.grab_focus();
            }
        });
        ComponentParts { model, widgets: () }
    }

    fn update(&mut self, message: Self::Input, _sender: ComponentSender<Self>, root: &Self::Root) {
        match message {
            AccelDialogInput::Refresh => self.refresh(),
            AccelDialogInput::Set => {
                let accelerator = self.accelerator().unwrap_or_default();
                self.save(&accelerator, root);
            }
            AccelDialogInput::Reset => {
                if let Some(default) = self.default_accelerator.clone() {
                    self.save(&default, root);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gdk::Key;
    use gdk::ModifierType as M;

    fn default_mask() -> M {
        M::CONTROL_MASK | M::SHIFT_MASK | M::ALT_MASK | M::SUPER_MASK | M::HYPER_MASK | M::META_MASK
    }

    #[test]
    fn key_presses() {
        let ctrl = M::CONTROL_MASK;
        let shift = M::SHIFT_MASK;
        let cases = [
            (Key::Escape, M::empty(), Capture::Cancel),
            (Key::BackSpace, M::empty(), Capture::Disable),
            // Escape with a modifier is a real shortcut.
            (Key::Escape, ctrl, Capture::Accelerator(Key::Escape, ctrl)),
            (Key::t, ctrl, Capture::Accelerator(Key::t, ctrl)),
            // Shift producing an upper-case letter becomes <shift>t.
            (
                Key::T,
                ctrl | shift,
                Capture::Accelerator(Key::t, ctrl | shift),
            ),
            // Shift that did not change the key is dropped...
            (Key::F5, shift, Capture::Accelerator(Key::F5, M::empty())),
            // ...except for arrows.
            (Key::Left, shift, Capture::Accelerator(Key::Left, shift)),
            // Caps Lock is never part of an accelerator.
            (
                Key::a,
                ctrl | M::LOCK_MASK,
                Capture::Accelerator(Key::a, ctrl),
            ),
            (
                Key::ISO_Left_Tab,
                ctrl,
                Capture::Accelerator(Key::Tab, ctrl | shift),
            ),
            (
                Key::Page_Down,
                ctrl,
                Capture::Accelerator(Key::Page_Down, ctrl),
            ),
        ];
        for (keyval, state, expected) in cases {
            assert_eq!(
                capture(keyval, state, default_mask()),
                expected,
                "{keyval:?} {state:?}"
            );
        }
    }

    #[test]
    fn key_releases() {
        let ctrl = M::CONTROL_MASK;
        let cases = [
            (M::empty(), false, true, None, Key::a, true),
            (ctrl, false, true, Some(Key::Control_L), Key::a, false),
            (ctrl, true, true, Some(Key::Control_L), Key::Control_L, true),
            (ctrl, true, true, Some(Key::Control_L), Key::Shift_L, false),
            (
                ctrl,
                true,
                false,
                Some(Key::Control_L),
                Key::Control_L,
                false,
            ),
        ];
        for (modifier, is_modifier, have_key, first, released, expected) in cases {
            assert_eq!(
                release_ends_capture(modifier, is_modifier, have_key, first, released),
                expected,
                "{modifier:?} {released:?}"
            );
        }
    }
}
