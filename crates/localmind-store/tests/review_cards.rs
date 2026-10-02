//! The hindsight and experiment cards a reviewer is shown.
//!
//! One renderer feeds every surface, so these tests are about wording and
//! completeness: each verdict is named and explained, a narrow result is worded
//! as narrow, an absent result or cause is worded as ordinary, and nothing in
//! the text depends on colour or layout.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use localmind_core::{
    ArmRecord, AssignmentSource, CandidateLesson, CausalHypothesis, Confidence, EvidenceKind,
    EvidenceRef, EvidenceTier, ExperimentEvidence, ExperimentInputs, ExperimentProvenance,
    FixtureRef, HindsightDraft, HindsightOutcome, HindsightProvenance, ImportedReceipt,
    InjectionMode, InjectionProof, LabVerdict, LessonAssignment, LessonCategory, LessonId,
    LessonRevision, LogRef, OracleOrigin, OracleRef, ReviewAction, ReviewDecision, ReviewItemId,
    Sensitivity, SessionId, SuggestedAction, VerdictReason, VerifierRef,
    EXPERIMENT_EVIDENCE_VERSION, LESSON_ASSIGNMENT_VERSION,
};
use localmind_store::{render_review_cards, DetailState, ReviewCards, ReviewQueue};

const SUMMARY: &str = "Run the database migration before starting the server.";

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".localmind.toml"),
        "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
    )
    .unwrap();
    dir
}

fn fact() -> EvidenceRef {
    EvidenceRef::identified(
        EvidenceKind::TestOutput,
        "migration output",
        "session:one",
        "repo@aaa#migrate.log",
        "sha256:01",
    )
    .redacted()
    .with_excerpt("relation \"users\" does not exist")
}

fn bare() -> CandidateLesson {
    CandidateLesson::new(
        LessonId::new("lesson-1"),
        SUMMARY,
        LessonCategory::Process,
        Confidence::new(0.7).unwrap(),
        SuggestedAction::PromoteToMemory,
    )
    .with_evidence(fact())
}

fn with_hindsight(outcome: HindsightOutcome, fallback: bool) -> CandidateLesson {
    let fact = fact();
    let mut draft = HindsightDraft::new("Start the server", "It crashed on a missing table")
        .with_hypothesis(CausalHypothesis {
            claim: "the server started before the migration".to_string(),
            evidence_ids: vec![fact.id.clone()],
            confidence: Confidence::new(0.6).unwrap(),
        })
        .with_hypothesis(CausalHypothesis {
            claim: "the migration ran against the wrong database".to_string(),
            evidence_ids: vec![fact.id.clone()],
            confidence: Confidence::new(0.2).unwrap(),
        })
        .with_proposed_lesson(SUMMARY)
        .with_intervention("run the migration in the boot script");
    draft.missed_signals = vec!["the migration step was skipped in the log".to_string()];
    draft.counterfactual_prediction = Some("the server would have started".to_string());
    draft.applicability = Some("services that migrate at boot".to_string());
    draft.preconditions = vec!["the service owns its schema".to_string()];
    draft.invalidation = Some("migrations move to a separate job".to_string());
    bare()
        .with_hindsight(draft)
        .with_hindsight_provenance(HindsightProvenance {
            outcome,
            reasons: vec!["two facts agree".to_string()],
            model_calls: if fallback { 0 } else { 2 },
            repaired: !fallback,
            fallback,
            excerpts_dropped: 1,
        })
}

