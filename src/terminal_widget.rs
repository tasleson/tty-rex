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
//! (`src/ptyxis-terminal.c`) that need a subclass: the snapshot override,
//! the size and hover-URI revealers with their `measure`/`size_allocate`
//! overrides, and the drop target with its highlight, which pastes dropped
//! files, URIs and text.

use std::cell::{Cell, OnceCell, RefCell};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, gio, glib, graphene, gsk, pango};
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

/// Style class `style.css` and the window dressing give the size revealer,
/// as in `ptyxis-terminal.ui`.
const SIZE_CSS_CLASS: &str = "size";

/// Font family and features of the size label, from `ptyxis-terminal.ui`:
/// tabular figures keep the label from jittering while it counts.
const SIZE_LABEL_FAMILY: &str = "Sans";
const SIZE_LABEL_FONT_FEATURES: &str = "tnum";

/// How long the size label stays up after the last resize
/// (`SIZE_DISMISS_TIMEOUT_MSEC`).
const SIZE_DISMISS_TIMEOUT: Duration = Duration::from_millis(1000);

/// Style class of the drop highlight, as in `ptyxis-terminal.ui`.
const DROP_HIGHLIGHT_CSS_CLASS: &str = "drop-highlight";

/// Priority of the drop data reads (`DROP_REQUEST_PRIORITY`).
const DROP_REQUEST_PRIORITY: glib::Priority = glib::Priority::DEFAULT;
const APPLICATION_VND_PORTAL_FILETRANSFER: &str = "application/vnd.portal.filetransfer";
const APPLICATION_VND_PORTAL_FILES: &str = "application/vnd.portal.files";
const TEXT_X_MOZ_URL: &str = "text/x-moz-url";
const TEXT_URI_LIST: &str = "text/uri-list";

/// Where the Document Portal records the real path of a file it exports.
const FILE_ATTRIBUTE_HOST_PATH: &str = "xattr::document-portal.host-path";

/// Suffix early Document Portal versions appended to the host path.
const HOST_PATH_NUL_SUFFIX: &str = "\\x00";

/// `ptyxis_terminal_get_drop_action()`: copy if the source allows it, else
/// move, else nothing.
fn drop_action(actions: gdk::DragAction) -> gdk::DragAction {
    if actions.contains(gdk::DragAction::COPY) {
        gdk::DragAction::COPY
    } else if actions.contains(gdk::DragAction::MOVE) {
        gdk::DragAction::MOVE
    } else {
        gdk::DragAction::empty()
    }
}

/// How a drop is read, by the first format it offers in
/// `ptyxis_terminal_drop_target_drop()`'s order of preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DropRequest {
    /// Files, as a `GdkFileList` (falling back to `text/uri-list`).
    FileList,
    /// A link dragged from a browser: URL, newline, page title, in UCS-2.
    MozUrl,
    /// Plain text.
    String,
}

impl DropRequest {
    fn for_formats(formats: &gdk::ContentFormats) -> Option<Self> {
        if formats.contains_type(gdk::FileList::static_type())
            || formats.contains_type(gio::File::static_type())
            || [
                TEXT_URI_LIST,
                APPLICATION_VND_PORTAL_FILETRANSFER,
                APPLICATION_VND_PORTAL_FILES,
            ]
            .into_iter()
            .any(|mime_type| formats.contain_mime_type(mime_type))
        {
            Some(Self::FileList)
        } else if formats.contain_mime_type(TEXT_X_MOZ_URL) {
            Some(Self::MozUrl)
        } else if formats.contains_type(glib::Type::STRING) {
            Some(Self::String)
        } else {
            None
        }
    }
}

/// `file_is_from_document_portal()`: whether `path` lies under the
/// Document Portal's FUSE mount in `runtime_dir`.
fn is_document_portal_path(path: &Path, runtime_dir: &Path) -> bool {
    let mut docportal = runtime_dir.as_os_str().to_owned();
    docportal.push("/doc/");
    path.as_os_str()
        .as_bytes()
        .starts_with(docportal.as_bytes())
}

/// Drops the `\x00` suffix early Document Portal versions put on host
/// paths, as `ptyxis_terminal_get_portal_host_path()` does.
fn trim_host_path(host_path: &str) -> &str {
    match host_path.strip_suffix(HOST_PATH_NUL_SUFFIX) {
        Some(trimmed) if host_path.len() > HOST_PATH_NUL_SUFFIX.len() => trimmed,
        _ => host_path,
    }
}

