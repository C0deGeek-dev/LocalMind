//! Experiment evidence: what was tested, against what, and what it showed.
//!
//! Three things are kept apart here because conflating any two of them is how a
//! lab manufactures support for a lesson it never tested.
//!
//! - **What was tested** is a [`LessonAssignment`]: a task, an independent and
//!   frozen oracle, a fixture, and the rules of the run. Its identity is fixed
//!   before any compared run, so a treatment can never define the criterion
//!   that judges it.
//! - **What the result was bound to** is [`ExperimentInputs`]: the reproducible
//!   identity of the candidate, assignment, fixture, oracle, source revision
//!   and configuration. Change any of them and the evidence is stale.
//! - **What happened** is everything else — attempts, observations, timings,
//!   log references. Those vary between honest reruns and are deliberately
//!   *not* part of identity, so a rerun is comparable rather than "different".
//!
//! Lab output is evidence for a reviewer, never an actor. Nothing in review-mode
//! processing reads these types: a candidate carrying every verdict reaches the
//! same automatic decision as one carrying none.

use crate::{CandidateLesson, EvidenceId};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;

/// Contract version of [`LessonAssignment`].
pub const LESSON_ASSIGNMENT_VERSION: u32 = 1;

/// Contract version of [`ExperimentEvidence`].
pub const EXPERIMENT_EVIDENCE_VERSION: u32 = 1;

/// Prefix on an assignment identity.
pub const ASSIGNMENT_IDENTITY_PREFIX: &str = "asg-";

/// Prefix on an experiment-input identity.
pub const EXPERIMENT_IDENTITY_PREFIX: &str = "exp-";

/// How long a lab run log — check output, per-trial output, verifier output,
/// and the sessions machine-generated trials create — is kept before it may be
/// removed. The evidence that cites a log outlives it: an expired log leaves a
/// digest and a bounded summary behind and never invalidates a verdict.
pub const LAB_LOG_RETENTION_DAYS: u64 = 30;

/// Most observations one arm or assignment list may carry.
pub const MAX_OBSERVATIONS: usize = 16;

/// Character ceiling on one observation, summary, or description. Bulky output
/// belongs in a log referenced by [`LogRef`], not inline.
pub const MAX_OBSERVATION_CHARS: usize = 500;

const ASSIGNMENT_IDENTITY_SCHEME: &str = "localmind.assignment.identity.v1";
const EXPERIMENT_IDENTITY_SCHEME: &str = "localmind.experiment.identity.v1";
const RECEIPT_DIGEST_SCHEME: &str = "localmind.receipt.digest.v1";

/// Which evidence tier produced a result. The tiers have different safety,
/// cost and evidentiary properties, and they may not emit the same verdicts.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum EvidenceTier {
    /// In-process validation of an assignment, oracle and fixture. No model.
    Logic,
    /// Execution of ratified checks in a temporary worktree. Replays a known
    /// trajectory, so it proves the fixture and the fix — never the lesson.
    Replay,
    /// A lesson-off/on A/B with a real actor, imported from LocalBench.
    Uplift,
}

/// What an experiment concluded.
///
/// Split into two families on purpose. `Valid`, `Invalid` and `NotExecutable`
/// say whether an *assignment* is sound; they are all Logic and Replay may
/// emit, because a replayed trajectory makes the same decisions with or without
/// the lesson. `Supported`, `Contradicted` and `Inconclusive` say whether a
/// *lesson* helped, and only an uplift run — a real actor whose choices can
/// differ lesson-on versus lesson-off — can reach them.
///
/// The uplift interpretation is predeclared, before any run: control fails and
/// treatment passes supports the lesson; both pass shows no demonstrated uplift
/// (`Inconclusive`, not support); both fail is insufficient (`Inconclusive`, or
/// `InvalidExperiment` when the assignment could not discriminate); control
/// passes and treatment fails contradicts or harms. Absence of failure is never
/// support. Mapping LocalBench's statistical verdict onto this table belongs to
/// the uplift adapter, which is where that verdict is read.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum LabVerdict {
    /// The assignment, oracle and fixture are sound.
    Valid,
    /// The assignment, oracle or fixture is unsound.
    Invalid,
    /// The lesson cannot be turned into a runnable assignment. A first-class
    /// result: ordinary review still applies.
    NotExecutable,
    /// The lesson measurably helped, with injection proven.
    Supported,
    /// The lesson measurably hurt, with injection proven.
    Contradicted,
    /// A valid uplift run that did not demonstrate an effect either way.
    Inconclusive,
    /// The experiment itself cannot be interpreted — most importantly, the
    /// lesson was not proven to have been injected. Reachable from any tier.
    InvalidExperiment,
}

