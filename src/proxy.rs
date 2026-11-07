// proxy.rs
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

//! The proxy environment a profile with `use-proxy` gives its shells,
//! as `ptyxis_agent_impl_handle_discover_proxy_environment()` builds it:
//! from the desktop's `org.gnome.system.proxy` settings when that schema
//! is installed, otherwise copied from tty-rex's own environment.

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

const PROXY_SCHEMA: &str = "org.gnome.system.proxy";
const MODE_KEY: &str = "mode";
/// Only manual proxies can be expressed as variables; "auto" (a PAC
/// file) is not supported, as in Ptyxis.
const MANUAL_MODE: &str = "manual";
const IGNORE_HOSTS_KEY: &str = "ignore-hosts";
const HOST_KEY: &str = "host";
const PORT_KEY: &str = "port";
const USE_AUTHENTICATION_KEY: &str = "use-authentication";
const AUTHENTICATION_USER_KEY: &str = "authentication-user";
const AUTHENTICATION_PASSWORD_KEY: &str = "authentication-password";
/// The only protocol whose settings carry credentials.
const AUTHENTICATED_PROTOCOL: &str = "http";

/// (child schema suffix, URI scheme, variable) per proxied protocol.
const PROTOCOLS: &[(&str, &str, &str)] = &[
    ("http", "http", "http_proxy"),
    ("https", "http", "https_proxy"),
    ("ftp", "ftp", "ftp_proxy"),
    ("socks", "socks", "all_proxy"),
];

/// Variables copied from the environment when the desktop has no proxy
/// settings.
const ENVIRONMENT_VARIABLES: &[&str] = &[
    "ftp_proxy",
    "FTP_PROXY",
    "http_proxy",
    "HTTP_PROXY",
    "https_proxy",
    "HTTPS_PROXY",
    "no_proxy",
    "NO_PROXY",
    "all_proxy",
    "ALL_PROXY",
];

/// Characters left unescaped in each URI part (`uri_build_with_user()`).
const USER_ALLOWED_CHARS: &str = "!$&'()*+,=";
const PASSWORD_ALLOWED_CHARS: &str = "!$&'()*+,=:";
const IP_ADDR_ALLOWED_CHARS: &str = ":";
/// `G_URI_RESERVED_CHARS_SUBCOMPONENT_DELIMITERS`.
const HOST_ALLOWED_CHARS: &str = "!$&'()*+,;=";

/// One protocol's manual proxy settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: i32,
    pub user: String,
    pub password: String,
}

fn escape(part: &str, allowed: &str) -> glib::GString {
    glib::Uri::escape_string(part, Some(allowed), true)
}

/// Builds `scheme://[user[:password]@]host:port`, or `None` when the
/// endpoint has no host or port (it is not configured).
pub fn proxy_uri(scheme: &str, endpoint: &Endpoint) -> Option<String> {
    if endpoint.host.is_empty() || endpoint.port <= 0 {
        return None;
    }
    let mut uri = format!("{scheme}://");
    if !endpoint.user.is_empty() {
        uri.push_str(&escape(&endpoint.user, USER_ALLOWED_CHARS));
        if !endpoint.password.is_empty() {
            uri.push(':');
            uri.push_str(&escape(&endpoint.password, PASSWORD_ALLOWED_CHARS));
        }
        uri.push('@');
    }
    if endpoint.host.contains(':') && glib::hostname_is_ip_address(&endpoint.host) {
        uri.push('[');
        uri.push_str(&escape(&endpoint.host, IP_ADDR_ALLOWED_CHARS));
        uri.push(']');
    } else {
        uri.push_str(&escape(&endpoint.host, HOST_ALLOWED_CHARS));
    }
    uri.push_str(&format!(":{}", endpoint.port));
    Some(uri)
}

/// The variables for a manual proxy configuration. `endpoint` returns a
/// protocol's settings by its child schema suffix; each configured one is
/// set in lower and upper case, like `no_proxy` for `ignore_hosts`.
pub fn manual_environment(
    endpoint: impl Fn(&str) -> Option<Endpoint>,
    ignore_hosts: &[String],
) -> Vec<String> {
    let mut env = Vec::new();
    for (protocol, scheme, variable) in PROTOCOLS {
        if let Some(uri) = endpoint(protocol).and_then(|e| proxy_uri(scheme, &e)) {
            env.push(format!("{variable}={uri}"));
            env.push(format!("{}={uri}", variable.to_ascii_uppercase()));
        }
    }
    if !ignore_hosts.is_empty() {
        let hosts = ignore_hosts.join(",");
        env.push(format!("no_proxy={hosts}"));
        env.push(format!("NO_PROXY={hosts}"));
    }
    env
}

