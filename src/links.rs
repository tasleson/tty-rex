// links.rs
//
// Copyright 2015 Egmont Koblinger
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

//! Links in terminal output: the URL patterns of `src/terminal-regex.h`,
//! the custom links of a profile (`src/ptyxis-custom-link.c`) and finding
//! the link under a point (`ptyxis_terminal_update_url_actions()`).
//!
//! The patterns are PCRE2 and use features (subroutine calls, `DEFINE`)
//! that only VTE's own regex engine offers, so they are matched by VTE.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gtk::glib;
use vte::prelude::*;

use crate::profile::CustomLink;

/// Cursor shown over a link (`URL_MATCH_CURSOR_NAME`).
const URL_MATCH_CURSOR_NAME: &str = "pointer";

// `PCRE2_UTF | PCRE2_NO_UTF_CHECK | PCRE2_UCP | PCRE2_MULTILINE`.
const PCRE2_UTF: u32 = 0x0008_0000;
const PCRE2_NO_UTF_CHECK: u32 = 0x4000_0000;
const PCRE2_UCP: u32 = 0x0002_0000;
const PCRE2_MULTILINE: u32 = 0x0000_0400;
const REGEX_FLAGS: u32 = PCRE2_UTF | PCRE2_NO_UTF_CHECK | PCRE2_UCP | PCRE2_MULTILINE;

// The fragments of `src/terminal-regex.h`, as macros so that `concat!()`
// can put them together at compile time like the C preprocessor does.
macro_rules! apos_start_def {
    () => {
        "(?<APOS_START>(?<='))?"
    };
}
macro_rules! scheme {
    () => {
        "(?ix: news | telnet | nntp | https? | ftps? | sftp | webcal )"
    };
}
macro_rules! user {
    () => {
        "[-+.[:alnum:]]+"
    };
}
macro_rules! passchars_class {
    () => {
        r##"[-[:alnum:]\Q,?;.:/!%$^*&~"#'\E]"##
    };
}
macro_rules! pass {
    () => {
        concat!("(?x: :", passchars_class!(), "* )?")
    };
}
macro_rules! userpass {
    () => {
        concat!("(?:", user!(), pass!(), "@)?")
    };
}
macro_rules! s4_def {
    () => {
        "(?(DEFINE)(?<S4>(?x: (?: [0-9] | [1-9][0-9] | 1[0-9]{2} | 2[0-4][0-9] | 25[0-5] ) (?! [0-9] ) )))"
    };
}
macro_rules! ipv4_def {
    () => {
        concat!(
            s4_def!(),
            r"(?(DEFINE)(?<IPV4>(?x: (?: (?&S4) \. ){3} (?&S4) )))"
        )
    };
}
macro_rules! s6_def {
    () => {
        "(?(DEFINE)(?<S6>[[:xdigit:]]{1,4})(?<CS6>:(?&S6))(?<S6C>(?&S6):))"
    };
}
macro_rules! ipv6_full {
    () => {
        "(?x: (?&S6C){7} (?&S6) )"
    };
}
macro_rules! ipv6_left {
    () => {
        "(?x: : (?&CS6){1,7} )"
    };
}
macro_rules! ipv6_mid {
    () => {
        "(?x: (?! (?: [[:xdigit:]]*: ){8} ) (?&S6C){1,6} (?&CS6){1,6} )"
    };
}
macro_rules! ipv6_right {
    () => {
        "(?x: (?&S6C){1,7} : )"
    };
}
macro_rules! ipv6_null {
    () => {
        "(?x: :: )"
    };
}
macro_rules! ipv6v4_full {
    () => {
        "(?x: (?&S6C){6} )"
    };
}
macro_rules! ipv6v4_left {
    () => {
        "(?x: :: (?&S6C){0,5} )"
    };
}
macro_rules! ipv6v4_mid {
    () => {
        "(?x: (?! (?: [[:xdigit:]]*: ){7} ) (?&S6C){1,4} (?&CS6){1,4} ) :"
    };
}
macro_rules! ipv6v4_right {
    () => {
        "(?x: (?&S6C){1,5} : )"
    };
}
macro_rules! ip_def {
    () => {
        concat!(
            ipv4_def!(),
            s6_def!(),
            "(?(DEFINE)(?<IPV6>(?x: (?: ",
            ipv6_null!(),
            " | ",
            ipv6_left!(),
            " | ",
            ipv6_mid!(),
            " | ",
            ipv6_right!(),
            " | ",
            ipv6_full!(),
            " | (?: ",
            ipv6v4_full!(),
            " | ",
            ipv6v4_left!(),
            " | ",
            ipv6v4_mid!(),
            " | ",
            ipv6v4_right!(),
            " ) (?&IPV4) ) (?! [.:[:xdigit:]] ) )))"
        )
    };
}
macro_rules! hostnamesegmentchars_class {
    () => {
        "(?x: [-[:alnum:]] | (?! [[:ascii:]] ) [[:graph:]] )"
    };
}
macro_rules! hostname1 {
    () => {
        concat!(
            "(?x: (?: ",
            hostnamesegmentchars_class!(),
            r"+ \. )* ",
            hostnamesegmentchars_class!(),
            "* (?! [0-9] ) ",
            hostnamesegmentchars_class!(),
            "+ )"
        )
    };
}
macro_rules! hostname2 {
    () => {
        concat!(
            "(?x: (?: ",
            hostnamesegmentchars_class!(),
            r"+ \.)+ ",
            hostname1!(),
            " )"
        )
    };
}
macro_rules! url_host {
    () => {
        concat!("(?x: ", hostname1!(), r" | (?&IPV4) | \[ (?&IPV6) \] )")
    };
}
macro_rules! email_host {
    () => {
        concat!(
            "(?x: ",
            hostname2!(),
            r" | \[ (?: (?&IPV4) | (?&IPV6) ) \] )"
        )
    };
}
macro_rules! n_1_65535 {
    () => {
        "(?x: (?: [1-9][0-9]{0,3} | [1-5][0-9]{4} | 6[0-4][0-9]{3} | 65[0-4][0-9]{2} | 655[0-2][0-9] | 6553[0-5] ) (?! [0-9] ) )"
    };
}
macro_rules! port {
    () => {
        concat!(r"(?x: \:", n_1_65535!(), " )?")
    };
}
macro_rules! pathchars_class {
    () => {
        r"[-[:alnum:]\Q_$.+!*,:;@&=?/~#|%'\E]"
    };
}
macro_rules! pathterm_class {
    () => {
        r"[-[:alnum:]\Q_$+*:@&=/~#|%'\E]"
    };
}
macro_rules! pathterm_noapos_class {
    () => {
        r"[-[:alnum:]\Q_$+*:@&=/~#|%\E]"
    };
}
macro_rules! path_inner_def {
    () => {
        concat!(
            "(?(DEFINE)(?<PATH_INNER>(?x: (?: ",
            pathchars_class!(),
            r"* (?: \( (?&PATH_INNER) \) | \[ (?&PATH_INNER) \] ) )* ",
            pathchars_class!(),
            "* )))"
        )
    };
}
macro_rules! path_def {
    () => {
        concat!(
            "(?(DEFINE)(?<PATH>(?x: (?: ",
            pathchars_class!(),
            r"* (?: \( (?&PATH_INNER) \) | \[ (?&PATH_INNER) \] ) )* (?: ",
            pathchars_class!(),
            "* (?(<APOS_START>)",
            pathterm_noapos_class!(),
            "|",
            pathterm_class!(),
            ") )? )))"
        )
    };
}
macro_rules! urlpath {
    () => {
        "(?x: [/?#](?&PATH) )?"
    };
}
macro_rules! defs {
    () => {
        concat!(apos_start_def!(), ip_def!(), path_inner_def!(), path_def!())
    };
}