impl LabVerdict {
    /// Whether `tier` may emit this verdict.
    #[must_use]
    pub fn permitted_for(self, tier: EvidenceTier) -> bool {
        match self {
            Self::InvalidExperiment => true,
            Self::Valid | Self::Invalid | Self::NotExecutable => {
                matches!(tier, EvidenceTier::Logic | EvidenceTier::Replay)
            }
            Self::Supported | Self::Contradicted | Self::Inconclusive => {
                matches!(tier, EvidenceTier::Uplift)
            }
        }
    }

    /// Whether this verdict claims the lesson had an effect. These, and only
    /// these, require passed injection proof.
    #[must_use]
    pub fn claims_efficacy(self) -> bool {
        matches!(self, Self::Supported | Self::Contradicted)
    }

    /// Whether this verdict must say why. A negative or non-result without a
    /// reason code cannot be acted on or rerun deliberately.
    #[must_use]
    pub fn requires_reason(self) -> bool {
        matches!(
            self,
            Self::Invalid | Self::NotExecutable | Self::InvalidExperiment
        )
    }
}

/// Why a verdict was reached. Codes, not prose, so a result can be filtered,
/// counted and rerun on purpose.
///
/// Reading is tolerant: a code this build does not know becomes
/// [`VerdictReason::Other`] carrying its name, so a record written by a newer
/// build still opens in an older one instead of failing to parse.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum VerdictReason {
    /// The fixture could not be materialised.
    FixtureUnavailable,
    /// The oracle carries no frozen content identity.
    OracleMutable,
    /// The oracle was authored from the lesson under test.
    OracleNotIndependent,
    /// No verifier could discriminate success from failure.
    NoDiscriminatingVerifier,
    /// The lesson does not reduce to an executable task.
    NotReplayable,
    /// The lesson was not proven to be in the treatment arm.
    InjectionNotObserved,
    /// The control arm saw the lesson, or the arms differed in more than the
    /// lesson.
    ArmContaminated,
    /// One arm of a pair is missing.
    PartialPair,
    /// A predeclared resource ceiling was breached.
    BudgetExceeded,
    /// The run was cancelled before it finished.
    Cancelled,
    /// The verifier itself failed.
    VerifierFailed,
    /// The lesson states a preference. Nothing can fail it.
    Preference,
    /// The lesson records what someone wants, which only they can confirm.
    HumanIntent,
    /// The lesson is about style, and no ratified check can verify it.
    UnverifiableStyle,
    /// Testing the lesson would take an action with real-world effect.
    UnsafeAction,
    /// No trusted source — a recorded trajectory, a fail/fix pair, a ratified
    /// check — could supply an independent oracle.
    NoTrustedSource,
    /// The change the lesson came from also changed the oracle, so the oracle
    /// cannot judge it.
    OracleChangedByFix,
    /// Anything not yet named. Bounded like an observation.
    Other(String),
}

impl VerdictReason {
    /// The code's name as written, for a unit code.
    fn named(name: &str) -> Option<Self> {
        Some(match name {
            "FixtureUnavailable" => Self::FixtureUnavailable,
            "OracleMutable" => Self::OracleMutable,
            "OracleNotIndependent" => Self::OracleNotIndependent,
            "NoDiscriminatingVerifier" => Self::NoDiscriminatingVerifier,
            "NotReplayable" => Self::NotReplayable,
            "InjectionNotObserved" => Self::InjectionNotObserved,
            "ArmContaminated" => Self::ArmContaminated,
            "PartialPair" => Self::PartialPair,
            "BudgetExceeded" => Self::BudgetExceeded,
            "Cancelled" => Self::Cancelled,
            "VerifierFailed" => Self::VerifierFailed,
            "Preference" => Self::Preference,
            "HumanIntent" => Self::HumanIntent,
            "UnverifiableStyle" => Self::UnverifiableStyle,
            "UnsafeAction" => Self::UnsafeAction,
            "NoTrustedSource" => Self::NoTrustedSource,
            "OracleChangedByFix" => Self::OracleChangedByFix,
            _ => return None,
        })
    }
}

