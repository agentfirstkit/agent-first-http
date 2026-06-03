//! `afhttp skill` subcommand. Installs/uninstalls/reports status of the embedded
//! Agent Skill across Codex, Claude Code, and opencode via the shared
//! `agent_first_data::skill` admin.

use clap::{Args as ClapArgs, Subcommand};

use agent_first_data::skill::{
    self, SkillAction, SkillAgentSelection, SkillError, SkillOptions, SkillScope, SkillSpec,
};

use crate::cli::output;
use crate::shared::error::{Error, ErrorCode};

/// The embedded skill this binary installs.
const SPEC: SkillSpec = SkillSpec {
    name: "agent-first-http",
    source: include_str!("../../../skills/agent-first-http.md"),
    title: "Agent-First HTTP",
    marker_slug: "afhttp",
};

#[derive(ClapArgs, Debug)]
pub struct Args {
    #[command(subcommand)]
    pub sub: SkillSub,
}

#[derive(Subcommand, Debug)]
pub enum SkillSub {
    /// Show whether the skill is installed, valid, and up to date.
    Status(TargetArgs),
    /// Install or refresh the skill.
    Install(WriteArgs),
    /// Remove a managed skill.
    Uninstall(WriteArgs),
}

#[derive(ClapArgs, Debug)]
pub struct TargetArgs {
    /// Agent to manage: all, codex, claude-code, opencode.
    #[arg(long, default_value = "all")]
    pub agent: String,
    /// Skill scope: personal or project (project is Claude Code / opencode only).
    #[arg(long, default_value = "personal")]
    pub scope: String,
    /// Skills directory; requires a single concrete --agent.
    #[arg(long = "skills-dir")]
    pub skills_dir: Option<String>,
}

#[derive(ClapArgs, Debug)]
pub struct WriteArgs {
    #[command(flatten)]
    pub target: TargetArgs,
    /// Overwrite or remove a skill this tool did not manage.
    #[arg(long)]
    pub force: bool,
}

pub async fn run(args: Args) -> Result<(), Error> {
    let (action, code, target, force) = match args.sub {
        SkillSub::Status(t) => (SkillAction::Status, "skill_status", t, false),
        SkillSub::Install(w) => (SkillAction::Install, "skill_install", w.target, w.force),
        SkillSub::Uninstall(w) => (SkillAction::Uninstall, "skill_uninstall", w.target, w.force),
    };
    let options = build_options(target, force)?;
    let report = skill::run_skill_admin(&SPEC, action, &options).map_err(to_error)?;
    output::emit(code, &report)
}

/// Parse the `--agent` / `--scope` string flags into the library enums.
fn build_options(target: TargetArgs, force: bool) -> Result<SkillOptions, Error> {
    let agent = match target.agent.as_str() {
        "all" => SkillAgentSelection::All,
        "codex" => SkillAgentSelection::Codex,
        "claude-code" => SkillAgentSelection::ClaudeCode,
        "opencode" => SkillAgentSelection::Opencode,
        other => {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                format!("invalid --agent '{other}': expected all, codex, claude-code, opencode"),
            ))
        }
    };
    let scope = match target.scope.as_str() {
        "personal" => SkillScope::Personal,
        "project" => SkillScope::Project,
        other => {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                format!("invalid --scope '{other}': expected personal, project"),
            ))
        }
    };
    Ok(SkillOptions {
        agent,
        scope,
        skills_dir: target.skills_dir,
        force,
    })
}

/// afhttp's Error has no hint field, so fold the skill hint into the detail.
fn to_error(err: SkillError) -> Error {
    let detail = match err.hint {
        Some(hint) => format!("{} ({hint})", err.message),
        None => err.message,
    };
    Error::new(ErrorCode::InvalidArgument, detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_skills_dir(tag: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("afhttp_skill_{tag}_{suffix}"))
    }

    #[test]
    fn build_options_parses_and_rejects() {
        let ok = build_options(
            TargetArgs {
                agent: "opencode".into(),
                scope: "project".into(),
                skills_dir: Some("/tmp/x".into()),
            },
            true,
        )
        .unwrap();
        assert_eq!(ok.agent, SkillAgentSelection::Opencode);
        assert_eq!(ok.scope, SkillScope::Project);
        assert!(ok.force);

        let bad = build_options(
            TargetArgs {
                agent: "emacs".into(),
                scope: "personal".into(),
                skills_dir: None,
            },
            false,
        );
        assert_eq!(bad.unwrap_err().error_code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn install_status_uninstall_roundtrip() {
        let dir = temp_skills_dir("opencode");
        let options = SkillOptions {
            agent: SkillAgentSelection::Opencode,
            scope: SkillScope::Personal,
            skills_dir: Some(dir.to_string_lossy().into_owned()),
            force: false,
        };

        skill::run_skill_admin(&SPEC, SkillAction::Install, &options).unwrap();
        let skill_path = dir.join("agent-first-http").join("SKILL.md");
        assert!(skill_path.is_file());

        let report = skill::run_skill_admin(&SPEC, SkillAction::Status, &options).unwrap();
        let status = serde_json::to_value(&report).unwrap();
        assert_eq!(status["installed_all"], true);
        assert_eq!(status["valid_all"], true);
        assert_eq!(status["current_all"], true);
        assert_eq!(status["targets"][0]["agent"], "opencode");

        skill::run_skill_admin(&SPEC, SkillAction::Uninstall, &options).unwrap();
        assert!(!skill_path.exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
