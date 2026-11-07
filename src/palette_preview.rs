// palette_preview.rs
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

//! Palette swatches and previews (`src/ptyxis-palette-preview.c`,
//! `src/ptyxis-palette-preview-color.c`), styled by `src/style.css`.
//!
//! These are plain widget compositions rather than relm4 components:
//! they have no messages of their own and the preferences window creates
//! one per palette.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, pango};

use crate::palette::Palette;

const PREVIEW_CSS_NAME: &str = "palettepreview";
const COLOR_CSS_NAME: &str = "palettepreviewcolor";
const SELECTED_CSS_CLASS: &str = "selected";
const SAMPLE_TEXT: &str = "The quick brown fox jumps over the lazy dog";
/// Indexed colors shown in a preview (`ptyxis-palette-preview.ui`).
const PREVIEW_COLOR_INDEXES: std::ops::RangeInclusive<usize> = 1..=6;

/// Fills a drawing area with a color that can change later.
fn color_area(css_name: &str) -> (gtk::DrawingArea, Rc<RefCell<Option<gdk::RGBA>>>) {
    let color = Rc::new(RefCell::new(None::<gdk::RGBA>));
    let area = gtk::DrawingArea::builder().css_name(css_name).build();
    let draw_color = Rc::clone(&color);
    area.set_draw_func(move |_, cr, width, height| {
        if let Some(c) = *draw_color.borrow() {
            cr.set_source_rgba(
                c.red().into(),
                c.green().into(),
                c.blue().into(),
                c.alpha().into(),
            );
            cr.rectangle(0.0, 0.0, width.into(), height.into());
            // Drawing errors only mean the surface is unusable; nothing to do.
            let _ = cr.fill();
        }
    });
    (area, color)
}

/// A single indexed color of a palette (`PtyxisPalettePreviewColor`).
#[derive(Debug, Clone)]
pub struct ColorSwatch {
    area: gtk::DrawingArea,
    color: Rc<RefCell<Option<gdk::RGBA>>>,
    index: usize,
}

impl ColorSwatch {
    pub fn new(index: usize) -> Self {
        let (area, color) = color_area(COLOR_CSS_NAME);
        area.set_hexpand(true);
        area.set_has_tooltip(true);
        let tooltip_color = Rc::clone(&color);
        area.connect_query_tooltip(move |_, _, _, _, tooltip| {
            let Some(color) = *tooltip_color.borrow() else {
                return false;
            };
            tooltip.set_text(Some(&color.to_str()));
            true
        });
        Self { area, color, index }
    }

    pub fn widget(&self) -> &gtk::DrawingArea {
        &self.area
    }

    pub fn set_palette(&self, palette: Option<&Palette>, dark: bool) {
        *self.color.borrow_mut() =
            palette.and_then(|p| p.face(dark).indexed.get(self.index).copied());
        self.area.queue_draw();
    }
}

/// A palette card: name, sample text and a few swatches on the palette
/// background (`PtyxisPalettePreview`).
#[derive(Debug, Clone)]
pub struct PalettePreview {
    root: gtk::Overlay,
    background: Rc<RefCell<Option<gdk::RGBA>>>,
    background_area: gtk::DrawingArea,
    label: gtk::Label,
    swatches: Vec<ColorSwatch>,
    palette: Rc<Palette>,
    font: Rc<RefCell<Option<pango::FontDescription>>>,
}

impl PalettePreview {
    pub fn new(palette: &Rc<Palette>, dark: bool, font: Option<&pango::FontDescription>) -> Self {
        let (background_area, background) = color_area(PREVIEW_CSS_NAME);
        let root = gtk::Overlay::builder()
            .css_name(PREVIEW_CSS_NAME)
            .child(&background_area)
            .has_tooltip(true)
            .build();

        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let title_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        title_row.append(
            &gtk::Inscription::builder()
                .text(palette.name())
                .text_overflow(gtk::InscriptionOverflow::EllipsizeEnd)
                .hexpand(true)
                // The C template also sets wrap-mode=none (Pango 1.56); a
                // single-line inscription already never wraps.
                .nat_lines(1)
                .build(),
        );
        title_row.append(
            &gtk::Image::builder()
                .icon_name("object-select-symbolic")
                .valign(gtk::Align::Start)
                .build(),
        );
        content.append(&title_row);

        let label = gtk::Label::builder()
            .label(SAMPLE_TEXT)
            .wrap(true)
            .width_chars(6)
            .max_width_chars(6)
            .xalign(0.0)
            .hexpand(true)
            .lines(3)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        content.append(&label);

        let swatch_box = gtk::Box::builder().spacing(3).hexpand(true).build();
        let swatches: Vec<ColorSwatch> = PREVIEW_COLOR_INDEXES.map(ColorSwatch::new).collect();
        for swatch in &swatches {
            swatch_box.append(swatch.widget());
        }
        content.append(&swatch_box);
        root.add_overlay(&content);
        // The background must size to the content, not the other way round.
        root.set_measure_overlay(&content, true);

        let name = palette.name().to_owned();
        root.connect_query_tooltip(move |_, _, _, _, tooltip| {
            tooltip.set_text(Some(&name));
            true
        });

        let preview = Self {
            root,
            background,
            background_area,
            label,
            swatches,
            palette: Rc::clone(palette),
            font: Rc::new(RefCell::new(font.cloned())),
        };
        preview.set_dark(dark);
        preview
    }

    pub fn widget(&self) -> &gtk::Overlay {
        &self.root
    }

    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    pub fn set_dark(&self, dark: bool) {
        let face = self.palette.face(dark);
        *self.background.borrow_mut() = Some(face.background);
        self.background_area.queue_draw();
        for swatch in &self.swatches {
            swatch.set_palette(Some(&self.palette), dark);
        }
        self.update_label(dark);
    }

    fn update_label(&self, dark: bool) {
        let attrs = pango::AttrList::new();
        if let Some(font) = self.font.borrow().as_ref() {
            attrs.insert(pango::AttrFontDesc::new(font));
        }
        let fg = self.palette.face(dark).foreground;
        let channel = |c: f32| (f64::from(c) * 65535.0).round() as u16;
        attrs.insert(pango::AttrColor::new_foreground(
            channel(fg.red()),
            channel(fg.green()),
            channel(fg.blue()),
        ));
        self.label.set_attributes(Some(&attrs));
    }

    /// Marks the preview's container (the palette button) as selected,
    /// which `style.css` uses to highlight it.
    pub fn set_selected(&self, selected: bool) {
        if let Some(parent) = self.root.parent() {
            if selected {
                parent.add_css_class(SELECTED_CSS_CLASS);
            } else {
                parent.remove_css_class(SELECTED_CSS_CLASS);
            }
        }
    }
}
