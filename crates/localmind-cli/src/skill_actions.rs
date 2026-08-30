//! Delegating Skills-tab reviewer actions to LocalPilot (LocalHub#41).
//!
//! LocalMind's Skills tab never registers a source or installs a skill itself:
//! every mutation is delegated to LocalPilot's `skills` CLI (issue #40), which owns
//! the confirmation, trust, provenance, and atomicity invariants. This module turns
//! a reviewer decision into the ordered `localpilot skills …` invocations, runs them
//! through an injectable [`CommandRunner`] seam (so the flow is testable without the
//! real binary), keeps an install-from-unregistered atomic (rolling back a fresh
//! registration if the install fails), surfaces repository drift, and advances the
//! proposal's local state. Defer and reject are local state transitions with no
//! delegation.

use localmind_store::{ProposalState, SkillProposal, SkillProposalError, SkillProposalStore};

/// A reviewer action on a proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillAction {
    /// Register the repository as a source (installs nothing).
    AddSource,
    /// Register the repository (if needed) and install the recommended skill.
    InstallOne,
    /// Register the repository (if needed) and install every skill it offers.
    InstallAll,
    /// Keep the proposal for later without acting.
    Defer,
    /// Reject the proposal; discovery must not resurrect it.
    Reject,
}

impl SkillAction {
    /// Parse an action name from the request. Returns `None` for an unknown action.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "add-source" => Some(SkillAction::AddSource),
            "install-one" => Some(SkillAction::InstallOne),
            "install-all" => Some(SkillAction::InstallAll),
            "defer" => Some(SkillAction::Defer),
            "reject" => Some(SkillAction::Reject),
            _ => None,
        }
    }

    /// Whether this action delegates a mutation to LocalPilot (as opposed to a
    /// local state transition).
    #[must_use]
    fn delegates(self) -> bool {
        matches!(
            self,
            SkillAction::AddSource | SkillAction::InstallOne | SkillAction::InstallAll
        )
    }
}

/// The result of running one `localpilot` invocation.
#[derive(Debug, Clone)]
pub struct CommandOutcome {
    pub success: bool,
    /// Combined stdout+stderr, for surfacing to the reviewer and parsing the commit.
    pub output: String,
}

/// A seam over running the `localpilot` binary, so the delegation flow is testable
/// without the real CLI or any network.
pub trait CommandRunner {
    /// Run `localpilot` with `args`. Never panics: a spawn failure is a
    /// non-success [`CommandOutcome`] carrying the error text.
    fn run(&self, args: &[String]) -> CommandOutcome;
}

/// The production runner: spawns the `localpilot` binary found on `PATH`.
pub struct LocalpilotCli;

impl CommandRunner for LocalpilotCli {
    fn run(&self, args: &[String]) -> CommandOutcome {
        match std::process::Command::new("localpilot").args(args).output() {
            Ok(output) => {
                let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
                text.push_str(&String::from_utf8_lossy(&output.stderr));
                CommandOutcome {
                    success: output.status.success(),
                    output: text,
                }
            }
            Err(error) => CommandOutcome {
                success: false,
                output: format!("could not run localpilot: {error} (is it on PATH?)"),
            },
        }
    }
}

/// The outcome of a reviewer action, for the review UI.
#[derive(Debug, Clone, Default)]
pub struct ActionReport {
    /// Whether the action completed successfully.
    pub ok: bool,
    /// Human-readable lines describing what happened (delegated commands, drift,
    /// rollback), surfaced to the reviewer.
    pub messages: Vec<String>,
    /// True if the repository's fetched commit differed from the one recorded at
    /// discovery — the reviewer should re-review before trusting the result.
    pub drift: bool,
}

/// The `-g` scope flag for a proposal's intended scope, if global.
fn scope_flag(scope: &str) -> Option<&'static str> {
    (scope == "global").then_some("-g")
}

/// Build the ordered `localpilot skills …` argument vectors for `action` against a
/// proposal. Empty for a local-only action (defer/reject). Install always registers
/// the source first (the proposal is for an unregistered repository), then installs.
#[must_use]
pub fn localpilot_commands(action: SkillAction, proposal: &SkillProposal) -> Vec<Vec<String>> {
    let url = proposal.repo_url.clone();
    let flag = scope_flag(&proposal.scope);
    let add = || {
        let mut args = vec![
            "skills".to_string(),
            "repo".to_string(),
            "add".to_string(),
            url.clone(),
        ];
        if let Some(f) = flag {
            args.push(f.to_string());
        }
        args.push("--yes".to_string());
        args
    };
    match action {
        SkillAction::AddSource => vec![add()],
        SkillAction::InstallOne => {
            let name = proposal.recommended_skill.clone().unwrap_or_default();
            let mut install = vec![
                "skills".to_string(),
                "install".to_string(),
                name,
                "--repo".to_string(),
                url.clone(),
            ];
            if let Some(f) = flag {
                install.push(f.to_string());
            }
            install.push("--yes".to_string());
            vec![add(), install]
        }
        SkillAction::InstallAll => {
            let mut install = vec![
                "skills".to_string(),
                "install".to_string(),
                "--all".to_string(),
                "--repo".to_string(),
                url.clone(),
            ];
            if let Some(f) = flag {
                install.push(f.to_string());
            }
            install.push("--yes".to_string());
            vec![add(), install]
        }
        SkillAction::Defer | SkillAction::Reject => Vec::new(),
    }
}