impl<'de> Deserialize<'de> for VerdictReason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        /// The two shapes a code is written in: a bare name, or a one-key map
        /// for a code with a payload.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Written {
            Name(String),
            Tagged(BTreeMap<String, String>),
        }

        Ok(match Written::deserialize(deserializer)? {
            Written::Name(name) => Self::named(&name).unwrap_or(Self::Other(name)),
            Written::Tagged(map) => match map.into_iter().next() {
                Some((key, value)) if key == "Other" => Self::Other(value),
                Some((key, value)) => Self::Other(format!("{key}: {value}")),
                None => Self::Other(String::new()),
            },
        })
    }
}

/// How sensitive an assignment's material is, which decides where its content
/// may travel.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Sensitivity {
    /// May be shared as-is.
    Shareable,
    /// Must pass redaction before it leaves the machine.
    Redacted,
    /// Never leaves the machine.
    LocalOnly,
}

/// Where an oracle came from. Independence is the property that matters: an
/// oracle derived from the lesson under test can be satisfied by that lesson
/// whether or not the lesson is right.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum OracleOrigin {
    /// Existed before the lesson did — a committed test, a ratified check.
    Preexisting,
    /// Written or ratified by a person independently of the lesson.
    Human,
    /// Produced by the lab from the lesson itself. Never independent.
    DerivedFromLesson,
}

/// A frozen, independent success criterion.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OracleRef {
    /// Where the criterion lives (a test path at a revision, a check name).
    pub locator: String,
    /// Content fingerprint taken when the assignment was frozen. Empty means the
    /// oracle can change underneath the result, which is the same as having no
    /// oracle.
    pub content_hash: String,
    pub origin: OracleOrigin,
}

/// The starting material a run is set up from.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FixtureRef {
    pub locator: String,
    pub content_hash: String,
}

/// The verifier that judged a run, by name and version, so a verifier change is
/// an input change.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct VerifierRef {
    pub name: String,
    pub version: String,
}

/// Where an assignment came from. Only trusted sources build assignments: the
/// run's own record, the project's own history, and the project's own ratified
/// checks — never text the lesson wrote.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum AssignmentSource {
    /// A recorded trajectory from the run the lesson came out of: an attempt
    /// that failed, a change, and the same attempt passing. Its observations
    /// are replayed; nothing is executed.
    RecordedTrajectory { session: String },
    /// A step's own commits: the oracle fails at `base_revision` and passes at
    /// `fix_revision`.
    FailFixPair {
        base_revision: String,
        fix_revision: String,
    },
    /// A ratified project check, by name, as the oracle for a lesson whose
    /// evidence cites it failing.
    RatifiedCheck { name: String },
    /// A known repair taken back out of a later revision: `applied_to` with the
    /// changes of `repair_revision` reverted, so the repair is the expected fix.
    ControlledMutation {
        applied_to: String,
        repair_revision: String,
    },
}

/// A runnable test of one lesson, frozen before any compared run.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LessonAssignment {
    pub version: u32,
    /// The [`CandidateLesson::content_identity`] of the lesson under test.
    /// Hindsight lives inside the candidate, so this binds it too.
    pub candidate_identity: String,
    /// What the run is asked to do.
    pub task: String,
    /// The facts the task was drawn from, by id, out of the candidate's evidence.
    #[serde(default)]
    pub task_evidence: Vec<EvidenceId>,
    pub oracle: OracleRef,
    pub fixture: FixtureRef,
    /// The state the run starts from, described.
    pub initial_state: String,
    /// The tools the actor may use. Anything else is out of contract.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// What a successful run is expected to observe.
    #[serde(default)]
    pub success_observations: Vec<String>,
    /// What a failed run is expected to observe.
    #[serde(default)]
    pub failure_observations: Vec<String>,
    pub verifier: VerifierRef,
    /// How the run's effects are undone.
    pub cleanup: String,
    pub sensitivity: Sensitivity,
    /// Where the assignment came from. Absent on assignments written before
    /// the field existed, which therefore keep their identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<AssignmentSource>,
    /// What must hold before the task starts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preconditions: Vec<String>,
    /// The claim under test, in the hindsight's own terms: had the lesson's
    /// change been made, the outcome would have been different in this way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counterfactual: Option<String>,
}