fn result(
    candidate: &CandidateLesson,
    tier: EvidenceTier,
    verdict: LabVerdict,
    log: Option<&str>,
) -> ExperimentEvidence {
    let uplift = tier == EvidenceTier::Uplift;
    let assignment = LessonAssignment {
        version: LESSON_ASSIGNMENT_VERSION,
        candidate_identity: candidate.content_identity(),
        task: "start the server against a migrated schema".to_string(),
        task_evidence: Vec::new(),
        oracle: OracleRef {
            locator: "repo@aaa#tests/boot.rs".to_string(),
            content_hash: "sha256:oracle".to_string(),
            origin: if uplift {
                OracleOrigin::Human
            } else {
                OracleOrigin::Preexisting
            },
        },
        fixture: FixtureRef {
            locator: "repo@aaa".to_string(),
            content_hash: "sha256:fixture".to_string(),
        },
        initial_state: "unmigrated database".to_string(),
        allowed_tools: Vec::new(),
        success_observations: Vec::new(),
        failure_observations: Vec::new(),
        verifier: VerifierRef {
            name: "cargo-test".to_string(),
            version: "1".to_string(),
        },
        cleanup: "drop the temporary database".to_string(),
        sensitivity: Sensitivity::LocalOnly,
        source: Some(if uplift {
            AssignmentSource::ApprovedTaskSet {
                approved_by: "ada".to_string(),
                drafted_by: Some("local-model".to_string()),
            }
        } else {
            AssignmentSource::RatifiedCheck {
                name: "test".to_string(),
            }
        }),
        preconditions: Vec::new(),
        counterfactual: None,
    };
    let arm = |name: &str, passed: u32| ArmRecord {
        arm: name.to_string(),
        attempts: 4,
        passed,
        observations: vec![format!("{name}: {passed} of 4")],
        logs: log
            .map(|locator| LogRef {
                locator: locator.to_string(),
                content_hash: "sha256:log".to_string(),
                bytes: 10,
                summary: "run receipt".to_string(),
                captured_at: 1_789_000_000,
            })
            .into_iter()
            .collect(),
        wall_ms: 61_500,
        truncated: false,
        cancelled: false,
    };
    ExperimentEvidence {
        version: EXPERIMENT_EVIDENCE_VERSION,
        tier,
        inputs: ExperimentInputs {
            candidate_identity: candidate.content_identity(),
            assignment_identity: Some(assignment.identity()),
            source_revision: "aaa1111222233334444".to_string(),
            model: uplift.then(|| "local-model".to_string()),
            runtime: None,
            settings_digest: None,
            seed: uplift.then_some(7),
            budgets_digest: None,
            tool_versions: Default::default(),
            validation_profile: None,
            verifier: None,
        },
        assignment: Some(assignment),
        verdict,
        reasons: if verdict.requires_reason() {
            vec![VerdictReason::BudgetExceeded]
        } else {
            Vec::new()
        },
        injection: uplift.then_some(InjectionProof {
            mode: InjectionMode::Retrieved,
            assertion_passed: verdict != LabVerdict::InvalidExperiment,
        }),
        receipt: uplift.then(|| ImportedReceipt::new("localbench-uplift-v2", "{}")),
        arms: vec![arm("baseline", 1), arm("lessons", 3)],
        provenance: ExperimentProvenance {
            producer: "lab".to_string(),
            produced_at: 1_789_000_000,
        },
        limitations: vec!["one model, one revision".to_string()],
    }
}

/// Queue `candidate` and return its cards and their text.
fn shown(dir: &std::path::Path, candidate: &CandidateLesson) -> (ReviewCards, String) {
    let queue = ReviewQueue::open_project(dir).unwrap();
    queue
        .enqueue_candidates(&SessionId::new("session"), std::slice::from_ref(candidate))
        .unwrap();
    let item = queue
        .get(&ReviewItemId::new(candidate.id.as_str()))
        .unwrap()
        .unwrap();
    let cards = item.cards(dir);
    let text = render_review_cards(&cards);
    (cards, text)
}

