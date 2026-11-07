// telemetry_bar.rs
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

//! The telemetry footer: network, CPU and memory readings for the
//! processes of the selected tab (see [`crate::telemetry`]). A tty-rex
//! addition with no Ptyxis counterpart.
//!
//! The window owns one footer and tells it which tab is selected and
//! whether it is shown. It samples only while shown, every
//! `telemetry-interval` seconds, on a worker thread. At most one sample
//! runs at a time: a tick arriving while one is still running is skipped.

use std::cell::Cell;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use relm4::prelude::*;

use crate::settings::{AppSettings, tty_rex_key};
use crate::telemetry::{PLACEHOLDER, Pid, ReadingKind, Readings, Sampler};

/// Space between readings, and between a reading's label and value.
const READING_SPACING: i32 = 18;
const LABEL_SPACING: i32 = 6;
const VERTICAL_MARGIN: i32 = 3;
const HORIZONTAL_MARGIN: i32 = 12;
/// libadwaita style classes: small text, muted labels, tabular figures.
const CAPTION_CLASS: &str = "caption";
const DIM_CLASS: &str = "dim-label";
const NUMERIC_CLASS: &str = "numeric";

/// Where the footer reads the selected tab's child pid.
pub type PidSource = Rc<Cell<Option<Pid>>>;

#[derive(Debug)]
pub enum TelemetryBarInput {
    SetVisible(bool),
    /// The selected tab changed; `None` when there is none.
    SetTab(Option<PidSource>),
    Tick,
    IntervalChanged,
}

/// A finished sample, handing the sampler back with its new baseline.
#[derive(Debug)]
pub struct SampleDone {
    generation: u64,
    sampler: Sampler,
    readings: Readings,
}

pub struct TelemetryBar {
    settings: AppSettings,
    visible: bool,
    pid_source: Option<PidSource>,
    /// `None` while a sample runs on the worker, which owns it meanwhile.
    sampler: Option<Sampler>,
    /// Bumped whenever the sampled tab or visibility changes, so a sample
    /// started before is discarded along with its baseline.
    generation: u64,
    timer: Option<glib::SourceId>,
    /// Each reading with its label and the text the label shows, so an
    /// unchanged value does not touch the widget.
    values: Vec<(ReadingKind, gtk::Label, String)>,
    settings_handler: Option<glib::SignalHandlerId>,
}

/// Shows `text` in `label` unless it already does.
fn set_text(label: &gtk::Label, shown: &mut String, text: String) {
    if *shown != text {
        label.set_label(&text);
        *shown = text;
    }
}

impl TelemetryBar {
    fn show(&mut self, readings: &Readings) {
        for (kind, label, shown) in &mut self.values {
            set_text(label, shown, readings.text(*kind));
        }
    }

    fn clear(&mut self) {
        for (_, label, shown) in &mut self.values {
            set_text(label, shown, PLACEHOLDER.to_owned());
        }
    }

    fn stop_timer(&mut self) {
        if let Some(timer) = self.timer.take() {
            timer.remove();
        }
    }

    fn start_timer(&mut self, sender: &ComponentSender<Self>) {
        self.stop_timer();
        let input = sender.input_sender().clone();
        self.timer = Some(glib::timeout_add_local(
            self.settings.telemetry_interval(),
            move || {
                input.emit(TelemetryBarInput::Tick);
                glib::ControlFlow::Continue
            },
        ));
    }

    /// Drops the baseline; the next sample starts a new one.
    fn restart(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if let Some(sampler) = &mut self.sampler {
            sampler.reset();
        }
        self.clear();
    }