impl LessonAssignment {
    /// This assignment's identity, over its serialized form.
    #[must_use]
    pub fn identity(&self) -> String {
        content_digest(ASSIGNMENT_IDENTITY_SCHEME, ASSIGNMENT_IDENTITY_PREFIX, self)
    }

    /// Check that this assignment is fit to freeze: a frozen fixture and
    /// oracle, an oracle not derived from the lesson, and bounded lists.
    ///
    /// Returns every violation. Soundness of the oracle as a *discriminator* —
    /// that it fails where it should and passes where it should — is a run's to
    /// establish, not this check's.
    ///
    /// # Errors
    /// [`ExperimentViolation`] values describing each breach.
    pub fn validate(&self) -> Result<(), Vec<ExperimentViolation>> {
        let mut violations = Vec::new();
        if self.version != LESSON_ASSIGNMENT_VERSION {
            violations.push(ExperimentViolation::UnsupportedAssignmentVersion {
                version: self.version,
            });
        }
        if self.fixture.content_hash.trim().is_empty() {
            violations.push(ExperimentViolation::UnfrozenFixture);
        }
        if self.oracle.content_hash.trim().is_empty() {
            violations.push(ExperimentViolation::MutableOracle);
        }
        if self.oracle.origin == OracleOrigin::DerivedFromLesson {
            violations.push(ExperimentViolation::OracleNotIndependent);
        }
        for (field, items) in [
            ("success_observations", &self.success_observations),
            ("failure_observations", &self.failure_observations),
            ("preconditions", &self.preconditions),
        ] {
            if items.len() > MAX_OBSERVATIONS {
                violations.push(ExperimentViolation::TooManyItems {
                    field,
                    count: items.len(),
                    limit: MAX_OBSERVATIONS,
                });
            }
            for item in items {
                check_chars(field, item, &mut violations);
            }
        }
        for (field, text) in [
            ("task", Some(self.task.as_str())),
            ("initial_state", Some(self.initial_state.as_str())),
            ("cleanup", Some(self.cleanup.as_str())),
            ("counterfactual", self.counterfactual.as_deref()),
        ] {
            if let Some(text) = text {
                check_chars(field, text, &mut violations);
            }
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(violations)
        }
    }
}

/// Whether the lesson was proven to be present in the treatment arm, and how it
/// got there.
///
/// Forced versus retrieved matters for interpretation, not just for the record:
/// a retrieved arm's null result is confounded with retrieval quality, so a
/// reader must know which one produced it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InjectionProof {
    pub mode: InjectionMode,
    /// The injection assertion held: the lesson reached the treatment arm and
    /// not the control arm.
    pub assertion_passed: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum InjectionMode {
    /// The lesson was placed in context directly.
    Forced,
    /// The lesson was expected to arrive through normal retrieval.
    Retrieved,
}

/// An imported receipt, stored without being re-modelled.
///
/// The receipt's structure belongs to the producer and the shared eval
/// contract crate. LocalMind keeps what it needs to *trust* the bytes — a schema
/// name and a digest — and nothing that would drift from the producer's own
/// definition.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImportedReceipt {
    /// The producer's schema tag, recorded as given.
    pub schema: String,
    /// See [`ImportedReceipt::digest_of`].
    pub digest: String,
    /// The receipt exactly as imported.
    pub payload: String,
}

impl ImportedReceipt {
    /// Record a receipt, computing its digest from the schema and payload.
    #[must_use]
    pub fn new(schema: impl Into<String>, payload: impl Into<String>) -> Self {
        let schema = schema.into();
        let payload = payload.into();
        let digest = Self::digest_of(&schema, &payload);

        Self {
            schema,
            digest,
            payload,
        }
    }

    /// The digest a receipt with this schema and payload must carry.
    #[must_use]
    pub fn digest_of(schema: &str, payload: &str) -> String {
        crate::evidence::digest_parts(&[RECEIPT_DIGEST_SCHEME, schema, payload])
    }

    /// Whether the stored payload is still the one that was imported.
    #[must_use]
    pub fn is_intact(&self) -> bool {
        self.digest == Self::digest_of(&self.schema, &self.payload)
    }
}

