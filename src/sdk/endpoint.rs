//! Endpoint URL parsing.
//!
//! `afhttp` connects to a host over one of:
//!   - `ws://host:port` / `wss://host:port` — CDP over WebSocket (primary)
//!   - `http://host:port` / `https://host:port` — for `/health`,
//!     `/capabilities`, and the takeover panel
//!   - `unix:/path/to.sock` — local-machine listener (Linux/macOS only)
//!
//! The SDK accepts any of those forms and derives the others when needed
//! (e.g. the HTTP base URL from a WS endpoint).

use std::str::FromStr;

use crate::shared::error::{Error, ErrorCode};

/// Parsed endpoint. Paths, queries, and fragments after the authority are
/// dropped; [`Endpoint::http_base`] and [`Endpoint::cdp_ws_url`] derive the
/// URLs actually dialed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    Ws {
        host: String,
        port: u16,
        secure: bool,
    },
    Http {
        host: String,
        port: u16,
        secure: bool,
    },
    #[cfg(unix)]
    Unix { path: std::path::PathBuf },
}

impl Endpoint {
    pub fn parse(input: &str) -> Result<Self, Error> {
        if let Some(rest) = input.strip_prefix("ws://") {
            let (host, port) = split_host_port(rest, 80)?;
            Ok(Self::Ws {
                host,
                port,
                secure: false,
            })
        } else if let Some(rest) = input.strip_prefix("wss://") {
            let (host, port) = split_host_port(rest, 443)?;
            Ok(Self::Ws {
                host,
                port,
                secure: true,
            })
        } else if let Some(rest) = input.strip_prefix("http://") {
            let (host, port) = split_host_port(rest, 80)?;
            Ok(Self::Http {
                host,
                port,
                secure: false,
            })
        } else if let Some(rest) = input.strip_prefix("https://") {
            let (host, port) = split_host_port(rest, 443)?;
            Ok(Self::Http {
                host,
                port,
                secure: true,
            })
        } else if let Some(path) = input.strip_prefix("unix:") {
            #[cfg(unix)]
            {
                Ok(Self::Unix {
                    path: std::path::PathBuf::from(path),
                })
            }
            #[cfg(not(unix))]
            {
                let _ = path;
                Err(Error::new(
                    ErrorCode::InvalidEndpoint,
                    "unix: endpoints are not supported on this platform; use ws://127.0.0.1:<port>",
                ))
            }
        } else {
            Err(Error::new(
                ErrorCode::InvalidEndpoint,
                "endpoint must start with ws://, wss://, http://, https://, or unix:",
            ))
        }
    }

    /// HTTP base URL for `/health`, `/capabilities`, takeover panel. WS endpoints
    /// derive an HTTP twin on the same host:port; Unix endpoints derive
    /// `http://unix-socket` and use a unix transport at request time.
    #[must_use]
    pub fn http_base(&self) -> String {
        match self {
            Self::Http { host, port, secure } | Self::Ws { host, port, secure } => {
                let scheme = if *secure { "https" } else { "http" };
                format!("{scheme}://{host}:{port}")
            }
            #[cfg(unix)]
            Self::Unix { .. } => "http://unix-socket".to_string(),
        }
    }

    /// WS URL with path `/cdp` appended; the host routes that to the CDP
    /// proxy.
    #[must_use]
    pub fn cdp_ws_url(&self) -> String {
        match self {
            Self::Ws { host, port, secure } | Self::Http { host, port, secure } => {
                let scheme = if *secure { "wss" } else { "ws" };
                format!("{scheme}://{host}:{port}/cdp")
            }
            #[cfg(unix)]
            Self::Unix { .. } => "ws://unix-socket/cdp".to_string(),
        }
    }
}

