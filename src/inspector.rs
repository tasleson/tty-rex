// inspector.rs
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

//! The terminal inspector (`src/ptyxis-inspector.c` / `.ui`), opened from
//! the terminal context menu's "Inspect Terminal" item.
//!
//! Foreground process and container details come from `ptyxis-agent` in
//! the C version; without it the process row always reads "Shell" (what
//! the C inspector shows at a prompt) and the container rows are unset.
#![allow(deprecated)] // AdwPreferencesWindow, as in the C inspector.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use relm4::prelude::*;
use vte::prelude::*;

use crate::palette;
use crate::palette_preview::ColorSwatch;
use crate::profile::Profile;
use crate::signal_guard::SignalGuard;
use crate::terminal::{self, ZoomLevel};

const UNSET: &str = "unset";
const UNTRACKED: &str = "untracked";
const SHELL: &str = "Shell";
const SWATCHES_PER_ROW: usize = 8;

/// `Row: %3d,  Column: %3d` with 1-based positions.
pub fn format_cursor(column: i64, row: i64) -> String {
    format!("Row: {:3},  Column: {:3}", row + 1, column + 1)
}

/// The cell under the pointer as (row, column), if inside the terminal
/// (`get_coord_at_xy()`).
pub fn cell_at(x: f64, y: f64, size: (i32, i32), char_size: (i64, i64)) -> Option<(u32, u32)> {
    let (width, height) = size;
    let (char_width, char_height) = char_size;
    if x < 0.0 || y < 0.0 || x >= f64::from(width) || y >= f64::from(height) {
        return None;
    }
    if char_width <= 0 || char_height <= 0 {
        return None;
    }
    let column = (x / char_width as f64) as u32;
    let row = (y / char_height as f64) as u32;
    Some((row, column))
}

pub fn format_pointer(cell: Option<(u32, u32)>) -> String {
    match cell {
        Some((row, column)) => format!("Row: {},  Column: {}", row + 1, column + 1),
        None => UNTRACKED.to_owned(),
    }
}

fn or_unset(value: Option<String>) -> String {
    value
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| UNSET.to_owned())
}

pub struct InspectorInit {
    pub terminal: vte::Terminal,
    pub profile: Profile,
    pub zoom: Rc<Cell<ZoomLevel>>,
}

#[derive(Debug)]
pub enum InspectorInput {
    Cursor,
    CellSize,
    GridSize,
    Font,
    Uris,
    Palette,
    Pointer(Option<(f64, f64)>),
}

struct Rows {
    command: adw::ActionRow,
    window_title: adw::ActionRow,
    current_directory: adw::ActionRow,
    current_file: adw::ActionRow,
    container_runtime: adw::ActionRow,
    container_name: adw::ActionRow,
    grid_size: adw::ActionRow,
    font_desc: adw::ActionRow,
    cell_size: adw::ActionRow,
    cursor: adw::ActionRow,
    pointer: adw::ActionRow,
    hyperlink_hover: adw::ActionRow,
}

pub struct Inspector {
    terminal: vte::Terminal,
    profile: Profile,
    zoom: Rc<Cell<ZoomLevel>>,
    rows: Rows,
    swatches: Vec<ColorSwatch>,
    motion: gtk::EventControllerMotion,
    handlers: Vec<SignalGuard>,
    tick: Option<gtk::TickCallbackId>,
}

fn property_row(title: &str, classes: &[&str]) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(title)
        .css_classes(classes.to_vec())
        .build()
}

fn page_with(
    title: &str,
    icon: &str,
    groups: &[(Option<&str>, Vec<&adw::ActionRow>)],
) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title(title)
        .icon_name(icon)
        .build();
    for (group_title, rows) in groups {
        let group = adw::PreferencesGroup::new();
        if let Some(t) = group_title {
            group.set_title(t);
        }
        for row in rows {
            group.add(*row);
        }
        page.add(&group);
    }
    page
}

impl Inspector {
    fn update_cursor(&self) {
        let (column, row) = self.terminal.cursor_position();
        self.rows.cursor.set_subtitle(&format_cursor(column, row));
    }

    fn update_cell_size(&self) {
        self.rows.cell_size.set_subtitle(&format!(
            "{} × {} Units (Scale Factor {})",
            self.terminal.char_width(),
            self.terminal.char_height(),
            self.terminal.scale_factor()
        ));
    }

    fn update_grid_size(&self) {
        self.rows.grid_size.set_subtitle(&format!(
            "{} × {}",
            self.terminal.column_count(),
            self.terminal.row_count()
        ));
    }

