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

//! The `VteTerminal` subclass tabs use, for the snapshot override of
//! `PtyxisTerminal` (`src/ptyxis-terminal.c`).
//!
//! The rest of `PtyxisTerminal` (the size and hover-URI revealers, the drop
//! highlight and the matching `measure`/`size_allocate` overrides) has no
//! counterpart in tty-rex yet.

use gtk::prelude::*;
use gtk::{glib, graphene, gsk};
use vte::subclass::prelude::*;

/// How far the replacement clip reaches past the left and right edges,
/// as in `ptyxis_terminal_snapshot()`.
const CLIP_SIDE_SLACK: f32 = 2.0;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct TerminalWidget;

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

    impl ObjectImpl for TerminalWidget {}

    impl WidgetImpl for TerminalWidget {
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
        }
    }

    impl TerminalImpl for TerminalWidget {}

    impl TerminalWidget {
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
    /// padding (`PtyxisTerminal`).
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
