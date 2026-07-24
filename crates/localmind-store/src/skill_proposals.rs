//! Reading and updating skill-discovery review proposals (LocalHub#41).
//!
//! A companion tool (LocalPilot) writes review proposals to
//! `<project>/.localpilot/skill-proposals.toml` from its read-only discovery lane.
//! LocalMind's Skills review tab reads them, groups them by repository, and — after
//! a reviewer decision — either advances a proposal's local state (defer/reject) or
//! delegates the mutation back to LocalPilot. This store owns only the on-disk
//! read and state update; it never registers a source or installs a skill, and it
//! is a distinct surface from the memory review queue (skill recommendations are
//! not memory candidates).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The file, relative to a project root, that holds the project-scope proposals.
const PROPOSALS_FILE: &str = "skill-proposals.toml";

/// The lifecycle state of a proposal. LocalPilot's discovery lane only ever writes
/// [`ProposalState::Pending`]; the review surface advances it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProposalState {
    /// Awaiting a reviewer decision.
    Pending,
    /// Deferred by the reviewer — kept for later without action.
    Deferred,
    /// Rejected by the reviewer — discovery must not resurrect it.
    Rejected,
    /// The recommendation (or its source) was acted on from the review surface.
    Acted,
}

impl ProposalState {
    /// A short label for the review UI.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ProposalState::Pending => "pending",
            ProposalState::Deferred => "deferred",
            ProposalState::Rejected => "rejected",
            ProposalState::Acted => "acted",
        }
    }
}

/// A skill-discovery review proposal, mirroring the record LocalPilot writes. Extra
/// fields are tolerated (`serde` ignores unknowns) so a newer producer never breaks
/// the reader.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillProposal {
    /// The normalized repository URL (the primary identity).
    pub repo_url: String,
    /// The resolved snapshot commit at discovery time.
    #[serde(default)]
    pub commit: String,
    /// The selected catalog root label.
    #[serde(default)]
    pub catalog_root: String,
    /// The skill names the repository offers.
    #[serde(default)]
    pub available_skills: Vec<String>,
    /// The primary recommended skill, if any.
    #[serde(default)]
    pub recommended_skill: Option<String>,
    /// Confidence of the recommendation in `0.0..=1.0`.
    #[serde(default)]
    pub confidence: f32,
    /// The rationale behind the recommendation.
    #[serde(default)]
    pub reason: String,
    /// The discovery query that surfaced this repository.
    #[serde(default)]
    pub query: String,
    /// The intended scope for a resulting registration/install (`project`/`global`).
    pub scope: String,
    /// Lifecycle state.
    pub state: ProposalState,
    /// Where the repository was discovered.
    #[serde(default)]
    pub provenance: String,
    /// First and last time this (repo, skill, scope) was seen.
    #[serde(default)]
    pub first_seen: String,
    #[serde(default)]
    pub last_seen: String,
}

impl SkillProposal {
    /// The de-duplication identity shared with the producer: repository URL,
    /// recommended skill, and intended scope.
    #[must_use]
    fn identity(&self) -> (String, String, String) {
        (
            self.repo_url.clone(),
            self.recommended_skill.clone().unwrap_or_default(),
            self.scope.clone(),
        )
    }
}

/// The on-disk shape: a TOML file with a `[[proposal]]` array.
#[derive(Debug, Default, Serialize, Deserialize)]
struct ProposalsFile {
    #[serde(default)]
    proposal: Vec<SkillProposal>,
}

/// Errors reading or updating the proposals file.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SkillProposalError {
    /// The proposals file could not be read or written.
    #[error("skill-proposals io error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    /// The proposals file was not valid TOML in the expected shape.
    #[error("skill-proposals parse error: {0}")]
    Parse(String),
}

/// Read-and-update access to a project's skill-discovery proposals. Backed by the
/// LocalPilot-written TOML file under `<project>/.localpilot/`.
#[derive(Debug, Clone)]
pub struct SkillProposalStore {
    path: PathBuf,
}

impl SkillProposalStore {
    /// Open the store for a project root. Does not require the file to exist — a
    /// missing file is simply an empty proposal set.
    #[must_use]
    pub fn open(project_root: impl AsRef<Path>) -> Self {
        Self {
            path: project_root
                .as_ref()
                .join(".localpilot")
                .join(PROPOSALS_FILE),
        }
    }

    /// The file this store reads, for disclosure in the review UI.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// List every proposal. A missing file is an empty list, not an error.
    ///
    /// # Errors
    /// Returns [`SkillProposalError::Io`] if the file exists but cannot be read, or
    /// [`SkillProposalError::Parse`] if it is not valid proposal TOML.
    pub fn list(&self) -> Result<Vec<SkillProposal>, SkillProposalError> {
        Ok(self.load()?.proposal)
    }