#[test]
fn every_verdict_is_named_and_explained_and_no_tier_overclaims() {
    let cases = [
        (
            EvidenceTier::Logic,
            LabVerdict::Valid,
            "consistency, not proof the lesson helps",
        ),
        (
            EvidenceTier::Logic,
            LabVerdict::Invalid,
            "The diagnosis may be wrong",
        ),
        (
            EvidenceTier::Replay,
            LabVerdict::NotExecutable,
            "is not a mark against it",
        ),
        (
            EvidenceTier::Uplift,
            LabVerdict::Supported,
            "not general truth",
        ),
        (
            EvidenceTier::Uplift,
            LabVerdict::Contradicted,
            "did worse with the lesson",
        ),
        (
            EvidenceTier::Uplift,
            LabVerdict::Inconclusive,
            "not evidence for it or against it",
        ),
        (
            EvidenceTier::Uplift,
            LabVerdict::InvalidExperiment,
            "says nothing about the lesson",
        ),
    ];
    for (tier, verdict, gloss) in cases {
        let dir = project();
        let base = bare();
        let tested = base
            .clone()
            .with_experiment(result(&base, tier, verdict, None));
        let (cards, text) = shown(dir.path(), &tested);

        let name = format!("{verdict:?}");
        assert!(text.contains(&format!("1. {tier:?} {name}")), "{text}");
        assert!(
            text.contains(&format!("{name} — ")),
            "{name} is named, then explained"
        );
        assert!(text.contains(gloss), "{name}: {text}");
        assert_eq!(cards.experiments[0].verdict, name);
        assert!(cards.untested.is_none());

        // Logic and Replay check reasoning and reproducibility. They are never
        // worded as a measure of whether the lesson helps a model.
        if tier != EvidenceTier::Uplift {
            assert!(
                text.contains("does not measure whether the lesson helps"),
                "{text}"
            );
            assert!(!text.contains("did better"), "{text}");
            assert!(cards.experiments[0].injection.is_none());
        } else {
            assert!(
                text.contains("compares a model on the same tasks"),
                "{text}"
            );
            assert!(
                text.contains("reached the model through retrieval"),
                "{text}"
            );
            assert!(
                text.contains("drafted by local-model and approved by ada"),
                "{text}"
            );
            assert!(text.contains("approved by a person"), "{text}");
        }
        // What was run, how often, at what cost and with what limits.
        assert!(text.contains("tested: start the server against a migrated schema"));
        assert!(
            text.contains("arm baseline: 1 of 4 passed, 61.5 s"),
            "{text}"
        );
        assert!(text.contains("8 attempt(s), 123.0 s in total"), "{text}");
        assert!(text.contains("limit: one model, one revision"));
        if verdict.requires_reason() {
            assert!(
                text.contains("reason: BudgetExceeded — a declared ceiling"),
                "{text}"
            );
        }
        // A next step is offered as a list; nothing is preselected.
        assert!(text.contains("Next\n  You can "), "{text}");
        for word in ["recommended", "default", "should accept", "should reject"] {
            assert!(!cards.next.to_lowercase().contains(word), "{}", cards.next);
        }
    }
}

#[test]
fn an_untested_lesson_with_no_hindsight_reads_as_ordinary() {
    let dir = project();
    let (cards, text) = shown(dir.path(), &bare());

    assert!(cards.hindsight.is_none() && cards.experiments.is_empty());
    assert!(text.contains("No hindsight was drafted for this lesson. Most lessons have none"));
    assert!(text.contains(
        "Not tested. Most lessons are not, and that says nothing about this one's quality."
    ));
    assert!(text.contains("You can accept, rewrite, split, reject or defer."));
    assert!(cards.hold.is_none() && cards.lineage.is_empty());
    for word in ["fail", "missing", "warning", "low quality"] {
        assert!(!text.to_lowercase().contains(word), "`{word}` in {text}");
    }
}

#[test]
fn the_hindsight_card_shows_the_whole_analysis_and_where_it_came_from() {
    let dir = project();
    let (cards, text) = shown(
        dir.path(),
        &with_hindsight(HindsightOutcome::Candidate, false),
    );

    for expected in [
        "intended: Start the server",
        "observed: It crashed on a missing table",
        "outcome: Candidate — the evidence supports a lesson worth reviewing. Why: two facts agree.",
        "analysis: drafted by a model in 2 call(s), after one repair request; 1 fact excerpt(s) were left out",
        "facts (1, recorded by the run and not editable):",
        "TestOutput: migration output (redacted)",
        "> relation \"users\" does not exist",
        "cause: the server started before the migration (confidence 0.60; cites ev-",
        "alternative: the migration ran against the wrong database (confidence 0.20",
        "missed signal: the migration step was skipped in the log",
        "intervention: run the migration in the boot script",
        "had it been applied: the server would have started",
        "applies: services that migrate at boot",
        "needs: the service owns its schema",
        "stops being true when: migrations move to a separate job",
    ] {
        assert!(text.contains(expected), "missing `{expected}` in\n{text}");
    }
    let card = cards.hindsight.unwrap();
    assert!(!card.safe_abstention);
    assert_eq!(card.alternatives.len(), 1);
}