    fn update_font(&self) {
        let name = self
            .terminal
            .font()
            .map(|font| font.to_str().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| UNSET.to_owned());
        self.rows
            .font_desc
            .set_subtitle(&format!("{name} at {}", self.zoom.get().label()));
    }

    fn update_uris(&self) {
        self.rows
            .window_title
            .set_subtitle(&or_unset(terminal::window_title(&self.terminal)));
        self.rows
            .current_directory
            .set_subtitle(&or_unset(terminal::current_directory_uri(&self.terminal)));
        self.rows
            .current_file
            .set_subtitle(&or_unset(terminal::current_file_uri(&self.terminal)));
        self.rows.hyperlink_hover.set_subtitle(&or_unset(
            self.terminal
                .hyperlink_hover_uri()
                .map(|uri| uri.to_string()),
        ));
    }

    fn update_palette(&self) {
        let palette = palette::lookup_or_default(&self.profile.palette_id());
        let dark = adw::StyleManager::default().is_dark();
        for swatch in &self.swatches {
            swatch.set_palette(palette.as_deref(), dark);
        }
    }

    fn update_pointer(&self, position: Option<(f64, f64)>) {
        let cell = position.and_then(|(x, y)| {
            cell_at(
                x,
                y,
                (self.terminal.width(), self.terminal.height()),
                (self.terminal.char_width(), self.terminal.char_height()),
            )
        });
        self.rows.pointer.set_subtitle(&format_pointer(cell));
    }
}

impl Component for Inspector {
    type Init = InspectorInit;
    type Input = InspectorInput;
    type Output = ();
    type CommandOutput = ();
    type Root = adw::PreferencesWindow;
    type Widgets = ();

    fn init_root() -> Self::Root {
        adw::PreferencesWindow::builder()
            .title("Inspector")
            .modal(false)
            .build()
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let pid = gtk::Label::builder()
            .css_classes(["dim-label", "numeric"])
            .build();
        let command = property_row("Foreground Process", &["property"]);
        command.add_suffix(&pid);
        command.set_subtitle(SHELL);
        let rows = Rows {
            command,
            window_title: property_row("Window Title", &["property"]),
            current_directory: property_row("Current Directory", &["property"]),
            current_file: property_row("Current File", &["property"]),
            container_runtime: property_row("Runtime", &["property"]),
            container_name: property_row("Name", &["property"]),
            grid_size: property_row("Grid Size", &["numeric"]),
            font_desc: property_row("Name", &["property"]),
            cell_size: property_row("Cell Size", &["property"]),
            cursor: property_row("Position", &["property", "numeric"]),
            pointer: property_row("Position", &["property", "numeric"]),
            hyperlink_hover: property_row("Hyperlink", &["property"]),
        };
        rows.container_runtime.set_subtitle(UNSET);
        rows.container_name.set_subtitle(UNSET);

        let swatches: Vec<ColorSwatch> = (0..palette::N_INDEXED_COLORS)
            .map(ColorSwatch::new)
            .collect();
        let colors = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .spacing(3)
            .build();
        for chunk in swatches.chunks(SWATCHES_PER_ROW) {
            let line = gtk::Box::builder().spacing(3).build();
            for swatch in chunk {
                line.append(swatch.widget());
            }
            colors.append(&line);
        }
        let palette_row = property_row("Colors", &[]);
        palette_row.add_suffix(&colors);

        root.add(&page_with(
            "Process",
            "application-x-executable-symbolic",
            &[
                (None, vec![&rows.command]),
                (None, vec![&rows.window_title]),
                (None, vec![&rows.current_directory, &rows.current_file]),
                (
                    Some("Container"),
                    vec![&rows.container_runtime, &rows.container_name],
                ),
            ],
        ));
        root.add(&page_with(
            "Appearance",
            "font-select-symbolic",
            &[
                (None, vec![&rows.grid_size]),
                (Some("Palette"), vec![&palette_row]),
                (Some("Font"), vec![&rows.font_desc, &rows.cell_size]),
            ],
        ));
        root.add(&page_with(
            "Input",
            "input-keyboard-symbolic",
            &[
                (Some("Cursor"), vec![&rows.cursor]),
                (Some("Mouse"), vec![&rows.pointer, &rows.hyperlink_hover]),
            ],
        ));

        let terminal = init.terminal;
        let mut handlers = Vec::new();
        let emit = |message: fn() -> InspectorInput| {
            let input = sender.input_sender().clone();
            move |_: &vte::Terminal| input.emit(message())
        };
        let ids = [
            terminal.connect_cursor_moved(emit(|| InspectorInput::Cursor)),
            {
                let input = sender.input_sender().clone();
                terminal
                    .connect_char_size_changed(move |_, _, _| input.emit(InspectorInput::CellSize))
            },
            terminal.connect_font_desc_notify(emit(|| InspectorInput::Font)),
            terminal.connect_font_scale_notify(emit(|| InspectorInput::Font)),
            terminal::connect_window_title_changed(&terminal, emit(|| InspectorInput::Uris)),
            terminal::connect_current_directory_uri_changed(
                &terminal,
                emit(|| InspectorInput::Uris),
            ),
            terminal::connect_current_file_uri_changed(&terminal, emit(|| InspectorInput::Uris)),
            terminal.connect_hyperlink_hover_uri_notify(emit(|| InspectorInput::Uris)),
        ];
        handlers.extend(ids.into_iter().map(|id| SignalGuard::new(&terminal, id)));
        let input = sender.input_sender().clone();
        let profile_settings = init.profile.gsettings();
        handlers.push(SignalGuard::new(
            profile_settings,
            profile_settings.connect_changed(Some(crate::profile::key::PALETTE), move |_, _| {
                input.emit(InspectorInput::Palette)
            }),
        ));
        let input = sender.input_sender().clone();
        let style_manager = adw::StyleManager::default();
        handlers.push(SignalGuard::new(
            &style_manager,
            style_manager.connect_dark_notify(move |_| input.emit(InspectorInput::Palette)),
        ));

        // VTE has no grid-size signal (the C terminal emits its own from
        // size_allocate), so watch the counts once per frame.
        let last_grid = Cell::new((0, 0));
        let input = sender.input_sender().clone();
        let tick = terminal.add_tick_callback(move |terminal, _| {
            let grid = (terminal.column_count(), terminal.row_count());
            if last_grid.replace(grid) != grid {
                input.emit(InspectorInput::GridSize);
            }
            glib::ControlFlow::Continue
        });

        let motion = gtk::EventControllerMotion::new();
        let input = sender.input_sender().clone();
        motion.connect_enter(move |_, x, y| input.emit(InspectorInput::Pointer(Some((x, y)))));
        let input = sender.input_sender().clone();
        motion.connect_motion(move |_, x, y| input.emit(InspectorInput::Pointer(Some((x, y)))));
        let input = sender.input_sender().clone();
        motion.connect_leave(move |_| input.emit(InspectorInput::Pointer(None)));
        terminal.add_controller(motion.clone());

        let model = Inspector {
            terminal,
            profile: init.profile,
            zoom: init.zoom,
            rows,
            swatches,
            motion,
            handlers,
            tick: Some(tick),
        };
        model.update_cursor();
        model.update_cell_size();
        model.update_grid_size();
        model.update_font();
        model.update_uris();
        model.update_palette();
        model.update_pointer(None);
        ComponentParts { model, widgets: () }
    }

