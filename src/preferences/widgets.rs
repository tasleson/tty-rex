// preferences/widgets.rs
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

//! Row constructors and GSettings binding helpers shared by the
//! preferences pages, mirroring the widgets declared in
//! `src/ptyxis-preferences-window.ui` and `src/ptyxis-profile-editor.ui`.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::choices::{self, Choice};
use crate::color_dialog;
use crate::settings::{self, AppSettings};

pub const UNDO_LABEL: &str = "Undo";
pub const COPIED_TOAST_TIMEOUT_SECS: u32 = 3;
const RESET_COLOR_ICON: &str = "edit-undo-symbolic";
const RESET_COLOR_TOOLTIP: &str = "Use Palette Color";
const CHOOSE_COLOR_TOOLTIP: &str = "Choose Color";
/// Color swatch size (width, height), corner radius and outline.
const SWATCH_SIZE: (i32, i32) = (32, 20);
const SWATCH_RADIUS: f64 = 4.0;
const SWATCH_OUTLINE_WIDTH: f64 = 1.0;
const SWATCH_OUTLINE_ALPHA: f64 = 0.2;

/// Formats an opacity like `opacity_to_label()` (`"%3.0lf%%"` of the
/// floored percentage).
pub fn opacity_label(opacity: f64) -> String {
    format!("{:>3.0}%", (100.0 * opacity).floor())
}

pub fn group(title: Option<&str>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    if let Some(title) = title {
        group.set_title(title);
    }
    group
}

pub fn page(title: &str, icon_name: &str) -> adw::PreferencesPage {
    adw::PreferencesPage::builder()
        .title(title)
        .icon_name(icon_name)
        .build()
}

pub fn switch_row(title: &str, subtitle: Option<&str>) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder().title(title).build();
    if let Some(subtitle) = subtitle {
        row.set_subtitle(subtitle);
    }
    row
}

pub fn combo_row(title: &str, subtitle: Option<&str>, choices: &[Choice]) -> adw::ComboRow {
    let row = adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(&choices::titles(choices)))
        .build();
    if let Some(subtitle) = subtitle {
        row.set_subtitle(subtitle);
    }
    row
}

/// Spin row bounds: (lower, upper, step, page, digits).
pub type SpinRange = (f64, f64, f64, f64, u32);

pub const SPACING_RANGE: SpinRange = (1.0, 2.0, 0.1, 0.5, 1);
pub const GRID_RANGE: SpinRange = (1.0, 65535.0, 1.0, 10.0, 0);
pub const SCROLLBACK_RANGE: SpinRange = (0.0, 2_147_483_647.0, 10.0, 100.0, 0);
pub const TELEMETRY_INTERVAL_RANGE: SpinRange = (
    settings::TELEMETRY_INTERVAL_MIN as f64,
    settings::TELEMETRY_INTERVAL_MAX as f64,
    1.0,
    10.0,
    0,
);

pub fn spin_row(title: &str, subtitle: Option<&str>, range: SpinRange) -> adw::SpinRow {
    let (lower, upper, step, page, digits) = range;
    let adjustment = gtk::Adjustment::new(lower, lower, upper, step, page, 0.0);
    let row = adw::SpinRow::builder()
        .title(title)
        .adjustment(&adjustment)
        .digits(digits)
        .numeric(digits == 0)
        .build();
    if let Some(subtitle) = subtitle {
        row.set_subtitle(subtitle);
    }
    row
}

