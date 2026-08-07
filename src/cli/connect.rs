//! The host every client command talks to, resolved once per run.
//!
//! Two things used to be the caller's job on every single invocation: saying
//! where the host is, and carrying its token there. Both are now defaults with
//! an override. When `--endpoint-url` (or `AFHTTP_ENDPOINT_URL`) is absent, the
//! standard local `afhttp-host` container is discovered and its own token read
//! from it, so the ordinary local case is just the command. When an endpoint is
//! named, nothing is discovered — a caller who says where to go decides how to
//! authenticate too, by naming a source for the token.
//!
//! Discovery is read-only. It never starts, recreates, or reconfigures a
//! container; `afhttp container install` remains the only thing that does.

use agent_first_data::ValueSource;
use agent_first_data::value_source::SecretString;

use crate::cli::token_source;
use crate::sdk::Client;
use crate::shared::error::Error;

/// A host named on argv, or the local one to go find.
#[derive(Debug, Clone, Default)]
pub struct Connection {
    /// `--endpoint-url`, else `AFHTTP_ENDPOINT_URL`, else discovery.
    pub endpoint: Option<String>,
    /// `--token-secret`, else `AFHTTP_TOKEN_SECRET`, else whatever the
    /// discovered host says its token is.
    pub token: Option<ValueSource>,
}

/// The same connection with every source read.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub endpoint: String,
    pub token: Option<SecretString>,
}

impl Connection {
    pub fn new(endpoint: Option<String>, token: Option<ValueSource>) -> Self {
        Self { endpoint, token }
    }

    /// Whether the caller named the host, as opposed to it being discovered.
    /// A recommended follow-up command repeats the first and omits the second,
    /// so the next run rediscovers rather than pinning a container's address.
    pub fn is_explicit(&self) -> bool {
        self.endpoint.is_some()
    }

    /// Read the token source, then find the host if none was named.
    ///
    /// The token is resolved first so an explicit `--token-secret` still wins
    /// for a discovered host: a caller may know a token the container's own
    /// data volume does not hold.
    pub async fn resolve(&self) -> Result<Resolved, Error> {
        let token = self.token.as_ref().map(token_source::read).transpose()?;
        match self.endpoint.as_deref() {
            Some(endpoint) => Ok(Resolved {
                endpoint: endpoint.to_string(),
                token,
            }),
            None => {
                let host = crate::cli::cmd::container::discover_default_local_host(
                    token.as_ref().map(SecretString::expose_secret),
                )
                .await?;
                Ok(Resolved {
                    endpoint: host.endpoint,
                    token: host.token_secret,
                })
            }
        }
    }

    /// Resolve and connect in one step, for the commands that need nothing from
    /// the connection but a client.
    pub async fn client(&self) -> Result<Client, Error> {
        self.resolve().await?.client()
    }
}

impl Resolved {
    pub fn client(&self) -> Result<Client, Error> {
        let client = Client::connect(&self.endpoint)?;
        Ok(match &self.token {
            Some(token) => client.with_token(token.expose_secret()),
            None => client,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_named_endpoint_resolves_without_discovery() {
        // No container runtime is consulted: the endpoint and the token are
        // both on argv, so this test passes on a machine with no Docker at all.
        let connection = Connection::new(
            Some("ws://127.0.0.1:9222".to_string()),
            Some(ValueSource::Literal("t0ken".to_string())),
        );
        assert!(connection.is_explicit());
        let resolved = connection.resolve().await.expect("explicit host resolves");
        assert_eq!(resolved.endpoint, "ws://127.0.0.1:9222");
        assert_eq!(
            resolved.token.as_ref().map(SecretString::expose_secret),
            Some("t0ken")
        );
    }

    #[tokio::test]
    async fn an_unreadable_token_source_fails_before_the_host_is_touched() {
        let connection = Connection::new(
            Some("ws://127.0.0.1:9222".to_string()),
            Some(ValueSource::Env("AFHTTP_TEST_ABSENT_TOKEN".to_string())),
        );
        let error = connection.resolve().await.expect_err("unset env source");
        assert!(error.detail.contains("--token-secret"), "{}", error.detail);
    }
}