/// A `text/uri-list` line worth turning into a file: not empty and not a
/// comment.
fn uri_list_entry(line: &str) -> Option<&str> {
    (!line.is_empty() && !line.starts_with('#')).then_some(line)
}

/// The text `ptyxis_terminal_drop_file_list()` pastes: each item shell
/// quoted and followed by a space. It is bytes, not a string, because a
/// local path need not be UTF-8 and the shell must get it unchanged.
fn quoted_paste_text<I>(items: I) -> Vec<u8>
where
    I: IntoIterator,
    I::Item: AsRef<OsStr>,
{
    let mut text = Vec::new();
    for item in items {
        text.extend_from_slice(glib::shell_quote(item).as_bytes());
        text.push(b' ');
    }
    text
}

/// `vte_terminal_paste_text()` on raw bytes, as the C code calls it with
/// whatever `g_shell_quote()` returned. vte4-rs only binds it for `&str`,
/// which would turn a non-UTF-8 path into one that does not exist.
fn paste_bytes(terminal: &vte::Terminal, text: Vec<u8>) {
    use glib::translate::ToGlibPtr;
    match std::ffi::CString::new(text) {
        // SAFETY: `terminal` is a live `VteTerminal` and `text` a
        // NUL-terminated buffer that outlives the call, which copies it.
        Ok(text) => unsafe {
            vte::ffi::vte_terminal_paste_text(terminal.to_glib_none().0, text.as_ptr());
        },
        // Paths and URIs cannot hold a NUL, so this is not expected.
        Err(error) => glib::g_debug!("tty-rex", "Not pasting dropped text: {error}"),
    }
}

/// `ptyxis_terminal_get_portal_host_path()`: the real path behind a
/// Document Portal file, for display.
fn portal_host_path(file: &gio::File) -> Option<glib::GString> {
    let info = file
        .query_info(
            FILE_ATTRIBUTE_HOST_PATH,
            gio::FileQueryInfoFlags::NONE,
            gio::Cancellable::NONE,
        )
        .ok()?;
    let host_path = info.attribute_string(FILE_ATTRIBUTE_HOST_PATH)?;
    Some(glib::filename_display_name(trim_host_path(&host_path)))
}

/// What to paste for a dropped file, as in
/// `ptyxis_terminal_drop_file_list()`: the host path of a Document Portal
/// file (whose portal path means nothing to the shell), else a local
/// path, else the URI.
fn file_paste_item(file: &gio::File) -> std::ffi::OsString {
    match file.path().filter(|_| file.is_native()) {
        Some(path) => {
            if is_document_portal_path(&path, &glib::user_runtime_dir())
                && let Some(host_path) = portal_host_path(file)
            {
                return host_path.as_str().into();
            }
            path.into_os_string()
        }
        None => file.uri().as_str().into(),
    }
}

/// Where the drop highlight goes, as in `ptyxis_terminal_size_allocate()`:
/// over the terminal and its side padding, inset by a pixel.
fn drop_highlight_allocation(terminal: &TerminalBox) -> gtk::Allocation {
    let padding = &terminal.padding;
    gtk::Allocation::new(
        -padding.left + 1,
        1,
        padding.left - 1 + terminal.width + padding.right - 1,
        terminal.height - 2,
    )
}

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

/// The size label text, as `ptyxis_terminal_size_allocate()` formats it.
fn size_label_text(columns: i64, rows: i64) -> String {
    format!("{columns} × {rows}")
}

/// What an allocation does to the size revealer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SizeLabelAction {
    /// Show the new grid size and restart the dismiss timeout.
    Reveal,
    /// Hide it at once.
    Dismiss,
    /// Leave it as it is.
    Keep,
}

impl SizeLabelAction {
    /// The choice in `ptyxis_terminal_size_allocate()`: a grid size change
    /// with `show-window-size` on shows the label on the active terminal of
    /// a window that is neither maximized nor fullscreen (whose size the
    /// user is not dragging), and such a window hides it whatever changed.
    /// `window_maximized` is `None` when the terminal is not in a window.
    fn for_allocation(
        grid_size_changed: bool,
        show_window_size: bool,
        active: bool,
        window_maximized: Option<bool>,
    ) -> Self {
        match window_maximized {
            Some(false) if active && grid_size_changed && show_window_size => Self::Reveal,
            Some(true) => Self::Dismiss,
            _ => Self::Keep,
        }
    }
}