    fn tick(&mut self, sender: &ComponentSender<Self>) {
        if !self.visible {
            return;
        }
        // A sample is still running.
        let Some(mut sampler) = self.sampler.take() else {
            return;
        };
        let Some(pid) = self.pid_source.as_ref().and_then(|source| source.get()) else {
            // Nothing running in the tab (yet, or any more).
            sampler.reset();
            self.sampler = Some(sampler);
            self.clear();
            return;
        };
        let generation = self.generation;
        sender.spawn_oneshot_command(move || {
            let readings = sampler.sample(pid);
            SampleDone {
                generation,
                sampler,
                readings,
            }
        });
    }
}

impl Component for TelemetryBar {
    type Init = AppSettings;
    type Input = TelemetryBarInput;
    type Output = ();
    type CommandOutput = SampleDone;
    type Root = gtk::Box;
    type Widgets = ();

    fn init_root() -> Self::Root {
        gtk::Box::new(gtk::Orientation::Vertical, 0)
    }

    fn init(
        settings: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let readings = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(READING_SPACING)
            .margin_top(VERTICAL_MARGIN)
            .margin_bottom(VERTICAL_MARGIN)
            .margin_start(HORIZONTAL_MARGIN)
            .margin_end(HORIZONTAL_MARGIN)
            .css_classes([CAPTION_CLASS])
            .build();
        let values = ReadingKind::ALL
            .into_iter()
            .map(|kind| {
                let reading = gtk::Box::builder()
                    .spacing(LABEL_SPACING)
                    .tooltip_text(kind.tooltip())
                    .build();
                reading.append(
                    &gtk::Label::builder()
                        .label(kind.label())
                        .css_classes([DIM_CLASS])
                        .build(),
                );
                let value = gtk::Label::builder()
                    .label(PLACEHOLDER)
                    .width_chars(kind.width_chars())
                    .xalign(0.0)
                    .css_classes([NUMERIC_CLASS])
                    .build();
                reading.append(&value);
                readings.append(&reading);
                (kind, value, PLACEHOLDER.to_owned())
            })
            .collect();
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&readings);

        let input = sender.input_sender().clone();
        let settings_handler = settings
            .tty_rex_gsettings()
            .connect_changed(Some(tty_rex_key::TELEMETRY_INTERVAL), move |_, _| {
                input.emit(TelemetryBarInput::IntervalChanged)
            });

        let model = TelemetryBar {
            settings,
            visible: false,
            pid_source: None,
            sampler: Some(Sampler::new()),
            generation: 0,
            timer: None,
            values,
            settings_handler: Some(settings_handler),
        };
        ComponentParts { model, widgets: () }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match message {
            TelemetryBarInput::SetVisible(visible) => {
                if visible == self.visible {
                    return;
                }
                self.visible = visible;
                self.restart();
                if visible {
                    self.start_timer(&sender);
                    self.tick(&sender);
                } else {
                    self.stop_timer();
                }
            }
            TelemetryBarInput::SetTab(source) => {
                let same = match (&self.pid_source, &source) {
                    (Some(a), Some(b)) => Rc::ptr_eq(a, b),
                    (None, None) => true,
                    _ => false,
                };
                if same {
                    return;
                }
                self.pid_source = source;
                self.restart();
                self.tick(&sender);
            }
            TelemetryBarInput::Tick => self.tick(&sender),
            TelemetryBarInput::IntervalChanged => {
                if self.timer.is_some() {
                    self.start_timer(&sender);
                }
            }
        }
    }

    fn update_cmd(
        &mut self,
        done: Self::CommandOutput,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        if done.generation == self.generation {
            self.sampler = Some(done.sampler);
            self.show(&done.readings);
        } else {
            // Its baseline belongs to another tab or an earlier showing;
            // start the current one's right away rather than a tick late.
            let mut sampler = done.sampler;
            sampler.reset();
            self.sampler = Some(sampler);
            self.tick(&sender);
        }
    }

    fn shutdown(&mut self, _widgets: &mut Self::Widgets, _output: relm4::Sender<Self::Output>) {
        self.stop_timer();
        if let Some(id) = self.settings_handler.take() {
            self.settings.tty_rex_gsettings().disconnect(id);
        }
    }
}