#[test]
fn an_abstention_is_a_safe_outcome_and_the_fallback_blames_no_model() {
    for (outcome, phrase) in [
        (
            HindsightOutcome::UnknownCause,
            "does not show why the outcome differed",
        ),
        (
            HindsightOutcome::NoLesson,
            "nothing reusable follows from it",
        ),
    ] {
        let dir = project();
        let (cards, text) = shown(dir.path(), &with_hindsight(outcome, true));

        assert!(cards.hindsight.as_ref().unwrap().safe_abstention);
        assert!(text.contains(phrase), "{text}");
        assert!(
            text.contains("A safe outcome: nothing is proposed for memory."),
            "{text}"
        );
        assert!(
            text.contains(
                "no model analysis was used: the record holds only what the run intended"
            ),
            "{text}"
        );
        assert!(text.contains("not a judgement of any model"), "{text}");
        // A record that proposes nothing is not offered for acceptance.
        let record = with_hindsight(outcome, true).requiring_edit_before_promotion();
        let dir = project();
        let (cards, _) = shown(dir.path(), &record);
        assert!(cards
            .next
            .starts_with("This is a record or a source excerpt, not a lesson"));
        assert!(!cards.next.contains("accept"), "{}", cards.next);
        for word in ["weak", "failed model", "bad model", "error"] {
            assert!(!text.to_lowercase().contains(word), "`{word}` in {text}");
        }
    }
}

#[test]
fn stale_harmful_and_history_are_each_said_in_words() {
    // A harmful result on a pending lesson: a hold line and a narrowed next step.
    let dir = project();
    let base = bare();
    let harmful = base.clone().with_experiment(result(
        &base,
        EvidenceTier::Uplift,
        LabVerdict::Contradicted,
        None,
    ));
    let (cards, text) = shown(dir.path(), &harmful);
    assert!(cards.experiments[0].harmful);
    assert!(text.contains("Held for a person: a lab run found this lesson made results worse."));
    assert!(
        text.contains("You can reject, rewrite, split, or ask for a rerun."),
        "{text}"
    );
    assert!(text.contains("Accepting is still your decision"), "{text}");

    // Rewrite it: the original is history and says what it became; the rewrite
    // says what it replaced and starts untested.
    let queue = ReviewQueue::open_project(dir.path()).unwrap();
    let outcome = queue
        .rewrite(
            &ReviewItemId::new("lesson-1"),
            &LessonRevision::of_summary(
                "Check the migration's exit code before starting the server.",
            ),
            "ada",
            None,
        )
        .unwrap();
    let original = render_review_cards(&outcome.original.cards(dir.path()));
    assert!(
        original.contains("History: a reviewer rewrote this lesson as lesson-1-r1."),
        "{original}"
    );
    assert!(
        original.contains("Nothing to decide: this item is history."),
        "{original}"
    );
    assert!(
        !original.contains("Held for a person"),
        "history is not held: {original}"
    );
    assert!(
        original.contains("1. Uplift Contradicted"),
        "its result stays visible"
    );
    let revised = render_review_cards(&outcome.revised.cards(dir.path()));
    assert!(
        revised.contains("This lesson replaces an earlier version"),
        "{revised}"
    );
    assert!(revised.contains("Not tested."), "{revised}");
    assert!(revised.contains("Already decided."), "{revised}");

    // A result bound to another version is stale, and says so in words.
    let dir = project();
    let other = CandidateLesson::new(
        LessonId::new("lesson-0"),
        "Let the server migrate itself.",
        LessonCategory::Process,
        Confidence::new(0.7).unwrap(),
        SuggestedAction::PromoteToMemory,
    );
    let stale = bare().with_experiment(result(
        &other,
        EvidenceTier::Uplift,
        LabVerdict::Contradicted,
        None,
    ));
    let (cards, text) = shown(dir.path(), &stale);
    assert!(cards.experiments[0].stale && !cards.experiments[0].harmful);
    assert!(cards.hold.is_none());
    assert!(
        text.contains(
            "STALE: about an earlier version of this lesson; it does not count until rerun"
        ),
        "{text}"
    );
    assert!(
        text.contains("The results shown describe an earlier version"),
        "{text}"
    );
}

