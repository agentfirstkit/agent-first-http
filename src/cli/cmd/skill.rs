//! `afhttp skill` subcommand. Installs/uninstalls/reports status of the embedded
//! Agent Skill across Codex, Claude Code, opencode, and Hermes via the shared
//! `agent_first_data::skill` admin.

use agent_first_data::skill::{
    self, SkillAction, SkillAgentSelection, SkillError, SkillOptions, SkillScope, SkillSpec,
};

use crate::cli::output;
use crate::shared::error::{Error, ErrorCode};

/// The embedded skill this binary installs.
const SPEC: SkillSpec = SkillSpec {
    name: "agent-first-http",
    source: include_str!("../../../skills/agent-first-http/SKILL.md"),
    title: "Agent-First HTTP",
    marker_slug: "afhttp",
    assets: &[],
};

#[derive(Debug)]
pub struct Args {
    pub sub: SkillSub,
}

#[derive(Debug)]
pub enum SkillSub {
    Status(TargetArgs),
    Install(WriteArgs),
    Uninstall(WriteArgs),
}

/// `--agent` and `--scope` arrive as registry-checked enum values, so the
/// selector types below map from a closed set rather than parsing one.
#[derive(Debug)]
pub struct TargetArgs {
    pub agent: String,
    pub scope: String,
    pub skills_dir: Option<String>,
}

#[derive(Debug)]
pub struct WriteArgs {
    pub target: TargetArgs,
    pub force: bool,
}

pub async fn run(args: Args) -> Result<(), Error> {
    let (action, code, target, force) = match args.sub {
        SkillSub::Status(t) => (SkillAction::Status, "skill_status", t, false),
        SkillSub::Install(w) => (SkillAction::Install, "skill_install", w.target, w.force),
        SkillSub::Uninstall(w) => (SkillAction::Uninstall, "skill_uninstall", w.target, w.force),
    };
    let options = build_options(target, force);
    let report = skill::run_skill_admin(&SPEC, action, &options).map_err(to_error)?;
    output::emit(code, &report)
}

/// Convert the `--agent` / `--scope` flags into the library options.
fn build_options(target: TargetArgs, force: bool) -> SkillOptions {
    SkillOptions {
        agent: match target.agent.as_str() {
            "codex" => SkillAgentSelection::Codex,
            "claude-code" => SkillAgentSelection::ClaudeCode,
            "opencode" => SkillAgentSelection::Opencode,
            "hermes" => SkillAgentSelection::Hermes,
            _ => SkillAgentSelection::All,
        },
        scope: match target.scope.as_str() {
            "workspace" => SkillScope::Workspace,
            _ => SkillScope::Personal,
        },
        skills_dir: target.skills_dir,
        force,
    }
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
    fn build_options_maps_every_registry_value() {
        for (agent, expected) in [
            ("all", SkillAgentSelection::All),
            ("codex", SkillAgentSelection::Codex),
            ("claude-code", SkillAgentSelection::ClaudeCode),
            ("opencode", SkillAgentSelection::Opencode),
            ("hermes", SkillAgentSelection::Hermes),
        ] {
            let options = build_options(
                TargetArgs {
                    agent: agent.into(),
                    scope: "personal".into(),
                    skills_dir: None,
                },
                false,
            );
            assert_eq!(options.agent, expected, "{agent}");
        }

        let ok = build_options(
            TargetArgs {
                agent: "opencode".into(),
                scope: "workspace".into(),
                skills_dir: Some("/tmp/x".into()),
            },
            true,
        );
        assert_eq!(ok.scope, SkillScope::Workspace);
        assert!(ok.force);
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
