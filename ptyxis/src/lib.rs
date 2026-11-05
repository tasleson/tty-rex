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

//! Rust port of the Ptyxis terminal.
//!
//! # Building and running uninstalled
//!
//! ```sh
//! cd rust
//! cargo run -- --help
//! ```
//!
//! `build.rs` generates the GSettings schema from
//! `src/org.gnome.Ptyxis.gschema.xml.in` and compiles it into Cargo's
//! `OUT_DIR`. The binary prefers that compiled schema automatically unless
//! `GSETTINGS_SCHEMA_DIR` is set, so no installation step is needed. The
//! schema id is the stable `org.gnome.Ptyxis` so existing profiles and
//! shortcuts are used, while the application id defaults to
//! `org.gnome.Ptyxis.Devel` to avoid talking to an installed instance.

pub mod app;
pub mod cli;
pub mod config;
pub mod find_bar;
pub mod palette;
pub mod profile;
pub mod settings;
pub mod shortcuts;
pub mod spawn;
pub mod tab;
pub mod terminal;
pub mod window;