const REGEX_URL_AS_IS: &str = concat!(
    defs!(),
    scheme!(),
    "://",
    userpass!(),
    url_host!(),
    port!(),
    urlpath!()
);
const REGEX_URL_FILE: &str = concat!(
    defs!(),
    "(?ix: file:/ (?: / (?: ",
    hostname1!(),
    " )? / )? (?! / ) )(?&PATH)"
);
const REGEX_URL_HTTP: &str = concat!(
    defs!(),
    "(?<!(?:",
    hostnamesegmentchars_class!(),
    "|[.]))(?=(?i:www|ftp))",
    hostname1!(),
    port!(),
    urlpath!()
);
const REGEX_EMAIL: &str = concat!(defs!(), "(?i:mailto:)?", user!(), "@", email_host!());

/// The predefined patterns, in the order of `url_regexes_str`.
const URL_PATTERNS: [&str; 4] = [REGEX_URL_AS_IS, REGEX_URL_HTTP, REGEX_URL_FILE, REGEX_EMAIL];

thread_local! {
    /// The predefined patterns, compiled and JITted once for every terminal.
    /// A pattern that fails to compile is left out (with a warning).
    static PREDEFINED: Vec<vte::Regex> = URL_PATTERNS
        .iter()
        .filter_map(|pattern| match compile(pattern) {
            Ok(regex) => Some(regex),
            Err(e) => {
                glib::g_warning!("tty-rex", "Failed to compile regex: {e}");
                None
            }
        })
        .collect();
}

