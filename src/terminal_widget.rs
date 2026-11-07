// terminal_widget.rs
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

//! The `VteTerminal` subclass tabs use, for the parts of `PtyxisTerminal`
//! (`src/ptyxis-terminal.c`) that need a subclass: the snapshot override
//! and the hover-URI revealer with its `measure`/`size_allocate` overrides.
//!
//! The size revealer and the drop highlight have no counterpart in tty-rex
//! yet.

use std::cell::{Cell, OnceCell};

use gtk::prelude::*;
use gtk::{glib, graphene, gsk, pango};
use vte::prelude::*;
use vte::subclass::prelude::*;

/// How far the replacement clip reaches past the left and right edges,
/// as in `ptyxis_terminal_snapshot()`.
const CLIP_SIDE_SLACK: f32 = 2.0;

/// Style class `style.css` and the window dressing give the hover-URI
/// revealer, as in `ptyxis-terminal.ui`.
const HOVER_URI_CSS_CLASS: &str = "hover-uri";

/// Width limit of the hover-URI label, from `ptyxis-terminal.ui`.
const HOVER_URI_MAX_WIDTH_CHARS: i32 = 60;

/// One side each of a CSS padding or margin, like `GtkBorder` (whose Rust
/// binding needs GTK initialized, which unit tests do not have).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Sides {
    top: i32,
    right: i32,
    bottom: i32,
    left: i32,
}

impl From<gtk::Border> for Sides {
    fn from(border: gtk::Border) -> Self {
        Self {
            top: border.top().into(),
            right: border.right().into(),
            bottom: border.bottom().into(),
            left: border.left().into(),
        }
    }
}

/// The terminal's CSS box: its size, padding and margin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TerminalBox {
    width: i32,
    height: i32,
    padding: Sides,
    margin: Sides,
}