    /// Advance the local state of the proposal identified by (`repo_url`,
    /// `recommended_skill`, `scope`). Returns whether a matching proposal was found
    /// and updated. Used for reviewer transitions that need no LocalPilot mutation
    /// (defer / reject) and to mark a proposal `Acted` after a delegated mutation.
    ///
    /// # Errors
    /// Returns an error if the file cannot be read, parsed, or written.
    pub fn set_state(
        &self,
        repo_url: &str,
        recommended_skill: Option<&str>,
        scope: &str,
        state: ProposalState,
    ) -> Result<bool, SkillProposalError> {
        let key = (
            repo_url.to_string(),
            recommended_skill.unwrap_or_default().to_string(),
            scope.to_string(),
        );
        let mut file = self.load()?;
        let mut updated = false;
        for proposal in &mut file.proposal {
            if proposal.identity() == key {
                proposal.state = state;
                updated = true;
            }
        }
        if updated {
            self.save(&file)?;
        }
        Ok(updated)
    }

    fn load(&self) -> Result<ProposalsFile, SkillProposalError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ProposalsFile::default());
            }
            Err(source) => {
                return Err(SkillProposalError::Io {
                    path: self.path.display().to_string(),
                    source,
                });
            }
        };
        toml::from_str(&text).map_err(|e| SkillProposalError::Parse(e.to_string()))
    }

    fn save(&self, file: &ProposalsFile) -> Result<(), SkillProposalError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| SkillProposalError::Io {
                path: parent.display().to_string(),
                source,
            })?;
        }
        let text = toml::to_string_pretty(file).map_err(|e| {
            SkillProposalError::Parse(format!("could not serialize proposals: {e}"))
        })?;
        std::fs::write(&self.path, text).map_err(|source| SkillProposalError::Io {
            path: self.path.display().to_string(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const SAMPLE: &str = r#"
[[proposal]]
repo_url = "https://github.com/freshtechbro/claudedesignskills"
commit = "c0ffee1234"
catalog_root = ".localpilot/skills"
available_skills = ["threejs-webgl", "gardening"]
recommended_skill = "threejs-webgl"
confidence = 0.85
reason = "matched terms: threejs, materials"
query = "threejs procedural materials"
scope = "project"
state = "pending"
provenance = "github-search"
first_seen = "1000"
last_seen = "1000"
"#;

    #[test]
    fn reads_proposals_and_a_missing_file_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        // Missing file → empty, not an error.
        let store = SkillProposalStore::open(tmp.path());
        assert!(store.list().unwrap().is_empty());

        // Write the LocalPilot-shaped file and read it back.
        let dir = tmp.path().join(".localpilot");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(PROPOSALS_FILE), SAMPLE).unwrap();
        let proposals = store.list().unwrap();
        assert_eq!(proposals.len(), 1);
        let p = &proposals[0];
        assert_eq!(
            p.repo_url,
            "https://github.com/freshtechbro/claudedesignskills"
        );
        assert_eq!(p.recommended_skill.as_deref(), Some("threejs-webgl"));
        assert_eq!(p.available_skills, vec!["threejs-webgl", "gardening"]);
        assert_eq!(p.state, ProposalState::Pending);
    }

    #[test]
    fn set_state_advances_the_matching_proposal_only() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".localpilot");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(PROPOSALS_FILE), SAMPLE).unwrap();
        let store = SkillProposalStore::open(tmp.path());

        // Reject by identity.
        let found = store
            .set_state(
                "https://github.com/freshtechbro/claudedesignskills",
                Some("threejs-webgl"),
                "project",
                ProposalState::Rejected,
            )
            .unwrap();
        assert!(found);
        assert_eq!(store.list().unwrap()[0].state, ProposalState::Rejected);

        // A non-matching identity updates nothing.
        let missing = store
            .set_state(
                "https://github.com/o/other",
                None,
                "project",
                ProposalState::Deferred,
            )
            .unwrap();
        assert!(!missing);
    }

    #[test]
    fn tolerates_unknown_fields_from_a_newer_producer() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".localpilot");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(PROPOSALS_FILE),
            "[[proposal]]\nrepo_url = \"https://github.com/o/r\"\nscope = \"project\"\nstate = \"pending\"\nfuture_field = \"ignored\"\n",
        )
        .unwrap();
        let store = SkillProposalStore::open(tmp.path());
        let proposals = store.list().unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].repo_url, "https://github.com/o/r");
    }
}