    fn update(&mut self, message: Self::Input, _sender: ComponentSender<Self>, _root: &Self::Root) {
        match message {
            InspectorInput::Cursor => self.update_cursor(),
            InspectorInput::CellSize => self.update_cell_size(),
            InspectorInput::GridSize => self.update_grid_size(),
            InspectorInput::Font => self.update_font(),
            InspectorInput::Uris => self.update_uris(),
            InspectorInput::Palette => self.update_palette(),
            InspectorInput::Pointer(position) => self.update_pointer(position),
        }
    }

    fn shutdown(&mut self, _widgets: &mut Self::Widgets, _output: relm4::Sender<Self::Output>) {
        self.handlers.clear();
        if let Some(tick) = self.tick.take() {
            tick.remove();
        }
        self.terminal.remove_controller(&self.motion);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_positions() {
        assert_eq!(format_cursor(0, 0), "Row:   1,  Column:   1");
        assert_eq!(format_cursor(79, 123), "Row: 124,  Column:  80");
    }

    #[test]
    fn pointer_cells() {
        let size = (800, 480);
        let cell = (10, 20);
        let cases = [
            ((0.0, 0.0), Some((0, 0))),
            ((15.0, 45.0), Some((2, 1))),
            ((799.0, 479.0), Some((23, 79))),
            ((800.0, 10.0), None),
            ((-1.0, 10.0), None),
        ];
        for ((x, y), expected) in cases {
            assert_eq!(cell_at(x, y, size, cell), expected, "{x},{y}");
        }
        assert_eq!(cell_at(1.0, 1.0, size, (0, 0)), None);
        assert_eq!(format_pointer(Some((2, 1))), "Row: 3,  Column: 2");
        assert_eq!(format_pointer(None), UNTRACKED);
    }
}