/// Where the hover-URI revealer goes, as in
/// `ptyxis_terminal_size_allocate()`: at the bottom left, spanning the
/// padding, unless the pointer (`pointer_y`, when in the widget) is over
/// that spot, in which case it moves to the top so it does not cover what
/// the user points at. The revealer gets its natural width and minimum
/// height.
fn hover_uri_allocation(
    terminal: &TerminalBox,
    natural_width: i32,
    minimum_height: i32,
    pointer_y: Option<f64>,
) -> gtk::Allocation {
    let padding = &terminal.padding;
    let margin = &terminal.margin;
    let width = natural_width.min(terminal.width + padding.left + padding.right);
    let height = minimum_height;
    let bottom_y = terminal.height + padding.bottom + margin.bottom - height;
    let needs_top =
        pointer_y.is_some_and(|y| y >= f64::from(bottom_y) && y <= f64::from(bottom_y + height));
    let y = if needs_top {
        margin.top - padding.top
    } else {
        bottom_y
    };
    gtk::Allocation::new(-padding.left, y, width, height)
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct TerminalWidget {
        hover_uri_revealer: OnceCell<gtk::Revealer>,
        hover_uri_label: OnceCell<gtk::Label>,
        /// Pointer position while it is over the terminal, tracked so the
        /// hover-URI revealer can get out of its way.
        pointer_in_widget: Cell<bool>,
        pointer_y: Cell<f64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TerminalWidget {
        const NAME: &'static str = "TtyRexTerminal";
        type Type = super::TerminalWidget;
        type ParentType = vte::Terminal;

        fn class_init(klass: &mut Self::Class) {
            // vte4-rs installs a `commit` trampoline that wraps VTE's text
            // in an owning `GString`, so dropping it frees VTE's buffer: the
            // first commit (any keystroke, since tabs connect to `commit`)
            // aborts with a double free. Nothing here overrides `commit`,
            // so put VTE's own slot back.
            // SAFETY: `type_data()` holds the parent class pointer, set by
            // glib before this runs, and that class is a `VteTerminalClass`.
            let parent = unsafe {
                &*(Self::type_data().as_ref().parent_class() as *const vte::ffi::VteTerminalClass)
            };
            let klass: &mut glib::Class<vte::Terminal> = klass.upcast_ref_mut();
            klass.as_mut().commit = parent.commit;
        }
    }

    impl ObjectImpl for TerminalWidget {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            // The hover-URI revealer from `ptyxis-terminal.ui`.
            let label = gtk::Label::builder()
                .ellipsize(pango::EllipsizeMode::End)
                .max_width_chars(HOVER_URI_MAX_WIDTH_CHARS)
                .single_line_mode(true)
                .xalign(0.0)
                .build();
            let revealer = gtk::Revealer::builder()
                .transition_type(gtk::RevealerTransitionType::Crossfade)
                .reveal_child(false)
                .css_classes([HOVER_URI_CSS_CLASS])
                .halign(gtk::Align::Start)
                .valign(gtk::Align::End)
                .child(&label)
                .build();
            revealer.set_parent(&*obj);
            self.hover_uri_label
                .set(label)
                .expect("constructed runs once");
            self.hover_uri_revealer
                .set(revealer)
                .expect("constructed runs once");

            // `motion_controller` from `ptyxis-terminal.ui`.
            let motion = gtk::EventControllerMotion::new();
            motion.set_propagation_phase(gtk::PropagationPhase::Capture);
            motion.connect_enter(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_, _, y| this.pointer_motion(y)
            ));
            motion.connect_motion(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_, _, y| this.pointer_motion(y)
            ));
            motion.connect_leave(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_| this.pointer_leave()
            ));
            obj.add_controller(motion);

            obj.connect_hyperlink_hover_uri_notify(|obj| obj.imp().update_hover_uri());
            self.update_hover_uri();
        }

        fn dispose(&self) {
            // `gtk_widget_dispose_template()` in `ptyxis_terminal_dispose()`.
            if let Some(revealer) = self.hover_uri_revealer.get() {
                revealer.unparent();
            }
        }
    }

    impl WidgetImpl for TerminalWidget {
        /// `ptyxis_terminal_measure()`: at least as big as the revealer.
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let (minimum, natural, minimum_baseline, natural_baseline) =
                self.parent_measure(orientation, for_size);
            let (revealer_minimum, revealer_natural, _, _) =
                self.hover_uri_revealer().measure(orientation, for_size);
            (
                minimum.max(revealer_minimum),
                natural.max(revealer_natural),
                minimum_baseline,
                natural_baseline,
            )
        }

        /// `ptyxis_terminal_size_allocate()`, for the hover-URI revealer.
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);

            let widget = self.obj();
            // As in `snapshot`, the C code reads these the deprecated way.
            #[allow(deprecated)]
            let (padding, margin) = {
                let style = widget.style_context();
                (style.padding(), style.margin())
            };
            let terminal = TerminalBox {
                width,
                height,
                padding: padding.into(),
                margin: margin.into(),
            };
            let revealer = self.hover_uri_revealer();
            let (min, nat) = revealer.preferred_size();
            let pointer_y = self.pointer_in_widget.get().then(|| self.pointer_y.get());
            revealer.size_allocate(
                &hover_uri_allocation(&terminal, nat.width(), min.height(), pointer_y),
                -1,
            );
        }

        /// `ptyxis_terminal_snapshot()`: lets rows reach into the top and
        /// bottom padding, but no further.
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            // The C code reads the padding the same deprecated way; GTK has
            // no other getter for it.
            #[allow(deprecated)]
            let padding = widget.style_context().padding();
            let top = f32::from(padding.top());
            let bottom = f32::from(padding.bottom());
            snapshot.push_clip(&graphene::Rect::new(
                -CLIP_SIDE_SLACK,
                -top,
                widget.width() as f32 + 2.0 * CLIP_SIDE_SLACK,
                top + widget.height() as f32 + bottom,
            ));
            self.rewrite_snapshot(snapshot);
            snapshot.pop();

            widget.snapshot_child(self.hover_uri_revealer(), snapshot);
        }
    }

    impl TerminalImpl for TerminalWidget {}

    impl TerminalWidget {
        fn hover_uri_revealer(&self) -> &gtk::Revealer {
            self.hover_uri_revealer
                .get()
                .expect("revealer is created in constructed")
        }

        /// `ptyxis_terminal_update_hover_uri()`: shows the link under the
        /// pointer, or hides the revealer when there is none.
        fn update_hover_uri(&self) {
            let revealer = self.hover_uri_revealer();
            match self
                .obj()
                .hyperlink_hover_uri()
                .filter(|uri| !uri.is_empty())
            {
                Some(uri) => {
                    if let Some(label) = self.hover_uri_label.get() {
                        label.set_label(&uri);
                    }
                    revealer.set_reveal_child(true);
                    self.obj().queue_allocate();
                }
                None => revealer.set_reveal_child(false),
            }
        }

        /// `ptyxis_terminal_motion_cb()` (and `_enter_cb`).
        fn pointer_motion(&self, y: f64) {
            self.pointer_in_widget.set(true);
            self.pointer_y.set(y);
            self.reallocate_if_hover_uri_shown();
        }

        /// `ptyxis_terminal_motion_leave_cb()`.
        fn pointer_leave(&self) {
            self.pointer_in_widget.set(false);
            self.reallocate_if_hover_uri_shown();
        }

        /// The revealer may need to swap between bottom and top.
        fn reallocate_if_hover_uri_shown(&self) {
            if self.hover_uri_revealer().reveals_child() {
                self.obj().queue_allocate();
            }
        }

        /// `ptyxis_terminal_rewrite_snapshot()`: snapshots VTE, then drops
        /// its background color node (the window paints the background,
        /// see `dressing`) and unwraps the clip VTE puts around the rows
        /// when the view is scrolled by a partial row, so the clip from
        /// [`Self::snapshot`] applies instead.
        fn rewrite_snapshot(&self, snapshot: &gtk::Snapshot) {
            let alternate = gtk::Snapshot::new();
            self.parent_snapshot(&alternate);
            let Some(root) = alternate.to_node() else {
                return;
            };

            let root = match root.node_type() {
                gsk::RenderNodeType::ContainerNode => {
                    let container = root
                        .downcast_ref::<gsk::ContainerNode>()
                        .expect("node type is ContainerNode");
                    let children = rewrite_children(container);
                    if children.is_empty() {
                        root
                    } else {
                        gsk::ContainerNode::new(&children).upcast()
                    }
                }
                // A lone color node is a cursor blink frame with no cursor
                // drawn: only the background, which is dropped as above.
                gsk::RenderNodeType::ColorNode => return,
                _ => root,
            };
            snapshot.append_node(root);
        }
    }

    /// Children of VTE's root container minus the first color node, with
    /// clip nodes replaced by their child.
    fn rewrite_children(container: &gsk::ContainerNode) -> Vec<gsk::RenderNode> {
        let mut dropped_background = false;
        let mut children = Vec::new();
        for i in 0..container.n_children() {
            let node = container.child(i);
            match node.node_type() {
                gsk::RenderNodeType::ColorNode if !dropped_background => {
                    dropped_background = true;
                }
                gsk::RenderNodeType::ClipNode => {
                    let clip = node
                        .downcast_ref::<gsk::ClipNode>()
                        .expect("node type is ClipNode");
                    children.push(clip.child());
                }
                _ => children.push(node),
            }
        }
        children
    }
}