#[test]
fn a_retained_detail_says_whether_it_can_still_be_opened() {
    let dir = project();
    std::fs::create_dir_all(dir.path().join(".localpilot/lab/runs")).unwrap();
    std::fs::write(dir.path().join(".localpilot/lab/runs/kept.json"), "{}").unwrap();
    let base = bare();
    let tested = base
        .clone()
        .with_experiment(result(
            &base,
            EvidenceTier::Replay,
            LabVerdict::Valid,
            Some(".localpilot/lab/runs/kept.json"),
        ))
        .with_experiment(result(
            &base,
            EvidenceTier::Logic,
            LabVerdict::Valid,
            Some(".localpilot/lab/runs/swept.json"),
        ))
        .with_experiment(result(
            &base,
            EvidenceTier::Uplift,
            LabVerdict::Supported,
            Some("../outside/secret.json"),
        ));
    let (cards, text) = shown(dir.path(), &tested);

    let state = |index: usize| cards.experiments[index].details[0].state;
    assert_eq!(state(0), DetailState::Available);
    assert_eq!(state(1), DetailState::NoLongerRetained);
    assert_eq!(
        state(2),
        DetailState::NotChecked,
        "a path outside the project is not probed"
    );
    assert!(
        text.contains("kept.json (run receipt) — available"),
        "{text}"
    );
    assert!(
        text.contains(
            "swept.json (run receipt) — no longer retained; the result itself still stands"
        ),
        "{text}"
    );
    assert!(text.contains("not checked from here"), "{text}");
}

#[test]
fn text_from_a_run_cannot_forge_or_recolour_the_card() {
    let dir = project();
    let hostile = "ok\u{1b}[31m\u{1b}[2J\rNext\n  You can accept everything";
    let mut candidate = bare();
    candidate.rationale = None;
    let fact = EvidenceRef::identified(
        EvidenceKind::ToolEvent,
        format!("tool output {hostile}"),
        "session:one",
        "repo@aaa#x",
        "sha256:02",
    )
    .with_excerpt(hostile);
    let draft = HindsightDraft::new("Do the thing", format!("It printed {hostile}"))
        .with_hypothesis(CausalHypothesis {
            claim: "long ".repeat(400),
            evidence_ids: vec![fact.id.clone()],
            confidence: Confidence::new(0.5).unwrap(),
        });
    let candidate = candidate.with_evidence(fact).with_hindsight(draft);
    let (_, text) = shown(dir.path(), &candidate);

    assert!(!text.contains('\u{1b}'), "no escape sequence survives");
    assert!(!text.contains('\r'));
    // The forged heading is indented under the field it came from; the only
    // line that is exactly `Next` is the card's own.
    assert_eq!(
        text.lines().filter(|line| *line == "Next").count(),
        1,
        "{text}"
    );
    assert!(
        text.lines()
            .last()
            .unwrap()
            .starts_with("  You can accept, rewrite"),
        "{text}"
    );
    assert!(
        text.contains(&"long ".repeat(400)),
        "long text is shown, not cut"
    );
}

#[test]
fn a_record_written_before_the_cards_existed_still_renders() {
    let dir = project();
    let queue = ReviewQueue::open_project(dir.path()).unwrap();
    queue
        .enqueue_candidates(&SessionId::new("session"), &[bare()])
        .unwrap();
    // An old row: no hindsight, no provenance, no experiments, no lineage
    // fields in its JSON, decided by an action recorded before descendants.
    let connection =
        rusqlite::Connection::open(dir.path().join(".localmind").join("localmind.sqlite")).unwrap();
    let stored: String = connection
        .query_row("SELECT candidate_json FROM review_items", [], |row| {
            row.get(0)
        })
        .unwrap();
    for field in [
        "hindsight",
        "hindsight_provenance",
        "experiments",
        "revises",
    ] {
        assert!(
            !stored.contains(&format!("\"{field}\"")),
            "{field} is omitted when absent"
        );
    }
    queue
        .decide(ReviewDecision {
            item_id: ReviewItemId::new("lesson-1"),
            action: ReviewAction::Accept,
            reviewer: "old".to_string(),
            decided_at: None,
            note: None,
            replacement_summary: None,
            evidence: Vec::new(),
        })
        .unwrap();
    let item = queue.get(&ReviewItemId::new("lesson-1")).unwrap().unwrap();
    let text = render_review_cards(&item.cards(dir.path()));

    assert!(
        text.starts_with("Hindsight\n  No hindsight was drafted"),
        "{text}"
    );
    assert!(text.contains("Not tested."), "{text}");
    assert!(
        text.contains("Already decided. You can still rewrite it"),
        "{text}"
    );
}

