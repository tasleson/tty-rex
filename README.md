# tty-rex

tty-rex is an experimental Rust port of the
[Ptyxis](https://gitlab.gnome.org/chergert/ptyxis) terminal GUI, written
with [relm4](https://relm4.org/). It started as a branch of Ptyxis and
uses its GSettings schema, palettes, icons and stylesheet, copied into
`ptyxis/`. It does not use `ptyxis-agent`. It needs the GTK,
libadwaita and VTE development packages plus Rust 1.93 or newer:

```bash
cargo run
```

No installation is required: `build.rs` generates the schema from
`ptyxis/org.gnome.Ptyxis.gschema.xml.in`, compiles it with
`glib-compile-schemas`, and the binary uses that compiled schema unless
`GSETTINGS_SCHEMA_DIR` is set. tty-rex uses its own application id,
`io.github.tasleson.TtyRex`, but reads and writes the regular
`org.gnome.Ptyxis` settings, so your existing profiles and shortcuts apply.
Settings Ptyxis has no key for live in tty-rex's own
`io.github.tasleson.TtyRex` schema
(`data/io.github.tasleson.TtyRex.gschema.xml`), compiled alongside it, so a
`GSETTINGS_SCHEMA_DIR` must provide both.
`glib-compile-resources` is needed at build time for the bundled icons.

tty-rex understands `--tab`, `--new-window`, `--tab-with-profile`,
`-x`/`--execute` (and `--`), `-d`/`--working-directory`, `-T`/`--title`,
`-s`/`--standalone`, `--preferences` and `--import-palette FILE` with the
same meaning as in Ptyxis; `cargo run -- --help` lists them.

Containers work as in Ptyxis, but without the agent: tty-rex lists
podman's containers itself (`podman ps`), classifying toolbox, distrobox
and apx ones by their labels, and runs shells in them through
`podman exec` or `distrobox enter`. They appear in the new tab menu and
as a profile's default container. Ptyxis's JHBuild entry is not ported.

As in Ptyxis, closing the last window saves its windows and tabs (profile,
directory, title, zoom, size and container) and the next start reopens
them: every tab with "Restore Session" on, only pinned tabs otherwise. The
state is kept in `~/.config/<app-id>/session.gvariant`, in the same format
as Ptyxis's but separate from it, so the two do not overwrite each other's
session.

A closed tab keeps running for five seconds, during which "Undo Close
Tab" (<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Alt</kbd>+<kbd>T</kbd> by
default) reopens it. When several tabs were closed, tty-rex reopens the
most recent one first, as the shortcut's description says; Ptyxis
reopens the oldest.

Closing a tab or window while a program other than a shell runs in it
asks first, as Ptyxis does with its `prompt-on-close` setting on (the
default; Ptyxis has no preference for it either). While such a program
runs, the tab also asks the desktop to hold off logging out, following
Ptyxis's `inhibit-logout` setting.

When a shell that reports its commands through VTE's `vte.sh` finishes
a command that ran for a quarter second or more in a tab you are not
looking at, the tab is flagged; if the window is not focused, a "Command completed" desktop
notification also names the command, and clicking it brings that tab
forward. On X11 the window's urgency hint is set too, as in Ptyxis.

Beyond Ptyxis, "Show Telemetry" in the terminal's context menu
(<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>M</kbd> by default) toggles a footer
with the network (TCP only), CPU and memory use of the processes started
from the selected tab. It reads `/proc` and the kernel's socket
diagnostics, which need no privileges.

Also unlike Ptyxis, the "Set Shortcut" dialog in the preferences refuses
a combination another shortcut already uses, in either the Ptyxis or the
tty-rex schema, and names that shortcut instead of saving a duplicate.

## Trying it safely

Because the preferences window writes your real Ptyxis and tty-rex
settings, try it with `GSETTINGS_BACKEND=memory cargo run -- -s`. The `-s`
matters: without it a launch hands over to an already running tty-rex
instance, which keeps using the real settings. The application id is fixed
at build time (`TTY_REX_APP_ID=io.github.tasleson.TtyRex.Devel cargo build`
produces a binary with a separate id, and the `.Devel` suffix adds
libadwaita's development stripes). Importing or dropping palettes is not
covered by the memory backend: it writes to
`~/.local/share/<app-id>/palettes`, and palettes imported by Ptyxis are
found as well. The saved session is not covered either; point `XDG_CONFIG_HOME`
at a scratch directory to keep it apart.

## Installing for your user

Like Ptyxis, tty-rex has a D-Bus activatable desktop file, so the desktop
starts it on demand through the session bus, in its own systemd user unit.
Running `tty-rex` from a shell still starts it directly. Unlike Ptyxis, its
shells stay in the same unit as the UI because they are not started
through `ptyxis-agent`. `data/` holds the desktop file, the D-Bus
activation file, the unit and the icon; fill in the directory holding the
`tty-rex` binary and install them for your user:

```bash
cargo build --release
BINDIR=$PWD/target/release   # or wherever you copied tty-rex
APP_ID=io.github.tasleson.TtyRex
mkdir -p ~/.local/share/dbus-1/services ~/.config/systemd/user
sed "s|@bindir@|$BINDIR|g" data/$APP_ID.service.in \
  > ~/.local/share/dbus-1/services/$APP_ID.service
sed "s|@bindir@|$BINDIR|g" data/tty-rex.service.in \
  > ~/.config/systemd/user/app-$APP_ID.service
mkdir -p ~/.local/share/applications ~/.local/share/icons
sed "s|@bindir@|$BINDIR|g" data/$APP_ID.desktop.in \
  > ~/.local/share/applications/$APP_ID.desktop
cp -r data/icons/hicolor ~/.local/share/icons/
systemctl --user daemon-reload
```

These files name the default application id; a binary built with another
`TTY_REX_APP_ID` needs them adjusted to match. Where the window icon
comes from the application rather than the desktop file (X11, for
example), tty-rex falls back to the Ptyxis icon until its own is
installed.

## License

tty-rex is licensed under the GNU General Public License v3.0 or later,
like Ptyxis; see `COPYING`. Ptyxis is primarily developed by Christian
Hergert.