impl FromStr for Endpoint {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

fn split_host_port(rest: &str, default_port: u16) -> Result<(String, u16), Error> {
    if rest.is_empty() || rest.starts_with('/') {
        return Err(Error::new(
            ErrorCode::InvalidEndpoint,
            "endpoint missing host",
        ));
    }
    // Parse an authority with the URL parser so bracketed IPv6 hosts and
    // query/fragment boundaries have the same meaning as the derived URLs.
    // Use the matching default scheme to retain explicit non-default ports.
    let scheme = if default_port == 443 { "https" } else { "http" };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    if authority.ends_with(':') {
        return Err(Error::new(
            ErrorCode::InvalidEndpoint,
            "endpoint port must be a u16",
        ));
    }
    if rest
        .chars()
        .any(|ch| ch.is_whitespace() || ch.is_control() || ch == '\\')
    {
        return Err(Error::new(
            ErrorCode::InvalidEndpoint,
            "endpoint contains invalid URL characters",
        ));
    }
    let parsed = url::Url::parse(&format!("{scheme}://{rest}"))
        .map_err(|_| Error::new(ErrorCode::InvalidEndpoint, "invalid endpoint authority"))?;
    if authority.contains('@') {
        return Err(Error::new(
            ErrorCode::InvalidEndpoint,
            "endpoint credentials must be supplied separately",
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| Error::new(ErrorCode::InvalidEndpoint, "endpoint missing host"))?;
    Ok((host.to_owned(), parsed.port().unwrap_or(default_port)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ws_with_explicit_port() {
        let e = Endpoint::parse("ws://127.0.0.1:9222").unwrap();
        assert_eq!(
            e,
            Endpoint::Ws {
                host: "127.0.0.1".into(),
                port: 9222,
                secure: false
            }
        );
    }

    #[test]
    fn parses_wss_with_default_port() {
        let e = Endpoint::parse("wss://host.example").unwrap();
        assert_eq!(
            e,
            Endpoint::Ws {
                host: "host.example".into(),
                port: 443,
                secure: true
            }
        );
    }

    #[test]
    fn parses_http_endpoints() {
        let e = Endpoint::parse("http://localhost:8080").unwrap();
        assert!(matches!(e, Endpoint::Http { port: 8080, .. }));
    }

    #[cfg(unix)]
    #[test]
    fn parses_unix_endpoint() {
        let e = Endpoint::parse("unix:/run/afhttp/work.sock").unwrap();
        assert!(matches!(e, Endpoint::Unix { .. }));
    }

    #[test]
    fn http_base_strips_path() {
        let e = Endpoint::parse("ws://example:9222").unwrap();
        assert_eq!(e.http_base(), "http://example:9222");
    }

    #[test]
    fn cdp_ws_url_uses_secure_scheme_when_endpoint_is_secure() {
        let e = Endpoint::parse("https://example:443").unwrap();
        assert_eq!(e.cdp_ws_url(), "wss://example:443/cdp");
    }

    #[test]
    fn rejects_unknown_scheme() {
        let err = Endpoint::parse("ftp://example").err();
        assert_eq!(err.map(|e| e.error_code), Some(ErrorCode::InvalidEndpoint),);
    }

    #[test]
    fn rejects_bad_port() {
        let err = Endpoint::parse("ws://host:notnum").err();
        assert_eq!(err.map(|e| e.error_code), Some(ErrorCode::InvalidEndpoint),);
    }

    #[cfg(not(unix))]
    #[test]
    fn unix_endpoint_error_suggests_a_parseable_endpoint() {
        let error = Endpoint::parse("unix:/run/afhttp.sock").unwrap_err();
        assert_eq!(error.error_code, ErrorCode::InvalidEndpoint);
        assert!(error.detail.contains("ws://127.0.0.1:<port>"));
        assert!(Endpoint::parse("ws://127.0.0.1:9222").is_ok());
    }

    #[test]
    fn ipv6_and_url_boundaries_produce_valid_origin_urls() {
        for (endpoint, expected) in [
            ("ws://[::1]", "http://[::1]:80"),
            ("wss://[::1]", "https://[::1]:443"),
            ("wss://[::1]:80/path", "https://[::1]:80"),
            ("http://[2001:db8::1]:8080", "http://[2001:db8::1]:8080"),
            (
                "https://host.example?query=value",
                "https://host.example:443",
            ),
            ("ws://host.example#fragment", "http://host.example:80"),
        ] {
            let parsed = Endpoint::parse(endpoint).expect("valid endpoint");
            assert_eq!(parsed.http_base(), expected);
            assert!(url::Url::parse(&parsed.cdp_ws_url()).is_ok());
        }
    }

    #[test]
    fn invalid_authorities_fail_before_connecting() {
        for endpoint in [
            "ws:///host",
            "ws://:80",
            "ws://host:",
            "wss://[::1]:/path",
            "ws://@host",
            "ws://[::1",
            "ws://host name:80",
            "ws://host\\other",
            "ws://reader:password-canary@host:80",
        ] {
            let error = Endpoint::parse(endpoint).expect_err("invalid authority");
            assert_eq!(error.error_code, ErrorCode::InvalidEndpoint);
            assert!(!error.to_string().contains("password-canary"));
        }
    }

    #[test]
    fn client_connect_parse_errors_never_echo_endpoint_credentials() {
        for endpoint in [
            "ws://reader:password-canary@127.0.0.1:invalid?token_secret=query-canary",
            "ftp://reader:password-canary@127.0.0.1/path with-space?token_secret=query-canary",
            "reader:password-canary@127.0.0.1?token_secret=query-canary",
        ] {
            let err = crate::sdk::Client::connect(endpoint)
                .err()
                .expect("endpoint must fail parsing");
            assert_eq!(err.error_code, ErrorCode::InvalidEndpoint);
            let display = err.to_string();
            let json = serde_json::to_string(&err).expect("serialize SDK error");
            for text in [&err.detail, &display, &json] {
                assert!(!text.contains("password-canary"), "password leaked: {text}");
                assert!(!text.contains("query-canary"), "query leaked: {text}");
            }
        }
    }
}