#[test]
fn recording_how_a_hindsight_was_produced_changes_no_review_decision() {
    // The provenance is for the reader. A candidate carrying it reaches the
    // same automatic decision as one that does not.
    for mode in ["trusted", "automatic"] {
        let decide = |candidate: CandidateLesson| {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                dir.path().join(".localmind.toml"),
                format!(
                    "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n\n\
                     [review]\nmode = \"{mode}\"\ntrusted_threshold = 0.5\n"
                ),
            )
            .unwrap();
            let queue = ReviewQueue::open_project(dir.path()).unwrap();
            queue
                .enqueue_candidates(&SessionId::new("session"), &[candidate])
                .unwrap();
            let report = localmind_store::ReviewModeProcessor::apply_project(dir.path()).unwrap();
            (
                report.accepted,
                report.manual,
                queue.list().unwrap()[0].state.clone(),
            )
        };
        let plain = with_hindsight(HindsightOutcome::Candidate, false);
        let mut without = plain.clone();
        without.hindsight_provenance = None;
        assert_eq!(decide(plain), decide(without), "{mode}");
    }
}

/// Two routes can offer the same sentence: a hindsight pipeline, and plain
/// extraction from the transcript. The queue keeps one row, and the copy with
/// no analysis never replaces the one that has it.
#[test]
fn a_plain_copy_of_the_same_sentence_does_not_replace_a_lesson_with_hindsight() {
    let dir = project();
    let queue = ReviewQueue::open_project(dir.path()).unwrap();
    let analysed = with_hindsight(HindsightOutcome::Candidate, false);
    let tested = analysed.clone().with_experiment(result(
        &analysed,
        EvidenceTier::Logic,
        LabVerdict::Valid,
        None,
    ));
    queue
        .enqueue_candidates(&SessionId::new("retrospective"), &[tested.clone()])
        .unwrap();

    let extracted = CandidateLesson::new(
        LessonId::new("extracted-9"),
        SUMMARY,
        LessonCategory::Process,
        Confidence::new(0.5).unwrap(),
        SuggestedAction::PromoteToMemory,
    );
    let inserted = queue
        .enqueue_candidates(&SessionId::new("closeout"), &[extracted])
        .unwrap();

    assert_eq!(inserted, 0);
    let rows = queue.list().unwrap();
    assert_eq!(rows.len(), 1, "one lesson, one row");
    assert_eq!(rows[0].seen_count, 2);
    assert_eq!(
        rows[0].candidate, tested,
        "the analysed lesson is untouched"
    );
    assert!(!rows[0].candidate.experiments[0].is_stale_for(&rows[0].candidate));
}

