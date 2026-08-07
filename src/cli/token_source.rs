//! What `--token-secret` accepts, and the one source afhttp reads itself.
//!
//! The grammar and every general source — `env:NAME`, `file:PATH#DOT_PATH`,
//! `literal:` — come from AFDATA, which also renders the syntax into help and
//! rejects an unacceptable one with the rest of the argv errors. What is left
//! here is the part only afhttp can do: `container:NAME`, which reads a host
//! token out of a container this binary manages. That token lives in a named
//! volume, so no file path on this machine addresses it and no general source
//! could reach it.

use agent_first_data::value_source::SecretString;
use agent_first_data::{SourceSet, ValueSource};

use crate::shared::error::{Error, ErrorCode};

/// The scheme afhttp parses for itself. Declared to AFDATA so help documents it
/// and argv naming it is accepted; read below.
const CONTAINER: &str = "container";

/// The sources `--token-secret` accepts, in both the places that must agree:
/// the registry that validates argv, and the read that follows.
///
/// No stream sources. A token is one short string a caller already keeps
/// somewhere, and `stdin` in particular would collide with `--params @-`.
pub fn set() -> SourceSet {
    SourceSet::config().host_scheme(CONTAINER, "container:NAME")
}

/// Classify one `--token-secret` value. Pure — argv has already been validated
/// against the same set, so this only fails if the two ever disagree.
pub fn parse(raw: &str) -> Result<ValueSource, Error> {
    set().parse(raw).map_err(|error| {
        Error::new(
            ErrorCode::InvalidArgument,
            format!("--token-secret {error}"),
        )
    })
}

/// Read the token. A `SecretString`, so it reaches the `Authorization` header
/// and nothing else — including a `{:?}` on whatever ends up holding it.
pub fn read(source: &ValueSource) -> Result<SecretString, Error> {
    match source {
        ValueSource::Host { scheme, value } if scheme == CONTAINER => {
            crate::cli::cmd::container::read_host_token(value)
        }
        other => other.read_secret().map_err(|error| {
            Error::new(
                ErrorCode::InvalidArgument,
                format!("--token-secret {error}"),
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_container_scheme_is_afhttps_and_the_rest_is_afdatas() {
        assert_eq!(
            parse("container:afhttp-host").expect("container"),
            ValueSource::Host {
                scheme: CONTAINER.to_string(),
                value: "afhttp-host".to_string(),
            }
        );
        assert_eq!(
            parse("env:AFHTTP_TOKEN_SECRET").expect("env"),
            ValueSource::Env("AFHTTP_TOKEN_SECRET".to_string())
        );
        // A bare token is the token, and `literal:` rescues one that starts
        // with a scheme prefix.
        assert_eq!(
            parse("t0ken").expect("bare"),
            ValueSource::Literal("t0ken".to_string())
        );
        assert_eq!(
            parse("literal:container:x").expect("escape hatch"),
            ValueSource::Literal("container:x".to_string())
        );
    }

    /// A stream source is not in the set, so it is refused rather than read.
    #[test]
    fn a_token_does_not_come_from_a_stream() {
        for raw in ["stdin", "fd:3", "prompt"] {
            let error = parse(raw).expect_err(raw);
            assert_eq!(error.error_code, ErrorCode::InvalidArgument, "{raw}");
            assert!(error.detail.starts_with("--token-secret"), "{raw}");
        }
    }

    #[test]
    fn an_unreadable_source_names_the_flag_and_not_the_value() {
        let error = read(&ValueSource::Env("AFHTTP_TEST_ABSENT_TOKEN".to_string()))
            .expect_err("unset env source");
        assert!(
            error.detail.starts_with("--token-secret"),
            "{}",
            error.detail
        );
        assert!(
            error.detail.contains("AFHTTP_TEST_ABSENT_TOKEN"),
            "{}",
            error.detail
        );
    }
}