/// The opacity row: a flat menu button showing the percentage that pops
/// up a scale (the "Transparency" rows in both .ui files).
pub fn opacity_row() -> (adw::ActionRow, gtk::Adjustment, gtk::Label) {
    let adjustment = gtk::Adjustment::new(1.0, 0.0, 1.0, 0.05, 0.25, 0.0);
    let scale = gtk::Scale::builder()
        .adjustment(&adjustment)
        .width_request(200)
        .build();
    for mark in [0.25, 0.5, 0.75] {
        scale.add_mark(mark, gtk::PositionType::Bottom, None);
    }
    let label = gtk::Label::builder()
        .width_chars(6)
        .max_width_chars(6)
        .build();
    let button_content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    button_content.append(&label);
    button_content.append(&gtk::Image::from_icon_name("pan-down-symbolic"));
    let button = gtk::MenuButton::builder()
        .css_classes(["flat"])
        .valign(gtk::Align::Center)
        .child(&button_content)
        .popover(&gtk::Popover::builder().child(&scale).build())
        .build();
    let row = adw::ActionRow::builder()
        .title("Transparency")
        .subtitle("Reduces the opacity of the background window")
        .activatable_widget(&scale)
        .build();
    row.add_suffix(&button);

    let update_label = {
        let label = label.clone();
        move |adjustment: &gtk::Adjustment| label.set_label(&opacity_label(adjustment.value()))
    };
    update_label(&adjustment);
    adjustment.connect_value_changed(update_label);
    (row, adjustment, label)
}

/// A row choosing a color for a tty-rex color setting, where an empty
/// value means a color derived elsewhere (the palette's). That fallback
/// is shown but not stored, and a reset button returns to it.
///
/// `gtk::ColorDialogButton` is not used because it only reports a pick
/// through `notify::rgba`, which stays silent when the picked color equals
/// the one shown, so choosing the palette color itself would not store it.
/// Running the dialog from a plain button stores whatever it returns.
pub struct ColorRow {
    pub row: adw::ActionRow,
    settings: AppSettings,
    key: &'static str,
    /// The color the swatch shows and the dialog starts from.
    shown: Rc<Cell<gdk::RGBA>>,
    swatch: gtk::DrawingArea,
    reset: gtk::Button,
}

impl ColorRow {
    pub fn new(settings: &AppSettings, key: &'static str, title: &str, subtitle: &str) -> Self {
        let shown = Rc::new(Cell::new(gdk::RGBA::BLACK));
        let swatch = gtk::DrawingArea::builder()
            .content_width(SWATCH_SIZE.0)
            .content_height(SWATCH_SIZE.1)
            .build();
        swatch.set_draw_func({
            let shown = Rc::clone(&shown);
            move |area, cr, width, height| {
                draw_swatch(cr, &shown.get(), &area.color(), width, height)
            }
        });
        let button = gtk::Button::builder()
            .child(&swatch)
            .tooltip_text(CHOOSE_COLOR_TOOLTIP)
            .valign(gtk::Align::Center)
            .build();
        // The swatch has no text, so name the button after its row for
        // screen readers; the tooltip only becomes the description.
        button.update_property(&[gtk::accessible::Property::Label(title)]);
        let reset = gtk::Button::builder()
            .icon_name(RESET_COLOR_ICON)
            .tooltip_text(RESET_COLOR_TOOLTIP)
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .build();
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(subtitle)
            .activatable_widget(&button)
            .build();
        row.add_suffix(&reset);
        row.add_suffix(&button);

        let store = move |settings: &AppSettings, color: Option<&gdk::RGBA>| {
            if let Err(e) = settings.set_color_override(key, color) {
                glib::g_warning!("tty-rex", "Cannot store {key}: {e}");
            }
        };
        let dialog = color_dialog::new(title);
        button.connect_clicked({
            let settings = settings.clone();
            let shown = Rc::clone(&shown);
            move |button| {
                let settings = settings.clone();
                let parent = button.root().and_downcast::<gtk::Window>();
                color_dialog::choose(
                    &dialog,
                    parent.as_ref(),
                    Some(&shown.get()),
                    None,
                    key,
                    move |color| {
                        if let Some(color) = color {
                            store(&settings, Some(&color));
                        }
                    },
                );
            }
        });
        reset.connect_clicked({
            let settings = settings.clone();
            move |_| store(&settings, None)
        });

        Self {
            row,
            settings: settings.clone(),
            key,
            shown,
            swatch,
            reset,
        }
    }

