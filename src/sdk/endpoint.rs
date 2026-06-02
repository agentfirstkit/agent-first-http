//! Endpoint URL parsing.
//!
//! `afhttp` connects to a host over one of:
//!   - `ws://host:port` / `wss://host:port` — CDP over WebSocket (primary)
//!   - `http://host:port` / `https://host:port` — for `/health`,
//!     `/capabilities`, and the ops panel
//!   - `unix:/path/to.sock` — local-machine listener (Linux/macOS only)
//!
//! The SDK accepts any of those forms and derives the others when needed
//! (e.g. the HTTP base URL from a WS endpoint).

use std::str::FromStr;

use crate::shared::error::{Error, ErrorCode};

/// Parsed endpoint. Round-trip-safe: any input that parses also formats
/// back to the canonical string with [`Endpoint::as_str`].
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
                    "unix: endpoints are not supported on this platform; use tcp:127.0.0.1:<port>",
                ))
            }
        } else {
            Err(Error::new(
                ErrorCode::InvalidEndpoint,
                format!(
                    "endpoint must start with ws://, wss://, http://, https://, or unix:; got {input:?}"
                ),
            ))
        }
    }

    /// HTTP base URL for `/health`, `/capabilities`, ops panel. WS endpoints
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
    let rest = rest.split('/').next().unwrap_or(rest);
    if rest.is_empty() {
        return Err(Error::new(
            ErrorCode::InvalidEndpoint,
            "endpoint missing host",
        ));
    }
    if let Some((host, port)) = rest.rsplit_once(':') {
        let port: u16 = port.parse().map_err(|_| {
            Error::new(
                ErrorCode::InvalidEndpoint,
                format!("endpoint port not a u16: {port:?}"),
            )
        })?;
        Ok((host.to_string(), port))
    } else {
        Ok((rest.to_string(), default_port))
    }
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
}