fn compile(pattern: &str) -> Result<vte::Regex, glib::Error> {
    let regex = vte::Regex::for_match(pattern, REGEX_FLAGS)?;
    // Like the C build, only a warning when the JIT is unavailable.
    if let Err(e) = regex.jit(0x0000_0001).and_then(|()| regex.jit(0x0000_0002)) {
        glib::g_warning!("tty-rex", "Failed to JIT regex: {e}: Regex was: {pattern}");
    }
    Ok(regex)
}

/// Makes the link of a custom link out of the text it matched
/// (`ptyxis_custom_link_substitute()`).
fn substitute(regex: &vte::Regex, link: &CustomLink, subject: &str) -> Option<String> {
    match regex.substitute(subject, &link.target, 0) {
        Ok(text) => Some(glib::Uri::escape_string(&text, Some(":/."), false).into()),
        Err(e) => {
            glib::g_warning!(
                "tty-rex",
                "Failed to perform substitution with regex: {e}: Regex was: '{}', \
                 Substitution was: '{}', Subject was: '{subject}'",
                link.pattern,
                link.target
            );
            None
        }
    }
}

/// The links a terminal matches: its profile's custom links first, then
/// the predefined patterns (`ptyxis_terminal_update_custom_links_list()`).
#[derive(Clone, Default)]
pub struct Links {
    /// Custom links by the tag VTE gave their pattern.
    custom: Rc<RefCell<HashMap<i32, (vte::Regex, CustomLink)>>>,
}

impl Links {
    /// Replaces the terminal's matches with the predefined patterns
    /// preceded by `custom_links`.
    pub fn update(&self, terminal: &vte::Terminal, custom_links: &[CustomLink]) {
        terminal.match_remove_all();
        let mut custom = self.custom.borrow_mut();
        custom.clear();
        for link in custom_links {
            let regex = match compile(&link.pattern) {
                Ok(regex) => regex,
                Err(e) => {
                    glib::g_warning!(
                        "tty-rex",
                        "Failed to compile regex: {e}: Regex was: {}",
                        link.pattern
                    );
                    continue;
                }
            };
            let tag = terminal.match_add_regex(&regex, 0);
            terminal.match_set_cursor_name(tag, URL_MATCH_CURSOR_NAME);
            custom.insert(tag, (regex, link.clone()));
        }
        PREDEFINED.with(|regexes| {
            for regex in regexes {
                let tag = terminal.match_add_regex(regex, 0);
                terminal.match_set_cursor_name(tag, URL_MATCH_CURSOR_NAME);
            }
        });
    }

    /// The link at the widget coordinates `x`, `y`: an OSC 8 hyperlink
    /// first, otherwise text matching a pattern.
    pub fn at(&self, terminal: &vte::Terminal, x: f64, y: f64) -> Option<String> {
        if let Some(hyperlink) = terminal.check_hyperlink_at(x, y) {
            return Some(hyperlink.into());
        }
        let (matched, tag) = terminal.check_match_at(x, y);
        let matched = matched?;
        match self.custom.borrow().get(&tag) {
            Some((regex, link)) => substitute(regex, link, &matched),
            None => Some(matched.into()),
        }
    }
}

/// The widget coordinates of the `VteEventContext` that
/// `setup-context-menu` carries, or `None` when the menu is going away
/// (a `NULL` context). The bindings do not wrap the type, so it is read
/// through the C API.
pub fn event_context_coordinates(value: &glib::Value) -> Option<(f64, f64)> {
    use glib::translate::ToGlibPtr;

    // SAFETY: the type check guards the pointer read, and the context, owned
    // by VTE, stays valid for the whole signal emission this runs in.
    unsafe {
        let context_type = vte::ffi::vte_event_context_get_type();
        if glib::gobject_ffi::g_type_check_value_holds(value.to_glib_none().0, context_type)
            == glib::ffi::GFALSE
        {
            return None;
        }
        // VTE registers the context as a pointer type, not a boxed one.
        let context = glib::gobject_ffi::g_value_get_pointer(value.to_glib_none().0)
            as *const vte::ffi::VteEventContext;
        if context.is_null() {
            return None;
        }
        let (mut x, mut y) = (0.0, 0.0);
        (vte::ffi::vte_event_context_get_coordinates(context, &mut x, &mut y) != glib::ffi::GFALSE)
            .then_some((x, y))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predefined_patterns_compile() {
        for pattern in URL_PATTERNS {
            compile(pattern).unwrap_or_else(|e| panic!("{e}: {pattern}"));
        }
    }

    #[test]
    fn custom_link_substitutes_and_escapes() {
        let link = CustomLink {
            pattern: r"#(\d+)".into(),
            target: "https://example.com/issues/$1 x".into(),
        };
        let regex = compile(&link.pattern).unwrap();
        assert_eq!(
            substitute(&regex, &link, "#42").as_deref(),
            Some("https://example.com/issues/42%20x")
        );
    }
}