    /// Shows the stored color, or `fallback` when none (or an invalid one,
    /// which is reported elsewhere) is stored.
    pub fn refresh(&self, fallback: &gdk::RGBA) {
        let chosen = self.settings.color_override(self.key);
        self.shown.set(chosen.unwrap_or(*fallback));
        self.swatch.queue_draw();
        // An invalid value is offered for reset too, to clean it up.
        self.reset
            .set_sensitive(self.settings.has_color_override(self.key));
    }
}

/// Fills a rounded rectangle with `color`, outlined in a faint `outline`
/// (the widget's text color, so it shows in light and dark styles) so that
/// colors close to the row's background stay visible.
fn draw_swatch(
    cr: &gtk::cairo::Context,
    color: &gdk::RGBA,
    outline: &gdk::RGBA,
    width: i32,
    height: i32,
) {
    use std::f64::consts::{FRAC_PI_2, PI};

    let (width, height) = (f64::from(width), f64::from(height));
    let inset = SWATCH_OUTLINE_WIDTH / 2.0;
    let (left, top) = (inset, inset);
    let (right, bottom) = (width - inset, height - inset);
    let radius = SWATCH_RADIUS
        .min((right - left) / 2.0)
        .min((bottom - top) / 2.0);
    cr.new_sub_path();
    cr.arc(right - radius, top + radius, radius, -FRAC_PI_2, 0.0);
    cr.arc(right - radius, bottom - radius, radius, 0.0, FRAC_PI_2);
    cr.arc(left + radius, bottom - radius, radius, FRAC_PI_2, PI);
    cr.arc(left + radius, top + radius, radius, PI, 3.0 * FRAC_PI_2);
    cr.close_path();
    cr.set_source_color(color);
    // Cairo errors only latch on the context; there is nothing to recover.
    let _ = cr.fill_preserve();
    cr.set_source_rgba(
        f64::from(outline.red()),
        f64::from(outline.green()),
        f64::from(outline.blue()),
        SWATCH_OUTLINE_ALPHA,
    );
    cr.set_line_width(SWATCH_OUTLINE_WIDTH);
    let _ = cr.stroke();
}

/// A boxed list as used for profiles and custom links.
pub fn boxed_list() -> gtk::ListBox {
    gtk::ListBox::builder()
        .css_classes(["boxed-list"])
        .selection_mode(gtk::SelectionMode::None)
        .build()
}

/// The trailing "add" row of the C `PtyxisAddButtonListModel` lists.
pub fn add_button_row(title: &str, action_name: &str) -> adw::ButtonRow {
    adw::ButtonRow::builder()
        .title(title)
        .start_icon_name("list-add-symbolic")
        .action_name(action_name)
        .build()
}

pub fn bind(settings: &gio::Settings, key: &str, object: &impl IsA<glib::Object>, property: &str) {
    settings.bind(key, object, property).build();
}

/// Binds an enum key to a combo row's selection through `choices`, like
/// `g_settings_bind_with_mapping()` with `string_to_index` /
/// `index_to_string` in the C code.
pub fn bind_choice(
    settings: &gio::Settings,
    key: &str,
    row: &adw::ComboRow,
    choices: &'static [Choice],
) {
    settings
        .bind(key, row, "selected")
        .mapping(move |variant, _| {
            choices::index_of(choices, variant.str()?).map(|index| index.to_value())
        })
        .set_mapping(move |value, _| {
            choices::nick_at(choices, value.get::<u32>().ok()?).map(|nick| nick.to_variant())
        })
        .build();
}

/// Like [`bind_choice`] for a run-time list of string values (palettes,
/// containers) shown with the given titles.
pub fn bind_values(
    settings: &gio::Settings,
    key: &str,
    row: &adw::ComboRow,
    titles: &[&str],
    values: Vec<String>,
) {
    row.set_model(Some(&gtk::StringList::new(titles)));
    let values = std::rc::Rc::new(values);
    let lookup = std::rc::Rc::clone(&values);
    settings
        .bind(key, row, "selected")
        .mapping(move |variant, _| {
            let value = variant.str()?;
            let index = lookup.iter().position(|v| v == value)?;
            u32::try_from(index).ok().map(|i| i.to_value())
        })
        .set_mapping(move |value, _| {
            let index = usize::try_from(value.get::<u32>().ok()?).ok()?;
            values.get(index).map(|v| v.to_variant())
        })
        .build();
}