/// A reference to bulky output kept outside the evidence record.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LogRef {
    /// Where the log was written. Meaningless once it has expired.
    pub locator: String,
    /// Fingerprint of the log's content, which survives its deletion.
    pub content_hash: String,
    pub bytes: u64,
    /// Bounded summary that survives the log's deletion.
    pub summary: String,
    /// Unix seconds.
    pub captured_at: i64,
}

impl LogRef {
    /// Whether the log is past [`LAB_LOG_RETENTION_DAYS`] at `now` (unix
    /// seconds). An expired log may be gone; the reference and its summary are
    /// still valid evidence.
    ///
    /// A capture time in the future is treated as live: expiring something that
    /// cannot be dated is the wrong direction for an irreversible action.
    #[must_use]
    pub fn is_expired(&self, now: i64) -> bool {
        let retention =
            i64::try_from(LAB_LOG_RETENTION_DAYS.saturating_mul(86_400)).unwrap_or(i64::MAX);
        now.saturating_sub(self.captured_at) > retention
    }
}

/// What one arm of a run did. Measurements, not identity.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArmRecord {
    /// `control`/`treatment` for uplift, or a single arm name for Logic/Replay.
    pub arm: String,
    pub attempts: u32,
    pub passed: u32,
    #[serde(default)]
    pub observations: Vec<String>,
    #[serde(default)]
    pub logs: Vec<LogRef>,
    /// Wall-clock milliseconds across the arm's attempts.
    #[serde(default)]
    pub wall_ms: u64,
    /// Output was cut at a limit.
    #[serde(default)]
    pub truncated: bool,
    /// The arm was stopped before it finished.
    #[serde(default)]
    pub cancelled: bool,
}

/// The reproducible identity a result is bound to. Everything that, if changed,
/// means a rerun is required — and nothing that merely differs between honest
/// reruns.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExperimentInputs {
    /// [`CandidateLesson::content_identity`] of the lesson under test.
    pub candidate_identity: String,
    /// [`LessonAssignment::identity`], or `None` when no assignment could be
    /// built — which is only ever a `NotExecutable` or `InvalidExperiment`.
    pub assignment_identity: Option<String>,
    pub source_revision: String,
    /// Model key, when a model took part.
    #[serde(default)]
    pub model: Option<String>,
    /// Runtime and its version, when one took part.
    #[serde(default)]
    pub runtime: Option<String>,
    /// Digest of the sampling settings.
    #[serde(default)]
    pub settings_digest: Option<String>,
    #[serde(default)]
    pub seed: Option<u64>,
    /// Digest of the predeclared resource budgets.
    #[serde(default)]
    pub budgets_digest: Option<String>,
    /// Tool name to contract version.
    #[serde(default)]
    pub tool_versions: BTreeMap<String, String>,
    /// The ratified validation profile the run used.
    #[serde(default)]
    pub validation_profile: Option<String>,
    #[serde(default)]
    pub verifier: Option<VerifierRef>,
}

/// Where a result came from. Not identity: two honest reruns differ here.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExperimentProvenance {
    /// What produced the result (a runner and its version).
    pub producer: String,
    /// Unix seconds.
    pub produced_at: i64,
}

/// One experiment's evidence about one lesson.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExperimentEvidence {
    pub version: u32,
    pub tier: EvidenceTier,
    pub inputs: ExperimentInputs,
    /// The assignment the run executed, carried with the result so the result
    /// stays interpretable after the lab that produced it is gone.
    #[serde(default)]
    pub assignment: Option<LessonAssignment>,
    pub verdict: LabVerdict,
    #[serde(default)]
    pub reasons: Vec<VerdictReason>,
    /// Required for an efficacy verdict. See [`InjectionProof`].
    #[serde(default)]
    pub injection: Option<InjectionProof>,
    /// The imported uplift receipt, for an uplift result.
    #[serde(default)]
    pub receipt: Option<ImportedReceipt>,
    #[serde(default)]
    pub arms: Vec<ArmRecord>,
    pub provenance: ExperimentProvenance,
    /// Known limits of this result, stated rather than implied.
    #[serde(default)]
    pub limitations: Vec<String>,
}

impl ExperimentEvidence {
    /// The identity this result is bound to: its tier and reproducible inputs,
    /// and nothing that varies between honest reruns.
    #[must_use]
    pub fn identity(&self) -> String {
        content_digest(
            EXPERIMENT_IDENTITY_SCHEME,
            EXPERIMENT_IDENTITY_PREFIX,
            &(self.version, self.tier, &self.inputs),
        )
    }

