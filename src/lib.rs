// lib.rs
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

//! tty-rex, a Rust port of the Ptyxis terminal.
//!
//! # Building and running uninstalled
//!
//! ```sh
//! cd rust
//! cargo run -- --help
//! ```
//!
//! `build.rs` generates the GSettings schema from
//! `ptyxis/org.gnome.Ptyxis.gschema.xml.in` and compiles it into Cargo's
//! `OUT_DIR`. The binary prefers that compiled schema automatically unless
//! `GSETTINGS_SCHEMA_DIR` is set, so no installation step is needed. The
//! schema id is the stable `org.gnome.Ptyxis` so existing profiles and
//! shortcuts are shared with Ptyxis, while the application id is
//! `io.github.tasleson.TtyRex` so the two never hand launches to each other.

pub mod app;
pub mod application;
pub mod choices;
pub mod cli;
pub mod close_dialog;
pub mod color_dialog;
pub mod config;
pub mod container;
pub mod dressing;
pub mod find_bar;
pub mod inspector;
pub mod leader;
pub mod palette;
pub mod palette_preview;
pub mod parking_lot;
pub mod preferences;
pub mod profile;
pub mod proxy;
pub mod session;
pub mod settings;
pub mod shortcuts;
pub mod spawn;
pub mod tab;
pub mod tab_notify;
pub mod telemetry;
pub mod telemetry_bar;
pub mod terminal;
pub mod terminal_intent;
pub mod theme_selector;
pub mod title_dialog;
pub mod window;