type BindFn = Box<dyn Fn(&gio::Settings)>;

/// Collects bindings of widgets to keys of a (profile) settings object so
/// they can be re-targeted when that object changes, as the preferences
/// window does when the default profile changes.
#[derive(Default)]
pub struct Binder {
    binds: Vec<BindFn>,
    targets: Vec<(glib::Object, &'static str)>,
}

impl Binder {
    pub fn bind(
        &mut self,
        key: &'static str,
        object: &impl IsA<glib::Object>,
        property: &'static str,
    ) {
        let target = object.clone().upcast::<glib::Object>();
        let bound = target.clone();
        self.binds.push(Box::new(move |settings| {
            settings.bind(key, &bound, property).build()
        }));
        self.targets.push((target, property));
    }

    pub fn bind_choice(
        &mut self,
        key: &'static str,
        row: &adw::ComboRow,
        choices: &'static [Choice],
    ) {
        let bound = row.clone();
        self.binds.push(Box::new(move |settings| {
            bind_choice(settings, key, &bound, choices)
        }));
        self.targets.push((row.clone().upcast(), "selected"));
    }

    /// Binds everything to `settings`, replacing earlier bindings.
    pub fn apply(&self, settings: &gio::Settings) {
        for (object, property) in &self.targets {
            gio::Settings::unbind(object, property);
        }
        for bind in &self.binds {
            bind(settings);
        }
    }
}

/// Shows a toast in the preferences window containing `widget`.
#[allow(deprecated)] // AdwPreferencesWindow, see the module docs in mod.rs.
pub fn add_toast(widget: &impl IsA<gtk::Widget>, toast: adw::Toast) {
    if let Some(window) = widget
        .ancestor(adw::PreferencesWindow::static_type())
        .and_downcast::<adw::PreferencesWindow>()
    {
        window.add_toast(toast);
    }
}

/// Pushes a subpage onto the preferences window containing `widget`.
#[allow(deprecated)]
pub fn push_subpage(widget: &impl IsA<gtk::Widget>, page: &adw::NavigationPage, replace: bool) {
    if let Some(window) = widget
        .ancestor(adw::PreferencesWindow::static_type())
        .and_downcast::<adw::PreferencesWindow>()
    {
        if replace {
            window.pop_subpage();
        }
        window.push_subpage(page);
    }
}

#[allow(deprecated)]
pub fn pop_subpage(widget: &impl IsA<gtk::Widget>) {
    if let Some(window) = widget
        .ancestor(adw::PreferencesWindow::static_type())
        .and_downcast::<adw::PreferencesWindow>()
    {
        window.pop_subpage();
    }
}

/// Installs `group.name` on `widget`, forwarding activation as `message`.
/// Each call creates its own group, so use distinct group names.
pub fn install_action<M: 'static>(
    widget: &impl IsA<gtk::Widget>,
    group: &str,
    name: &str,
    input: &relm4::Sender<M>,
    message: fn() -> M,
) -> gio::SimpleAction {
    let actions = gio::SimpleActionGroup::new();
    let action = gio::SimpleAction::new(name, None);
    let input = input.clone();
    action.connect_activate(move |_, _| input.emit(message()));
    actions.add_action(&action);
    widget.insert_action_group(group, Some(&actions));
    action
}

/// A toast offering to undo something.
pub fn undo_toast(title: &str, undo: impl Fn() + 'static) -> adw::Toast {
    let toast = adw::Toast::new(title);
    toast.set_button_label(Some(UNDO_LABEL));
    toast.connect_button_clicked(move |_| undo());
    toast
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opacity_labels() {
        let cases = [
            (1.0, "100%"),
            (0.0, "  0%"),
            (0.5, " 50%"),
            (0.955, " 95%"),
            (0.05, "  5%"),
        ];
        for (opacity, expected) in cases {
            assert_eq!(opacity_label(opacity), expected, "{opacity}");
        }
    }
}