    /// Whether this result no longer describes `candidate` — the lesson, its
    /// hindsight or its evidence changed after the run. A stale result is kept
    /// and shown, never silently discarded, and never counts until rerun.
    #[must_use]
    pub fn is_stale_for(&self, candidate: &CandidateLesson) -> bool {
        self.inputs.candidate_identity != candidate.content_identity()
    }

    /// Check this result against the candidate it is attached to.
    ///
    /// Returns every violation. An empty result means the record is internally
    /// consistent and bound to this candidate — not that its conclusion is
    /// right.
    ///
    /// # Errors
    /// [`ExperimentViolation`] values describing each contract breach.
    pub fn validate(&self, candidate: &CandidateLesson) -> Result<(), Vec<ExperimentViolation>> {
        let mut violations = Vec::new();

        if self.version != EXPERIMENT_EVIDENCE_VERSION {
            violations.push(ExperimentViolation::UnsupportedVersion {
                version: self.version,
            });
        }
        if self.is_stale_for(candidate) {
            violations.push(ExperimentViolation::StaleCandidate);
        }
        if !self.verdict.permitted_for(self.tier) {
            violations.push(ExperimentViolation::VerdictNotPermittedForTier {
                verdict: self.verdict,
                tier: self.tier,
            });
        }
        if self.verdict.requires_reason() && self.reasons.is_empty() {
            violations.push(ExperimentViolation::MissingReason {
                verdict: self.verdict,
            });
        }

        self.check_injection(&mut violations);
        self.check_receipt(&mut violations);
        self.check_assignment(&mut violations);
        self.check_bounds(&mut violations);

        if violations.is_empty() {
            Ok(())
        } else {
            Err(violations)
        }
    }

    fn check_injection(&self, violations: &mut Vec<ExperimentViolation>) {
        match (self.verdict.claims_efficacy(), self.injection) {
            (true, None) => violations.push(ExperimentViolation::EfficacyWithoutInjectionProof),
            (true, Some(proof)) if !proof.assertion_passed => {
                violations.push(ExperimentViolation::EfficacyWithoutInjectionProof);
            }
            // A failed injection assertion leaves nothing to interpret: it is
            // never "no demonstrated uplift" and never contradiction.
            (false, Some(proof))
                if !proof.assertion_passed && self.verdict != LabVerdict::InvalidExperiment =>
            {
                violations.push(ExperimentViolation::FailedInjectionNotInvalid {
                    verdict: self.verdict,
                });
            }
            _ => {}
        }
    }

    fn check_receipt(&self, violations: &mut Vec<ExperimentViolation>) {
        if let Some(receipt) = &self.receipt {
            if !receipt.is_intact() {
                violations.push(ExperimentViolation::ReceiptDigestMismatch);
            }
        } else if matches!(
            self.verdict,
            LabVerdict::Supported | LabVerdict::Contradicted | LabVerdict::Inconclusive
        ) {
            violations.push(ExperimentViolation::UpliftVerdictWithoutReceipt);
        }
    }

    fn check_assignment(&self, violations: &mut Vec<ExperimentViolation>) {
        let Some(assignment) = &self.assignment else {
            if !matches!(
                self.verdict,
                LabVerdict::NotExecutable | LabVerdict::InvalidExperiment
            ) {
                violations.push(ExperimentViolation::MissingAssignment {
                    verdict: self.verdict,
                });
            }
            return;
        };

        if assignment.version != LESSON_ASSIGNMENT_VERSION {
            violations.push(ExperimentViolation::UnsupportedAssignmentVersion {
                version: assignment.version,
            });
        }
        if self.inputs.assignment_identity.as_deref() != Some(assignment.identity().as_str()) {
            violations.push(ExperimentViolation::AssignmentMismatch);
        }
        if assignment.candidate_identity != self.inputs.candidate_identity {
            violations.push(ExperimentViolation::AssignmentForAnotherCandidate);
        }
        if assignment.fixture.content_hash.trim().is_empty() {
            violations.push(ExperimentViolation::UnfrozenFixture);
        }
        if assignment.oracle.content_hash.trim().is_empty() {
            violations.push(ExperimentViolation::MutableOracle);
        }
        // Soundness verdicts judge the assignment, so an unsound oracle there is
        // a finding the verdict must carry, not a contradiction in the record.
        if assignment.oracle.origin == OracleOrigin::DerivedFromLesson
            && self.verdict != LabVerdict::Invalid
            && self.verdict != LabVerdict::InvalidExperiment
        {
            violations.push(ExperimentViolation::OracleNotIndependent);
        }
    }

