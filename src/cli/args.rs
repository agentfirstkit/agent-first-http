//! Top-level CLI argument parsing. Uses `clap` derive for the subcommand
//! enum; each subcommand owns its own arg struct under `cmd::<name>`.

use clap::{Parser, Subcommand};

use crate::shared::error::{Error, ErrorCode};

pub const DISPLAY_NAME: &str = env!("DISPLAY_NAME");

#[derive(Parser, Debug)]
#[command(
    name = env!("DISPLAY_NAME"),
    bin_name = "afhttp",
    version,
    about = env!("CARGO_PKG_DESCRIPTION"),
    long_about = concat!(env!("DISPLAY_NAME"), " - ", env!("CARGO_PKG_DESCRIPTION")),
)]
pub struct Cli {
    /// Redirect stdout bytes to this file.
    #[arg(long = "stdout-file", value_name = "PATH", global = true)]
    pub stdout_file: Option<String>,

    /// Redirect stderr bytes to this file.
    #[arg(long = "stderr-file", value_name = "PATH", global = true)]
    pub stderr_file: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Fetch a URL.
    Fetch(Box<crate::cli::cmd::fetch::Args>),
    /// Run the browser host.
    Host(crate::cli::cmd::host::Args),
    /// Upload a local file to a browser tab via DOM.setFileInputFiles.
    Upload(crate::cli::cmd::upload::Args),
    /// Send a raw CDP method.
    Cdp(crate::cli::cmd::cdp::Args),
    /// Print a short-lived takeover URL.
    Panel(crate::cli::cmd::panel::Args),
    /// Query /health.
    Health(crate::cli::cmd::health::Args),
    /// Query /capabilities.
    Capabilities(crate::cli::cmd::capabilities::Args),
    /// Local profile lifecycle commands.
    Profile(crate::cli::cmd::profile::Args),
    /// List and close CDP targets attached to the host.
    Tabs(crate::cli::cmd::tabs::Args),
    /// Install, remove, or check the embedded Agent Skill (Codex, Claude Code, opencode, Hermes).
    Skill(crate::cli::cmd::skill::Args),
    /// Build and run the host container (Docker or Apple) from the embedded recipe.
    Container(crate::cli::cmd::container::Args),
}

pub struct Parsed {
    pub command: Command,
}

pub fn parse() -> Result<Parsed, Error> {
    let cli = Cli::try_parse().map_err(|e| {
        use clap::error::ErrorKind;
        // `--version` and per-subcommand `--help` arrive here as clap "errors";
        // render them to stdout and exit success rather than turning them into
        // an invalid_argument envelope. (Top-level `--help --output ...`
        // are handled earlier in `cli::run`.)
        if matches!(
            e.kind(),
            ErrorKind::DisplayHelp
                | ErrorKind::DisplayVersion
                | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        ) {
            let _ = e.print();
            std::process::exit(0);
        }
        // clap's error type already includes usage; surface as
        // invalid_argument so machine consumers can branch.
        Error::new(ErrorCode::InvalidArgument, e.to_string())
    })?;
    let _stream_redirect_args = (&cli.stdout_file, &cli.stderr_file);
    Ok(Parsed {
        command: cli.command,
    })
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn clap_command_flag_snapshot_matches() {
        let mut snapshot = String::new();
        write_command_snapshot(&Cli::command(), 0, &mut snapshot);
        assert_eq!(
            snapshot,
            include_str!("../../tests/golden/cli-command-flags.txt")
        );
    }

    #[test]
    fn cli_contract_has_no_legacy_aliases() {
        let command = Cli::command();
        assert_eq!(command.get_subcommands().count(), 11);
        let mut snapshot = String::new();
        write_command_snapshot(&command, 0, &mut snapshot);
        for forbidden in [
            "  command download\n",
            "  command takeover\n",
            "  command hard-site\n",
            "--profile-name",
            concat!("profile", "_name"),
            concat!("?", "profile="),
            "arg timeout --timeout\n",
            "arg health --health\n",
            "arg network_redact --network-redact\n",
            "arg takeover_quality --takeover-quality\n",
            "legacy",
        ] {
            assert!(
                !snapshot.contains(forbidden),
                "CLI contract retained forbidden legacy surface {forbidden:?}: {snapshot}"
            );
        }
    }

    fn write_command_snapshot(cmd: &clap::Command, depth: usize, out: &mut String) {
        let indent = "  ".repeat(depth);
        out.push_str(&format!("{indent}command {}\n", cmd.get_name()));
        for arg in cmd.get_arguments() {
            let long = arg
                .get_long()
                .map(|v| format!(" --{v}"))
                .unwrap_or_default();
            out.push_str(&format!("{indent}  arg {}{long}\n", arg.get_id()));
        }
        for sub in cmd.get_subcommands() {
            write_command_snapshot(sub, depth + 1, out);
        }
    }
}