/// The proxy variables found through `getenv`.
pub fn inherited_environment(getenv: impl Fn(&str) -> Option<String>) -> Vec<String> {
    ENVIRONMENT_VARIABLES
        .iter()
        .filter_map(|key| getenv(key).map(|value| format!("{key}={value}")))
        .collect()
}

fn settings(schema_id: &str) -> Option<gio::Settings> {
    let schema = gio::SettingsSchemaSource::default()?.lookup(schema_id, true)?;
    Some(gio::Settings::new_full(
        &schema,
        None::<&gio::SettingsBackend>,
        None,
    ))
}

fn endpoint(protocol: &str) -> Option<Endpoint> {
    let settings = settings(&format!("{PROXY_SCHEMA}.{protocol}"))?;
    let mut endpoint = Endpoint {
        host: settings.string(HOST_KEY).into(),
        port: settings.int(PORT_KEY),
        ..Default::default()
    };
    if protocol == AUTHENTICATED_PROTOCOL && settings.boolean(USE_AUTHENTICATION_KEY) {
        endpoint.user = settings.string(AUTHENTICATION_USER_KEY).into();
        endpoint.password = settings.string(AUTHENTICATION_PASSWORD_KEY).into();
    }
    Some(endpoint)
}

/// The proxy variables for a shell, as `KEY=VALUE` strings.
pub fn environment() -> Vec<String> {
    let Some(settings) = settings(PROXY_SCHEMA) else {
        return inherited_environment(|key| std::env::var(key).ok());
    };
    if settings.string(MODE_KEY) != MANUAL_MODE {
        return Vec::new();
    }
    let ignore_hosts: Vec<String> = settings
        .strv(IGNORE_HOSTS_KEY)
        .iter()
        .map(|host| host.to_string())
        .collect();
    manual_environment(endpoint, &ignore_hosts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(host: &str, port: i32, user: &str, password: &str) -> Endpoint {
        Endpoint {
            host: host.into(),
            port,
            user: user.into(),
            password: password.into(),
        }
    }

    #[test]
    fn uris() {
        let cases = [
            (endpoint("proxy", 3128, "", ""), Some("http://proxy:3128")),
            (endpoint("", 3128, "", ""), None),
            (endpoint("proxy", 0, "", ""), None),
            (
                endpoint("proxy", 8080, "me", "p@ss:w"),
                Some("http://me:p%40ss:w@proxy:8080"),
            ),
            // A password without a user is dropped.
            (
                endpoint("proxy", 8080, "", "secret"),
                Some("http://proxy:8080"),
            ),
            (endpoint("a b", 1, "a/b", ""), Some("http://a%2Fb@a%20b:1")),
            (endpoint("::1", 3128, "", ""), Some("http://[::1]:3128")),
        ];
        for (endpoint, expected) in cases {
            assert_eq!(
                proxy_uri("http", &endpoint).as_deref(),
                expected,
                "{endpoint:?}"
            );
        }
    }

    #[test]
    fn manual_variables() {
        let env = manual_environment(
            |protocol| match protocol {
                "https" => Some(endpoint("secure", 443, "", "")),
                "socks" => Some(endpoint("sock", 1080, "", "")),
                "ftp" => Some(endpoint("", 21, "", "")),
                _ => None,
            },
            &["localhost".into(), "127.0.0.0/8".into()],
        );
        assert_eq!(
            env,
            [
                "https_proxy=http://secure:443",
                "HTTPS_PROXY=http://secure:443",
                "all_proxy=socks://sock:1080",
                "ALL_PROXY=socks://sock:1080",
                "no_proxy=localhost,127.0.0.0/8",
                "NO_PROXY=localhost,127.0.0.0/8",
            ]
        );
        assert!(manual_environment(|_| None, &[]).is_empty());
    }

    #[test]
    fn inherited_variables() {
        let env = inherited_environment(|key| match key {
            "http_proxy" => Some("http://p:1".into()),
            "NO_PROXY" => Some("local".into()),
            _ => None,
        });
        assert_eq!(env, ["http_proxy=http://p:1", "NO_PROXY=local"]);
    }
}