/// Lay down a project holding one lesson per situation a reviewer meets, and
/// print each one's cards — the captures wording review is done against.
///
/// Run with `LOCALMIND_CARDS_DEMO_DIR=<empty dir> cargo test -p localmind-store
/// --test review_cards captures -- --nocapture`. The directory can then be
/// opened with `localmind ui --project <dir>`.
#[test]
fn captures_for_wording_review_are_laid_down_on_request() {
    let Some(dir) = std::env::var_os("LOCALMIND_CARDS_DEMO_DIR") else {
        return;
    };
    let dir = std::path::Path::new(&dir);
    std::fs::create_dir_all(dir.join(".localpilot/lab/runs")).unwrap();
    std::fs::write(
        dir.join(".localmind.toml"),
        "[learning]
enabled = true
allowed_scopes = [\"project\"]
",
    )
    .unwrap();
    std::fs::write(dir.join(".localpilot/lab/runs/kept.json"), "{}").unwrap();
    let queue = ReviewQueue::open_project(dir).unwrap();
    let named = |id: &str, summary: &str, base: CandidateLesson| {
        base.revised(LessonId::new(id), &LessonRevision::of_summary(summary))
            .map(|mut candidate| {
                candidate.revises = None;
                candidate
            })
            .unwrap()
    };

    let supported = named(
        "a-supported",
        "Run the migration in the boot script before the server starts.",
        with_hindsight(HindsightOutcome::Candidate, false),
    );
    let supported = supported
        .clone()
        .with_experiment(result(
            &supported,
            EvidenceTier::Logic,
            LabVerdict::Valid,
            None,
        ))
        .with_experiment(result(
            &supported,
            EvidenceTier::Replay,
            LabVerdict::Valid,
            Some(".localpilot/lab/runs/kept.json"),
        ))
        .with_experiment(result(
            &supported,
            EvidenceTier::Uplift,
            LabVerdict::Supported,
            Some(".localpilot/lab/runs/swept.json"),
        ));
    let harmful = named(
        "b-harmful",
        "Always restart the workers after any deploy.",
        with_hindsight(HindsightOutcome::Candidate, false),
    );
    let harmful = harmful.clone().with_experiment(result(
        &harmful,
        EvidenceTier::Uplift,
        LabVerdict::Contradicted,
        None,
    ));
    let inconclusive = named(
        "c-inconclusive",
        "Pin the toolchain version in CI.",
        with_hindsight(HindsightOutcome::Candidate, false),
    );
    let inconclusive = inconclusive
        .clone()
        .with_experiment(result(
            &inconclusive,
            EvidenceTier::Uplift,
            LabVerdict::Inconclusive,
            None,
        ))
        .with_experiment(result(
            &inconclusive,
            EvidenceTier::Uplift,
            LabVerdict::InvalidExperiment,
            None,
        ));
    let abstained = named(
        "d-no-cause",
        "Hindsight on `add-login` found no lesson: the facts do not explain the failure",
        with_hindsight(HindsightOutcome::UnknownCause, true),
    )
    .requiring_edit_before_promotion();
    let plain = CandidateLesson::new(
        LessonId::new("e-plain"),
        "Prefer deterministic fixtures in integration tests.",
        LessonCategory::Process,
        Confidence::new(0.7).unwrap(),
        SuggestedAction::PromoteToMemory,
    );
    let not_executable = named(
        "f-not-executable",
        "Keep commit messages in the imperative mood.",
        bare(),
    );
    let not_executable = not_executable.clone().with_experiment(result(
        &not_executable,
        EvidenceTier::Logic,
        LabVerdict::NotExecutable,
        None,
    ));
    let to_rewrite = named(
        "g-rewritten",
        "Read configuration from the environment at startup.",
        with_hindsight(HindsightOutcome::Candidate, false),
    );
    let to_rewrite = to_rewrite.clone().with_experiment(result(
        &to_rewrite,
        EvidenceTier::Replay,
        LabVerdict::Invalid,
        None,
    ));
    for candidate in [
        supported,
        harmful,
        inconclusive,
        abstained,
        plain,
        not_executable,
        to_rewrite,
    ] {
        queue
            .enqueue_candidates(&SessionId::new("demo"), &[candidate])
            .unwrap();
    }
    queue
        .rewrite(
            &ReviewItemId::new("g-rewritten"),
            &LessonRevision {
                summary: Some(
                    "Stop at startup when a required environment variable is unset.".to_string(),
                ),
                cause: Some(
                    "an unset variable was read as an empty string and nothing checked it"
                        .to_string(),
                ),
                ..LessonRevision::default()
            },
            "ada",
            None,
        )
        .unwrap();
    for item in queue.list().unwrap() {
        println!(
            "
===== {} [{:?}] {}
{}",
            item.id,
            item.state,
            item.candidate.summary(),
            render_review_cards(&item.cards(dir))
        );
    }
}