/// Where the size revealer goes, as in `ptyxis_terminal_size_allocate()`:
/// at the bottom right, over the right and bottom padding, at its minimum
/// size.
fn size_allocation(
    terminal: &TerminalBox,
    minimum_width: i32,
    minimum_height: i32,
) -> gtk::Allocation {
    gtk::Allocation::new(
        terminal.width - minimum_width + terminal.padding.right,
        terminal.height + terminal.padding.bottom + terminal.margin.bottom - minimum_height,
        minimum_width,
        minimum_height,
    )
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct TerminalWidget {
        size_revealer: OnceCell<gtk::Revealer>,
        size_label: OnceCell<gtk::Label>,
        /// Pending hide of the size revealer (`size_dismiss_source`).
        size_dismiss_source: RefCell<Option<glib::SourceId>>,
        /// Grid size at the last allocation, to spot changes.
        n_columns: Cell<i64>,
        n_rows: Cell<i64>,
        /// The `show-window-size` setting, kept current by the tab.
        show_window_size: Cell<bool>,
        hover_uri_revealer: OnceCell<gtk::Revealer>,
        hover_uri_label: OnceCell<gtk::Label>,
        drop_highlight: OnceCell<adw::Bin>,
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

            // The size revealer from `ptyxis-terminal.ui`.
            let attributes = pango::AttrList::new();
            attributes.insert(pango::AttrString::new_family(SIZE_LABEL_FAMILY));
            attributes.insert(pango::AttrFontFeatures::new(SIZE_LABEL_FONT_FEATURES));
            let label = gtk::Label::builder().attributes(&attributes).build();
            let revealer = gtk::Revealer::builder()
                .transition_type(gtk::RevealerTransitionType::Crossfade)
                .reveal_child(false)
                .css_classes([SIZE_CSS_CLASS])
                .halign(gtk::Align::End)
                .valign(gtk::Align::End)
                .child(&label)
                .build();
            revealer.set_parent(&*obj);
            self.size_label.set(label).expect("constructed runs once");
            self.size_revealer
                .set(revealer)
                .expect("constructed runs once");

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

            // `drop_highlight` and `drop_target` from `ptyxis-terminal.ui`,
            // set up as in `ptyxis_terminal_init()`.
            let highlight = adw::Bin::builder()
                .visible(false)
                .css_classes([DROP_HIGHLIGHT_CSS_CLASS])
                .build();
            highlight.set_parent(&*obj);
            self.drop_highlight
                .set(highlight)
                .expect("constructed runs once");

            let formats = gdk::ContentFormatsBuilder::new()
                .add_type(glib::Type::STRING)
                .add_type(gdk::FileList::static_type())
                .add_mime_type(APPLICATION_VND_PORTAL_FILES)
                .add_mime_type(APPLICATION_VND_PORTAL_FILETRANSFER)
                .add_mime_type(TEXT_URI_LIST)
                .add_mime_type(TEXT_X_MOZ_URL)
                .build();
            let drop_target = gtk::DropTargetAsync::new(
                Some(formats),
                gdk::DragAction::COPY | gdk::DragAction::MOVE,
            );
            drop_target.connect_drag_enter(glib::clone!(
                #[weak(rename_to = this)]
                self,
                #[upgrade_or]
                gdk::DragAction::empty(),
                move |_, drop, _, _| this.drag_enter(drop)
            ));
            drop_target.connect_drag_leave(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_, _| this.drop_highlight().set_visible(false)
            ));
            drop_target.connect_drop(glib::clone!(
                #[weak(rename_to = this)]
                self,
                #[upgrade_or]
                false,
                move |_, drop, _, _| this.drop(drop)
            ));
            obj.add_controller(drop_target);

            obj.connect_hyperlink_hover_uri_notify(|obj| obj.imp().update_hover_uri());
            self.update_hover_uri();
        }

        fn dispose(&self) {
            // `ptyxis_terminal_dispose()`.
            if let Some(source) = self.size_dismiss_source.take() {
                source.remove();
            }
            // `gtk_widget_dispose_template()` there.
            for revealer in [self.size_revealer.get(), self.hover_uri_revealer.get()]
                .into_iter()
                .flatten()
            {
                revealer.unparent();
            }
            if let Some(highlight) = self.drop_highlight.get() {
                highlight.unparent();
            }
        }
    }

    impl WidgetImpl for TerminalWidget {
        /// `ptyxis_terminal_measure()`: at least as big as the revealers.
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let (mut minimum, mut natural, minimum_baseline, natural_baseline) =
                self.parent_measure(orientation, for_size);
            for revealer in [self.size_revealer(), self.hover_uri_revealer()] {
                let (revealer_minimum, revealer_natural, _, _) =
                    revealer.measure(orientation, for_size);
                minimum = minimum.max(revealer_minimum);
                natural = natural.max(revealer_natural);
            }
            (minimum, natural, minimum_baseline, natural_baseline)
        }

        /// `ptyxis_terminal_size_allocate()`: shows the grid size when it
        /// changes and places the revealers and drop highlight.
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            self.update_size_label();

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
            let revealer = self.size_revealer();
            let (min, _) = revealer.preferred_size();
            revealer.size_allocate(&size_allocation(&terminal, min.width(), min.height()), -1);

            let revealer = self.hover_uri_revealer();
            let (min, nat) = revealer.preferred_size();
            let pointer_y = self.pointer_in_widget.get().then(|| self.pointer_y.get());
            revealer.size_allocate(
                &hover_uri_allocation(&terminal, nat.width(), min.height(), pointer_y),
                -1,
            );

            // The C code measures the highlight but allocates it a fixed
            // size; measuring keeps GTK from warning about it.
            let highlight = self.drop_highlight();
            let _ = highlight.preferred_size();
            highlight.size_allocate(&drop_highlight_allocation(&terminal), -1);
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

            widget.snapshot_child(self.size_revealer(), snapshot);
            widget.snapshot_child(self.hover_uri_revealer(), snapshot);
            widget.snapshot_child(self.drop_highlight(), snapshot);
        }
    }

    impl TerminalImpl for TerminalWidget {}

    impl TerminalWidget {
        pub(super) fn set_show_window_size(&self, show: bool) {
            self.show_window_size.set(show);
        }

        fn size_revealer(&self) -> &gtk::Revealer {
            self.size_revealer
                .get()
                .expect("revealer is created in constructed")
        }

        /// `ptyxis_terminal_is_active()`: whether this is the terminal of
        /// its window's selected tab.
        fn is_active(&self) -> bool {
            let obj = self.obj();
            obj.ancestor(adw::TabView::static_type())
                .and_downcast::<adw::TabView>()
                .and_then(|tab_view| tab_view.selected_page())
                .is_some_and(|page| obj.is_ancestor(&page.child()))
        }

        /// The size label part of `ptyxis_terminal_size_allocate()`.
        fn update_size_label(&self) {
            let obj = self.obj();
            let columns = obj.column_count();
            let rows = obj.row_count();
            let grid_size_changed = self.n_columns.get() != columns || self.n_rows.get() != rows;
            self.n_columns.set(columns);
            self.n_rows.set(rows);

            let window_maximized = obj
                .root()
                .and_downcast::<gtk::Window>()
                .map(|window| window.is_maximized() || window.is_fullscreen());
            let action = SizeLabelAction::for_allocation(
                grid_size_changed,
                self.show_window_size.get(),
                self.is_active(),
                window_maximized,
            );
            let revealer = self.size_revealer();
            match action {
                SizeLabelAction::Reveal => {
                    let text = size_label_text(columns, rows);
                    if let Some(label) = self.size_label.get() {
                        label.set_label(&text);
                        label.announce(&text, gtk::AccessibleAnnouncementPriority::Medium);
                    }
                    revealer.set_reveal_child(true);
                    self.cancel_size_dismiss();
                    let source = glib::timeout_add_local_once(
                        SIZE_DISMISS_TIMEOUT,
                        glib::clone!(
                            #[weak(rename_to = this)]
                            self,
                            move || {
                                // `dismiss_size_label_cb()`.
                                this.size_dismiss_source.take();
                                this.size_revealer().set_reveal_child(false);
                            }
                        ),
                    );
                    self.size_dismiss_source.replace(Some(source));
                }
                SizeLabelAction::Dismiss => {
                    self.cancel_size_dismiss();
                    revealer.set_reveal_child(false);
                }
                SizeLabelAction::Keep => {}
            }
        }

        fn cancel_size_dismiss(&self) {
            if let Some(source) = self.size_dismiss_source.take() {
                source.remove();
            }
        }

        fn hover_uri_revealer(&self) -> &gtk::Revealer {
            self.hover_uri_revealer
                .get()
                .expect("revealer is created in constructed")
        }

        fn drop_highlight(&self) -> &adw::Bin {
            self.drop_highlight
                .get()
                .expect("drop highlight is created in constructed")
        }

        /// `ptyxis_terminal_drop_target_drag_enter()`.
        fn drag_enter(&self, drop: &gdk::Drop) -> gdk::DragAction {
            self.drop_highlight().set_visible(true);
            drop_action(drop.actions())
        }

        /// `ptyxis_terminal_drop_target_drop()`: reads the drop in the
        /// background and pastes it, or declines formats it cannot use.
        fn drop(&self, drop: &gdk::Drop) -> bool {
            let Some(request) = DropRequest::for_formats(&drop.formats()) else {
                return false;
            };
            let obj = self.obj().clone();
            let drop = drop.clone();
            glib::spawn_future_local(async move {
                let pasted = match request {
                    DropRequest::FileList => receive_file_list(&obj, &drop).await,
                    DropRequest::MozUrl => receive_moz_url(&obj, &drop).await,
                    DropRequest::String => receive_string(&obj, &drop).await,
                };
                drop.finish(if pasted {
                    drop_action(drop.actions())
                } else {
                    gdk::DragAction::empty()
                });
            });
            true
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

    /// `ptyxis_terminal_drop_file_list()`: pastes the files, quoted.
    fn paste_files(terminal: &super::TerminalWidget, files: &[gio::File]) {
        let text = quoted_paste_text(files.iter().map(file_paste_item));
        if !text.is_empty() {
            paste_bytes(terminal.upcast_ref(), text);
        }
    }

    /// `ptyxis_terminal_drop_file_list_cb()`. Returns whether the drop
    /// succeeded.
    async fn receive_file_list(terminal: &super::TerminalWidget, drop: &gdk::Drop) -> bool {
        let error = match drop
            .read_value_future(gdk::FileList::static_type(), DROP_REQUEST_PRIORITY)
            .await
        {
            Ok(value) => match value.get::<gdk::FileList>() {
                Ok(file_list) => {
                    paste_files(terminal, &file_list.files());
                    return true;
                }
                Err(error) => {
                    glib::g_debug!("tty-rex", "Unexpected file-list offer: {error}");
                    return false;
                }
            },
            Err(error) => error,
        };
        glib::g_debug!("tty-rex", "Failed to receive file-list offer: {error}");

        // If the user dragged a directory from Nautilus or another
        // new-style application, a portal request would be made. But GTK
        // won't be able to open the directory so the request for
        // APPLICATION_VND_PORTAL_FILETRANSFER will fail. Fall back to
        // reading TEXT_URI_LIST gracefully.
        //
        // The C code checks `G_DBUS_ERROR_ACCESS_DENIED` against the
        // `G_IO_ERROR` domain, which really matches
        // `G_IO_ERROR_FILENAME_TOO_LONG` (the same number); kept as is.
        use glib::translate::IntoGlib;
        let falls_back = error.kind::<gio::IOErrorEnum>().is_some_and(|kind| {
            kind == gio::IOErrorEnum::NotFound
                || kind.into_glib() == gio::ffi::G_DBUS_ERROR_ACCESS_DENIED
        });
        falls_back && receive_uri_list(terminal, drop).await
    }

    /// `ptyxis_terminal_drop_uri_list_cb()`.
    async fn receive_uri_list(terminal: &super::TerminalWidget, drop: &gdk::Drop) -> bool {
        let stream = match drop
            .read_future(&[TEXT_URI_LIST], DROP_REQUEST_PRIORITY)
            .await
        {
            Ok((stream, _mime_type)) => stream,
            Err(error) => {
                glib::g_debug!("tty-rex", "Failed to receive text/uri-list offer: {error}");
                return false;
            }
        };
        let line_reader = gio::DataInputStream::new(&stream);
        line_reader.set_newline_type(gio::DataStreamNewlineType::CrLf);
        receive_uri_lines(terminal, &line_reader, TEXT_URI_LIST).await
    }

    /// `ptyxis_terminal_drop_moz_url_cb()`.
    async fn receive_moz_url(terminal: &super::TerminalWidget, drop: &gdk::Drop) -> bool {
        let Ok((stream, _mime_type)) = drop
            .read_future(&[TEXT_X_MOZ_URL], DROP_REQUEST_PRIORITY)
            .await
        else {
            return false;
        };
        let converter = match gio::CharsetConverter::new("UTF-8", "UCS-2") {
            Ok(converter) => converter,
            Err(error) => {
                glib::g_debug!("tty-rex", "Failed to create UTF-8 decoder: {error}");
                return false;
            }
        };
        // TEXT_X_MOZ_URL is in UCS-2 so convert it to UTF-8.
        //
        // The data is expected to be URL, a \n, then the title of the web
        // page. However, some applications (e.g. dolphin) delimit with a
        // \r\n (see Ptyxis issue#293) so handle that generically with the
        // line reader.
        let converter_stream = gio::ConverterInputStream::new(&stream, &converter);
        let line_reader = gio::DataInputStream::new(&converter_stream);
        line_reader.set_newline_type(gio::DataStreamNewlineType::Any);
        receive_uri_lines(terminal, &line_reader, TEXT_X_MOZ_URL).await
    }

    /// `ptyxis_terminal_drop_uri_list_line_cb()`: collects a file per URI
    /// line until the end (only the first line of a `text/x-moz-url`, the
    /// rest being the page title), then pastes them.
    async fn receive_uri_lines(
        terminal: &super::TerminalWidget,
        line_reader: &gio::DataInputStream,
        mime_type: &str,
    ) -> bool {
        let mut files = Vec::new();
        loop {
            let line = match line_reader
                .read_line_utf8_future(DROP_REQUEST_PRIORITY)
                .await
            {
                Ok(line) => line,
                Err(error) => {
                    glib::g_debug!("tty-rex", "Failed to receive '{mime_type}': {error}");
                    return false;
                }
            };
            if let Some(uri) = line.as_deref().and_then(uri_list_entry) {
                files.push(gio::File::for_uri(uri));
            }
            if line.is_none() || mime_type == TEXT_X_MOZ_URL {
                paste_files(terminal, &files);
                return true;
            }
        }
    }

    /// `ptyxis_terminal_drop_string_cb()`.
    async fn receive_string(terminal: &super::TerminalWidget, drop: &gdk::Drop) -> bool {
        let Ok(value) = drop
            .read_value_future(glib::Type::STRING, DROP_REQUEST_PRIORITY)
            .await
        else {
            return false;
        };
        if let Ok(Some(text)) = value.get::<Option<String>>()
            && !text.is_empty()
        {
            terminal.paste_text(&text);
        }
        true
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

    /// Whether resizing shows the grid size (`show-window-size`).
    pub fn set_show_window_size(&self, show: bool) {
        self.imp().set_show_window_size(show);
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

    #[test]
    fn size_allocation_sits_bottom_right() {
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
        let alloc = size_allocation(&terminal, 80, 24);
        // x: 600 - 80 + 6; y: 400 + 3 + 1 - 24.
        assert_eq!(
            (alloc.x(), alloc.y(), alloc.width(), alloc.height()),
            (526, 380, 80, 24)
        );
    }

    #[test]
    fn size_label_text_matches_ptyxis() {
        assert_eq!(size_label_text(80, 24), "80 × 24");
        assert_eq!(size_label_text(132, 7), "132 × 7");
    }

    #[test]
    fn size_label_action() {
        use SizeLabelAction::{Dismiss, Keep, Reveal};
        // (grid size changed, show-window-size, active, window maximized)
        let cases = [
            ((true, true, true, Some(false)), Reveal),
            // Nothing to show.
            ((false, true, true, Some(false)), Keep),
            ((true, false, true, Some(false)), Keep),
            ((true, true, false, Some(false)), Keep),
            // Not in a window yet.
            ((true, true, true, None), Keep),
            // Maximized or fullscreen windows never show it.
            ((true, true, true, Some(true)), Dismiss),
            ((false, false, false, Some(true)), Dismiss),
        ];
        for ((changed, show, active, maximized), expected) in cases {
            assert_eq!(
                SizeLabelAction::for_allocation(changed, show, active, maximized),
                expected,
                "{changed} {show} {active} {maximized:?}"
            );
        }
    }

    #[test]
    fn drop_action_prefers_copy() {
        use gdk::DragAction as A;
        for (actions, expected) in [
            (A::COPY | A::MOVE | A::LINK, A::COPY),
            (A::COPY, A::COPY),
            (A::MOVE | A::LINK, A::MOVE),
            (A::LINK, A::empty()),
            (A::empty(), A::empty()),
        ] {
            assert_eq!(drop_action(actions), expected, "{actions:?}");
        }
    }

    #[test]
    fn drop_request_follows_ptyxis_preference() {
        let file_list = gdk::FileList::static_type();
        let file = gio::File::static_type();
        let string = glib::Type::STRING;
        for (types, mime_types, expected) in [
            (
                vec![string, file_list],
                vec![TEXT_X_MOZ_URL],
                Some(DropRequest::FileList),
            ),
            (vec![file], vec![], Some(DropRequest::FileList)),
            (vec![], vec![TEXT_URI_LIST], Some(DropRequest::FileList)),
            (
                vec![],
                vec![APPLICATION_VND_PORTAL_FILETRANSFER],
                Some(DropRequest::FileList),
            ),
            (
                vec![],
                vec![APPLICATION_VND_PORTAL_FILES],
                Some(DropRequest::FileList),
            ),
            (
                vec![string],
                vec![TEXT_X_MOZ_URL],
                Some(DropRequest::MozUrl),
            ),
            (vec![string], vec!["text/html"], Some(DropRequest::String)),
            (vec![], vec!["image/png"], None),
        ] {
            let builder = types
                .iter()
                .fold(gdk::ContentFormatsBuilder::new(), |b, t| b.add_type(*t));
            let formats = mime_types
                .iter()
                .fold(builder, |b, m| b.add_mime_type(*m))
                .build();
            assert_eq!(
                DropRequest::for_formats(&formats),
                expected,
                "{types:?} {mime_types:?}"
            );
        }
    }

    #[test]
    fn document_portal_paths() {
        let runtime = Path::new("/run/user/1000");
        for (path, expected) in [
            ("/run/user/1000/doc/a1b2c3/file.txt", true),
            ("/run/user/1000/doc/", true),
            ("/run/user/1000/doc", false),
            ("/run/user/1000/docs/file.txt", false),
            ("/run/user/1001/doc/a1b2c3/file.txt", false),
            ("/home/me/file.txt", false),
        ] {
            assert_eq!(
                is_document_portal_path(Path::new(path), runtime),
                expected,
                "{path}"
            );
        }
    }

    #[test]
    fn host_path_suffix_is_trimmed() {
        for (host_path, expected) in [
            ("/home/me/file.txt\\x00", "/home/me/file.txt"),
            ("/home/me/file.txt", "/home/me/file.txt"),
            // Only when something precedes the suffix, as in the C code.
            ("\\x00", "\\x00"),
            ("a\\x00", "a"),
        ] {
            assert_eq!(trim_host_path(host_path), expected, "{host_path}");
        }
    }

    #[test]
    fn uri_list_entries() {
        for (line, expected) in [
            ("file:///tmp/a", Some("file:///tmp/a")),
            ("# comment", None),
            ("", None),
            (" file:///tmp/a", Some(" file:///tmp/a")),
        ] {
            assert_eq!(uri_list_entry(line), expected, "{line:?}");
        }
    }

    #[test]
    fn paste_text_is_shell_quoted() {
        for (items, expected) in [
            (vec![], ""),
            (vec!["/tmp/a"], "'/tmp/a' "),
            (vec!["/tmp/a b", "/tmp/it's"], "'/tmp/a b' '/tmp/it'\\''s' "),
            (
                vec!["https://example.com/?q=1"],
                "'https://example.com/?q=1' ",
            ),
        ] {
            assert_eq!(quoted_paste_text(&items), expected.as_bytes(), "{items:?}");
        }
    }

    #[test]
    fn paste_text_keeps_non_utf8_paths() {
        let path = OsStr::from_bytes(b"/tmp/lat\xe9n\n$(x)");
        assert_eq!(quoted_paste_text([path]), b"'/tmp/lat\xe9n\n$(x)' ");
    }

    #[test]
    fn drop_highlight_covers_terminal_and_side_padding() {
        let terminal = TerminalBox {
            width: 600,
            height: 400,
            padding: Sides {
                top: 7,
                right: 6,
                bottom: 3,
                left: 6,
            },
            margin: Sides::default(),
        };
        let alloc = drop_highlight_allocation(&terminal);
        assert_eq!(
            (alloc.x(), alloc.y(), alloc.width(), alloc.height()),
            (-5, 1, 610, 398)
        );
    }
}
