//! Top-level CLI argument parsing. Uses `clap` derive for the subcommand
//! enum; each subcommand owns its own arg struct under `cmd::<name>`.

use clap::{Parser, Subcommand};

use crate::shared::error::{Error, ErrorCode};

/// A URL acquisition tool for AI agents.
///
/// Give afhttp a URL and it returns the page plus the artifacts an agent needs to
/// decide what to do next: rendered HTML, a DOM observation, a screenshot, and
/// network and console logs. It covers the whole acquisition range behind one
/// structured contract — a plain HTTP fetch when that works, a browser-backed
/// fetch when it does not, deep network capture, a raw CDP escape hatch, and an
/// ops panel for human takeover (login, captcha, 2FA).
///
/// Two roles. `afhttp host` is the long-lived browser-host: it holds Chromium and
/// one on-disk profile, and exposes a CDP endpoint plus the ops panel. The other
/// commands are short-lived drivers that connect to a host, do work, and write
/// artifacts locally. Run the host where the browser needs to be and the driver
/// wherever the agent runs.
///
/// Every output is one line of structured JSON; every failure carries a stable
/// error_code. The tool never decides what a page means — the agent does.
#[derive(Parser, Debug)]
#[command(name = "afhttp", version, verbatim_doc_comment)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run the browser host.
    Host(crate::cli::cmd::host::Args),
    /// Fetch a URL.
    Fetch(Box<crate::cli::cmd::fetch::Args>),
    /// Upload a local file to a browser tab via DOM.setFileInputFiles.
    Upload(crate::cli::cmd::upload::Args),
    /// Send a raw CDP method.
    Cdp(crate::cli::cmd::cdp::Args),
    /// Print or open the ops panel URL.
    Ui(crate::cli::cmd::ui::Args),
    /// Query /health.
    Health(crate::cli::cmd::health::Args),
    /// Query /capabilities.
    Capabilities(crate::cli::cmd::capabilities::Args),
    /// Local profile lifecycle commands.
    Profile(crate::cli::cmd::profile::Args),
    /// List and close CDP targets attached to the host.
    Tabs(crate::cli::cmd::tabs::Args),
    /// Install, remove, or check the embedded Agent Skill (Codex, Claude Code, opencode).
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
        // an invalid_argument envelope. (Top-level `--help`/`--help-markdown`
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
            "--profile-name",
            concat!("profile", "_name"),
            concat!("?", "profile="),
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