/// The `localpilot skills repo delete …` compensating command, used to roll back a
/// registration when a subsequent install fails so an install-from-unregistered
/// stays all-or-nothing.
fn rollback_delete(proposal: &SkillProposal) -> Vec<String> {
    let mut args = vec![
        "skills".to_string(),
        "repo".to_string(),
        "delete".to_string(),
        proposal.repo_url.clone(),
    ];
    if let Some(f) = scope_flag(&proposal.scope) {
        args.push(f.to_string());
    }
    args.push("--yes".to_string());
    args
}

/// Extract the short commit an `added source …` line reports (`… @ <commit> — …`),
/// so a drift against the recorded proposal commit can be surfaced.
fn added_commit(output: &str) -> Option<String> {
    let after = output.split(" @ ").nth(1)?;
    after.split_whitespace().next().map(str::to_string)
}

/// Run a reviewer `action` against `proposal`: delegate any mutation to LocalPilot
/// through `runner`, keep install-from-unregistered atomic, surface drift, and
/// advance the proposal's local state in `store`.
///
/// # Errors
/// Returns [`SkillProposalError`] only if the local state update fails to persist; a
/// failed delegated command is reported in the [`ActionReport`], not as an `Err`.
pub fn run_action(
    runner: &dyn CommandRunner,
    store: &SkillProposalStore,
    action: SkillAction,
    proposal: &SkillProposal,
) -> Result<ActionReport, SkillProposalError> {
    let recommended = proposal.recommended_skill.as_deref();
    let mut report = ActionReport::default();

    // Local-only transitions: no delegation.
    if !action.delegates() {
        let state = match action {
            SkillAction::Defer => ProposalState::Deferred,
            SkillAction::Reject => ProposalState::Rejected,
            _ => unreachable!("only defer/reject are non-delegating"),
        };
        store.set_state(&proposal.repo_url, recommended, &proposal.scope, state)?;
        report.ok = true;
        report.messages.push(format!("proposal {}", state.label()));
        return Ok(report);
    }

    let commands = localpilot_commands(action, proposal);
    let mut registered = false;
    for (index, args) in commands.iter().enumerate() {
        let is_add = args.get(1).map(String::as_str) == Some("repo")
            && args.get(2).map(String::as_str) == Some("add");
        let outcome = runner.run(args);
        report.messages.push(format!(
            "localpilot {}: {}",
            args.join(" "),
            if outcome.success { "ok" } else { "failed" }
        ));
        if is_add && outcome.success {
            registered = true;
            if let Some(commit) = added_commit(&outcome.output) {
                if !proposal.commit.is_empty() && !proposal.commit.starts_with(&commit) {
                    report.drift = true;
                    report.messages.push(format!(
                        "note: repository moved since discovery (recorded {}, now {commit}) — re-review before trusting",
                        proposal.commit
                    ));
                }
            }
        }
        if !outcome.success {
            report.messages.push(outcome.output.trim().to_string());
            // Atomicity: if a later step failed after a fresh registration, roll
            // that registration back so nothing partial remains.
            if index > 0 && registered {
                let rollback = runner.run(&rollback_delete(proposal));
                report.messages.push(format!(
                    "rolled back the new source registration ({})",
                    if rollback.success {
                        "ok"
                    } else {
                        "manual cleanup may be needed"
                    }
                ));
            }
            report.ok = false;
            return Ok(report);
        }
    }

    // Every delegated command succeeded — mark the proposal acted.
    store.set_state(
        &proposal.repo_url,
        recommended,
        &proposal.scope,
        ProposalState::Acted,
    )?;
    report.ok = true;
    report.messages.push("proposal acted".to_string());
    Ok(report)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::cell::RefCell;

    fn proposal(scope: &str) -> SkillProposal {
        SkillProposal {
            repo_url: "https://github.com/freshtechbro/claudedesignskills".to_string(),
            commit: "c0ffee1234".to_string(),
            catalog_root: ".localpilot/skills".to_string(),
            available_skills: vec!["threejs-webgl".to_string(), "other".to_string()],
            recommended_skill: Some("threejs-webgl".to_string()),
            confidence: 0.8,
            reason: "matched".to_string(),
            query: "threejs".to_string(),
            scope: scope.to_string(),
            state: ProposalState::Pending,
            provenance: "github-search".to_string(),
            first_seen: "1".to_string(),
            last_seen: "1".to_string(),
        }
    }

    /// A runner that returns scripted outcomes and records the commands it ran.
    struct FakeRunner {
        outcomes: RefCell<Vec<CommandOutcome>>,
        seen: RefCell<Vec<Vec<String>>>,
    }
    impl FakeRunner {
        fn new(outcomes: Vec<CommandOutcome>) -> Self {
            Self {
                outcomes: RefCell::new(outcomes),
                seen: RefCell::new(Vec::new()),
            }
        }
    }
    impl CommandRunner for FakeRunner {
        fn run(&self, args: &[String]) -> CommandOutcome {
            self.seen.borrow_mut().push(args.to_vec());
            self.outcomes.borrow_mut().remove(0)
        }
    }

    fn ok(output: &str) -> CommandOutcome {
        CommandOutcome {
            success: true,
            output: output.to_string(),
        }
    }
    fn fail(output: &str) -> CommandOutcome {
        CommandOutcome {
            success: false,
            output: output.to_string(),
        }
    }

    fn store_with(proposal: &SkillProposal) -> (tempfile::TempDir, SkillProposalStore) {
        let tmp = tempfile::tempdir().unwrap();
        let store = SkillProposalStore::open(tmp.path());
        // Seed the file so set_state can find the proposal.
        let dir = tmp.path().join(".localpilot");
        std::fs::create_dir_all(&dir).unwrap();
        let file = format!(
            "[[proposal]]\nrepo_url = \"{}\"\nscope = \"{}\"\nstate = \"pending\"\nrecommended_skill = \"threejs-webgl\"\ncommit = \"c0ffee1234\"\n",
            proposal.repo_url, proposal.scope
        );
        std::fs::write(dir.join("skill-proposals.toml"), file).unwrap();
        (tmp, store)
    }

    #[test]
    fn command_building_adds_then_installs_with_scope_flag() {
        let p = proposal("global");
        let one = localpilot_commands(SkillAction::InstallOne, &p);
        assert_eq!(one.len(), 2, "install registers then installs");
        assert_eq!(
            one[0],
            vec!["skills", "repo", "add", &p.repo_url, "-g", "--yes"]
        );
        assert_eq!(
            one[1],
            vec![
                "skills",
                "install",
                "threejs-webgl",
                "--repo",
                &p.repo_url,
                "-g",
                "--yes"
            ]
        );
        let all = localpilot_commands(SkillAction::InstallAll, &proposal("project"));
        assert_eq!(all[1][1], "install");
        assert!(all[1].contains(&"--all".to_string()));
        assert!(
            !all[1].contains(&"-g".to_string()),
            "project scope has no -g"
        );
        // Local-only actions delegate nothing.
        assert!(localpilot_commands(SkillAction::Defer, &p).is_empty());
    }

    #[test]
    fn install_failure_rolls_back_the_registration() {
        let p = proposal("project");
        let (_tmp, store) = store_with(&p);
        // add ok, install fails → expect a rollback delete.
        let runner = FakeRunner::new(vec![
            ok("added source `x` (url) @ c0ffee1234 — 2 skill(s)"),
            fail("install failed: conflict"),
            ok("removed source `x`"),
        ]);
        let report = run_action(&runner, &store, SkillAction::InstallOne, &p).unwrap();
        assert!(!report.ok, "a failed install is not ok");
        let seen = runner.seen.borrow();
        assert_eq!(seen.len(), 3, "add, install, rollback-delete");
        assert_eq!(
            seen[2][2], "delete",
            "the third command rolls back the registration"
        );
        // The proposal was not marked acted.
        assert_eq!(store.list().unwrap()[0].state, ProposalState::Pending);
    }

    #[test]
    fn a_successful_install_marks_the_proposal_acted() {
        let p = proposal("project");
        let (_tmp, store) = store_with(&p);
        let runner = FakeRunner::new(vec![
            ok("added source `x` (url) @ c0ffee1234 — 2 skill(s)"),
            ok("installed 1 skill(s): threejs-webgl"),
        ]);
        let report = run_action(&runner, &store, SkillAction::InstallOne, &p).unwrap();
        assert!(report.ok);
        assert!(!report.drift);
        assert_eq!(store.list().unwrap()[0].state, ProposalState::Acted);
    }

    #[test]
    fn a_moved_repository_is_flagged_as_drift() {
        let p = proposal("project");
        let (_tmp, store) = store_with(&p);
        // The add reports a different commit than recorded → drift.
        let runner = FakeRunner::new(vec![ok("added source `x` (url) @ deadbeef99 — 2 skill(s)")]);
        let report = run_action(&runner, &store, SkillAction::AddSource, &p).unwrap();
        assert!(report.ok);
        assert!(report.drift, "a changed commit must be surfaced as drift");
    }

    #[test]
    fn reject_is_a_local_state_transition_with_no_delegation() {
        let p = proposal("project");
        let (_tmp, store) = store_with(&p);
        let runner = FakeRunner::new(Vec::new());
        let report = run_action(&runner, &store, SkillAction::Reject, &p).unwrap();
        assert!(report.ok);
        assert!(runner.seen.borrow().is_empty(), "reject delegates nothing");
        assert_eq!(store.list().unwrap()[0].state, ProposalState::Rejected);
    }
}