    fn check_bounds(&self, violations: &mut Vec<ExperimentViolation>) {
        let mut lists: Vec<(&'static str, &[String])> = vec![("limitations", &self.limitations)];
        for arm in &self.arms {
            lists.push(("observations", &arm.observations));
        }
        if let Some(assignment) = &self.assignment {
            lists.push(("success_observations", &assignment.success_observations));
            lists.push(("failure_observations", &assignment.failure_observations));
        }

        for (field, items) in lists {
            if items.len() > MAX_OBSERVATIONS {
                violations.push(ExperimentViolation::TooManyItems {
                    field,
                    count: items.len(),
                    limit: MAX_OBSERVATIONS,
                });
            }
            for item in items {
                check_chars(field, item, violations);
            }
        }
        for arm in &self.arms {
            for log in &arm.logs {
                check_chars("summary", &log.summary, violations);
            }
        }
        for reason in &self.reasons {
            if let VerdictReason::Other(text) = reason {
                check_chars("reason", text, violations);
            }
        }
    }
}

fn check_chars(field: &'static str, value: &str, violations: &mut Vec<ExperimentViolation>) {
    let chars = value.chars().count();
    if chars > MAX_OBSERVATION_CHARS {
        violations.push(ExperimentViolation::FieldTooLong {
            field,
            chars,
            limit: MAX_OBSERVATION_CHARS,
        });
    }
}

/// SHA-256 over a value's serialized form, domain-separated and prefixed.
fn content_digest<T: Serialize>(scheme: &str, prefix: &str, value: &T) -> String {
    // Every type hashed here holds only serde-infallible fields, so the fallback
    // is unreachable in practice and still varies with content.
    let content = serde_json::to_string(value).unwrap_or_default();
    let digest = crate::evidence::digest_parts(&[scheme, &content]);
    format!("{prefix}{digest}")
}

/// One way an experiment record breaks its contract.
///
/// Every variant is decidable from the record and its candidate alone. None of
/// them judges whether a conclusion is correct.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ExperimentViolation {
    #[error("experiment evidence version {version} is not supported")]
    UnsupportedVersion { version: u32 },
    #[error("lesson assignment version {version} is not supported")]
    UnsupportedAssignmentVersion { version: u32 },
    #[error("the candidate changed after this result was produced; it must be rerun")]
    StaleCandidate,
    #[error("{tier:?} evidence may not emit {verdict:?}")]
    VerdictNotPermittedForTier {
        verdict: LabVerdict,
        tier: EvidenceTier,
    },
    #[error("{verdict:?} must carry a reason code")]
    MissingReason { verdict: LabVerdict },
    #[error("an efficacy verdict requires a passed injection assertion")]
    EfficacyWithoutInjectionProof,
    #[error("a failed injection assertion must be InvalidExperiment, not {verdict:?}")]
    FailedInjectionNotInvalid { verdict: LabVerdict },
    #[error("an uplift verdict requires the imported receipt")]
    UpliftVerdictWithoutReceipt,
    #[error("the imported receipt no longer matches its digest")]
    ReceiptDigestMismatch,
    #[error("{verdict:?} requires the assignment that was executed")]
    MissingAssignment { verdict: LabVerdict },
    #[error("the recorded assignment identity does not match the assignment carried")]
    AssignmentMismatch,
    #[error("the assignment tests a different candidate than this result is bound to")]
    AssignmentForAnotherCandidate,
    #[error("the fixture has no frozen content identity")]
    UnfrozenFixture,
    #[error("the oracle has no frozen content identity")]
    MutableOracle,
    #[error("the oracle was derived from the lesson it judges")]
    OracleNotIndependent,
    #[error("{field} has {count} entries, over the {limit} limit")]
    TooManyItems {
        field: &'static str,
        count: usize,
        limit: usize,
    },
    #[error("{field} is {chars} characters, over the {limit} limit")]
    FieldTooLong {
        field: &'static str,
        chars: usize,
        limit: usize,
    },
}