glib::wrapper! {
    /// A `vte::Terminal` that draws partially scrolled rows into its
    /// padding and shows the hyperlink under the pointer
    /// (`PtyxisTerminal`).
    pub struct TerminalWidget(ObjectSubclass<imp::TerminalWidget>)
        @extends vte::Terminal, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Scrollable;
}

impl TerminalWidget {
    pub fn new() -> Self {
        glib::Object::new()
    }
}

impl Default for TerminalWidget {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hover_uri_allocation_follows_pointer() {
        // A padded terminal like `style.css`'s `vte-terminal.padded`.
        let terminal = TerminalBox {
            width: 600,
            height: 400,
            padding: Sides {
                top: 7,
                right: 6,
                bottom: 3,
                left: 6,
            },
            margin: Sides {
                bottom: 1,
                ..Sides::default()
            },
        };
        // Bottom: 400 + 3 + 1 - 20; top: 0 - 7.
        const BOTTOM: i32 = 384;
        const TOP: i32 = -7;
        for (natural_width, pointer_y, expected) in [
            (300, None, (300, BOTTOM)),
            (300, Some(10.0), (300, BOTTOM)),
            (300, Some(383.9), (300, BOTTOM)),
            (300, Some(384.0), (300, TOP)),
            (300, Some(404.0), (300, TOP)),
            (300, Some(404.1), (300, BOTTOM)),
            // Never wider than the terminal and its side padding.
            (1000, None, (612, BOTTOM)),
        ] {
            let alloc = hover_uri_allocation(&terminal, natural_width, 20, pointer_y);
            assert_eq!(
                (alloc.x(), alloc.y(), alloc.width(), alloc.height()),
                (-6, expected.1, expected.0, 20),
                "pointer at {pointer_y:?}"
            );
        }
    }
}
