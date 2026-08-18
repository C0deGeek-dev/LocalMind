//! Import a directory of Claude Code memory files into the review queue.
//!
//! Claude Code keeps per-project memory as Markdown files with YAML front
//! matter (`name`, `description`, a `type` under `metadata:`, then a body).
//! That knowledge never reached LocalMind. This importer reads such a directory
//! and enqueues each file as a **review candidate** — exactly like transcript
//! closeout and the OKF importer, it is:
//!
//! - never written straight to accepted memory (D-LM-0016 — David reviews);
//! - redacted before it is stored (the same [`Redactor`](crate::Redactor) the
//!   propose path and transcript ingest use), so a secret in a note never lands
//!   in the shared store;
//! - deduplicated against pending candidates, so re-running an import is
//!   idempotent;
//! - labelled with a `source` so the reviewer sees where it came from.
//!
//! A file without parseable front matter (or the `MEMORY.md` index) is skipped,
//! never fatal. `apply = false` predicts what an apply would enqueue.

use std::path::{Path, PathBuf};

use localmind_core::{
    CandidateLesson, Confidence, EvidenceKind, EvidenceRef, LessonId, ReviewState, SessionId,
    SuggestedAction,
};

use crate::{dedup, ReviewQueue, ReviewQueueError};

/// Outcome of a memory-directory import (or a dry-run prediction).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentMemoryImportReport {
    /// Files that parsed into a candidate.
    pub total: usize,
    /// Candidates newly enqueued (or that a dry run would enqueue).
    pub added: usize,
    /// Candidates that duplicated a pending one.
    pub duplicate: usize,
    /// Files skipped (no front matter, or the index file).
    pub skipped: usize,
    /// Whether the queue was actually written.
    pub applied: bool,
}

/// Imports a directory of agent memory files into one project's review queue.
pub struct AgentMemoryImporter {
    project_root: PathBuf,
    excluded_paths: Vec<String>,
}

impl AgentMemoryImporter {
    /// Open an importer for `project_root`. Redaction reuses the project's
    /// configured sensitive paths when the config is readable (best-effort: an
    /// unreadable config just means no path-specific redaction, standard secret
    /// patterns still apply).
    #[must_use]
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        let project_root = project_root.into();
        let excluded_paths = crate::ProjectConfig::discover(&project_root)
            .map(|config| config.config.learning.excluded_paths.clone())
            .unwrap_or_default();
        Self {
            project_root,
            excluded_paths,
        }
    }

    /// Read a directory of Claude Code memory files and enqueue each as a review
    /// candidate under `source`. With `apply = false` nothing is written and the
    /// report predicts what an apply would do.
    ///
    /// # Errors
    /// [`ReviewQueueError`] if the directory cannot be read or the review queue
    /// cannot be opened or written.
    pub fn import(
        &self,
        memory_dir: &Path,
        source: &str,
        apply: bool,
    ) -> Result<AgentMemoryImportReport, ReviewQueueError> {
        let mut files = Vec::new();
        collect_markdown(memory_dir, &mut files);
        files.sort();

        let redactor = crate::Redactor::new(self.excluded_paths.clone());
        let mut skipped = 0;
        let mut candidates = Vec::new();
        for path in &files {
            // The index is a table of contents, not a memory.
            if path.file_name().is_some_and(|name| name == "MEMORY.md") {
                skipped += 1;
                continue;
            }
            let Ok(text) = std::fs::read_to_string(path) else {
                skipped += 1;
                continue;
            };
            match parse_memory_file(&text) {
                Some(parsed) => candidates.push(to_candidate(&parsed, path, source, &redactor)),
                None => skipped += 1,
            }
        }
        self.enqueue_or_predict(&candidates, source, apply, skipped)
    }

    /// Read `lessons.md`-style bullet lists (LocalHub `plans/**/lessons.md`) and
    /// enqueue each top-level `- ` bullet as a review candidate. `path` may be a
    /// single file or a directory tree (every `lessons.md` under it is read).
    ///
    /// # Errors
    /// [`ReviewQueueError`] if the review queue cannot be opened or written.
    pub fn import_lessons(
        &self,
        path: &Path,
        source: &str,
        apply: bool,
    ) -> Result<AgentMemoryImportReport, ReviewQueueError> {
        let mut files = Vec::new();
        if path.is_dir() {
            collect_lessons_files(path, &mut files);
        } else {
            files.push(path.to_path_buf());
        }
        files.sort();

        let redactor = crate::Redactor::new(self.excluded_paths.clone());
        let mut skipped = 0;
        let mut candidates = Vec::new();
        for file in &files {
            let Ok(text) = std::fs::read_to_string(file) else {
                skipped += 1;
                continue;
            };
            for bullet in parse_lesson_bullets(&text) {
                candidates.push(lesson_to_candidate(&bullet, file, source, &redactor));
            }
        }
        self.enqueue_or_predict(&candidates, source, apply, skipped)
    }

    /// Shared tail: dry-run prediction, or a real enqueue, over already-built
    /// candidates.
    fn enqueue_or_predict(
        &self,
        candidates: &[CandidateLesson],
        source: &str,
        apply: bool,
        skipped: usize,
    ) -> Result<AgentMemoryImportReport, ReviewQueueError> {
        let total = candidates.len();
        if !apply {
            let (added, duplicate) = self.predict(candidates)?;
            return Ok(AgentMemoryImportReport {
                total,
                added,
                duplicate,
                skipped,
                applied: false,
            });
        }
        let queue = ReviewQueue::open_project(&self.project_root)?;
        let session = SessionId::new(format!("import-{source}"));
        let added = queue.enqueue_candidates(&session, candidates)?;
        Ok(AgentMemoryImportReport {
            total,
            added,
            duplicate: total - added,
            skipped,
            applied: true,
        })
    }

    /// Predict `(added, duplicate)` without writing, replaying the enqueue dedup
    /// ladder against the current pending candidates and earlier candidates in
    /// this batch.
    fn predict(&self, candidates: &[CandidateLesson]) -> Result<(usize, usize), ReviewQueueError> {
        let queue = ReviewQueue::open_project(&self.project_root)?;
        let mut seen: Vec<(String, String)> = queue
            .list()?
            .into_iter()
            .filter(|item| item.state == ReviewState::Pending)
            .map(|item| {
                let summary = item.candidate.summary().to_string();
                (dedup::canonical_hash(&summary), summary)
            })
            .collect();

        let mut added = 0;
        let mut duplicate = 0;
        for candidate in candidates {
            let summary = candidate.summary();
            let hash = dedup::canonical_hash(summary);
            let is_dup = seen.iter().any(|(seen_hash, seen_summary)| {
                seen_hash == &hash || dedup::is_near_duplicate(seen_summary, summary)
            });
            if is_dup {
                duplicate += 1;
            } else {
                added += 1;
                seen.push((hash, summary.to_string()));
            }
        }
        Ok((added, duplicate))
    }
}

/// The fields lifted from one Claude Code memory file.
struct ParsedMemory {
    /// The one-line summary (front-matter `description`, else `name`).
    summary: String,
    /// The full note body (after the front matter).
    body: String,
    /// The `type` value under `metadata:` (or top-level), if any.
    kind: Option<String>,
}

/// Parse a Claude Code memory file: YAML front matter delimited by `---` lines,
/// then a Markdown body. Returns `None` when there is no front matter or no
/// usable summary — those files are skipped, never a hard error.
fn parse_memory_file(text: &str) -> Option<ParsedMemory> {
    let mut lines = text.lines();
    // The file must open with a front-matter fence (allowing a leading BOM).
    let first = lines.next()?.trim_start_matches('\u{feff}').trim_end();
    if first != "---" {
        return None;
    }

    let mut name = None;
    let mut description = None;
    let mut kind = None;
    let mut fence_closed = false;
    let mut consumed = first.len() + 1;
    for line in lines.by_ref() {
        consumed += line.len() + 1;
        if line.trim_end() == "---" {
            fence_closed = true;
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim().trim_matches('"').trim();
            if value.is_empty() {
                continue;
            }
            match key {
                "name" if name.is_none() => name = Some(value.to_string()),
                "description" if description.is_none() => description = Some(value.to_string()),
                "type" if kind.is_none() => kind = Some(value.to_string()),
                _ => {}
            }
        }
    }
    if !fence_closed {
        return None;
    }

    let summary = description.or(name)?;
    let body = text.get(consumed..).unwrap_or("").trim().to_string();
    Some(ParsedMemory {
        summary,
        body,
        kind,
    })
}

/// Map one parsed memory to a redacted, review-queued candidate.
fn to_candidate(
    parsed: &ParsedMemory,
    path: &Path,
    source: &str,
    redactor: &crate::Redactor,
) -> CandidateLesson {
    let summary = redactor.redact(&parsed.summary).redacted_text;
    let body = redactor.redact(&parsed.body).redacted_text;
    let category_name = parsed.kind.as_deref().unwrap_or("Process");
    let category = crate::markdown::parse_category(category_name);
    // Content-derived id so a re-import maps to the same row even before the
    // summary dedup runs.
    let id = LessonId::new(format!(
        "memimport-{}",
        &crate::digest_hex(format!("{source}|{summary}|{body}").as_bytes())[..16]
    ));
    let mut candidate = CandidateLesson::new(
        id,
        summary,
        category,
        Confidence::clamped(0.7, 0.7),
        SuggestedAction::PromoteToMemory,
    )
    .with_source(source);
    if !body.is_empty() {
        candidate.rationale = Some(body);
    }
    let note = format!("imported from {}", path.display());
    candidate =
        candidate.with_evidence(EvidenceRef::new(EvidenceKind::ManualNote, note).redacted());
    candidate
}

/// Collect `.md` files under `dir` (one level; memory dirs are flat). A missing
/// directory yields no files rather than an error.
fn collect_markdown(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "md") {
            out.push(path);
        }
    }
}

/// Recursively collect files named `lessons.md` under `dir`.
fn collect_lessons_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_lessons_files(&path, out);
        } else if path.file_name().is_some_and(|name| name == "lessons.md") {
            out.push(path);
        }
    }
}

/// Extract each top-level `- ` bullet from a `lessons.md` file, joining
/// continuation (indented) lines into one lesson. Link-list entries (a bullet
/// that is just a `[title](path)` pointer, as in a generated discovery index)
/// and very short fragments are dropped.
fn parse_lesson_bullets(text: &str) -> Vec<String> {
    let mut bullets = Vec::new();
    let mut current: Option<String> = None;
    let flush = |current: &mut Option<String>, bullets: &mut Vec<String>| {
        if let Some(bullet) = current.take() {
            let bullet = bullet.trim().to_string();
            let is_link_entry = bullet.starts_with('[') && bullet.contains("](");
            if bullet.chars().count() >= 20 && !is_link_entry {
                bullets.push(bullet);
            }
        }
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("- ") {
            flush(&mut current, &mut bullets);
            current = Some(rest.trim().to_string());
        } else if (line.starts_with("  ") || line.starts_with('\t')) && current.is_some() {
            let cont = line.trim();
            if !cont.is_empty() {
                if let Some(bullet) = current.as_mut() {
                    bullet.push(' ');
                    bullet.push_str(cont);
                }
            }
        } else {
            // A header, blank line, or non-bullet paragraph ends the bullet.
            flush(&mut current, &mut bullets);
        }
    }
    flush(&mut current, &mut bullets);
    bullets
}

/// One lesson bullet → a redacted review candidate. The candidate summary is the
/// bullet's first sentence (or a word-bounded truncation); the full bullet is the
/// rationale.
fn lesson_to_candidate(
    bullet: &str,
    path: &Path,
    source: &str,
    redactor: &crate::Redactor,
) -> CandidateLesson {
    let full = redactor.redact(bullet).redacted_text;
    let summary = lesson_summary(&full);
    let id = LessonId::new(format!(
        "lessonimport-{}",
        &crate::digest_hex(format!("{source}|{full}").as_bytes())[..16]
    ));
    let mut candidate = CandidateLesson::new(
        id,
        summary.clone(),
        crate::markdown::parse_category("Process"),
        Confidence::clamped(0.7, 0.7),
        SuggestedAction::PromoteToMemory,
    )
    .with_source(source);
    if full != summary {
        candidate.rationale = Some(full);
    }
    let note = format!("imported from {}", path.display());
    candidate =
        candidate.with_evidence(EvidenceRef::new(EvidenceKind::ManualNote, note).redacted());
    candidate
}

/// A concise one-line summary for a lesson bullet: the first sentence when it is
/// a reasonable length, else a word-bounded truncation.
fn lesson_summary(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(index) = trimmed.find(". ") {
        if index >= 20 {
            return trimmed[..=index].trim().to_string();
        }
    }
    if trimmed.chars().count() <= 160 {
        return trimmed.to_string();
    }
    let mut out = String::new();
    for word in trimmed.split_whitespace() {
        if out.chars().count() + word.chars().count() + 1 > 160 {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    format!("{out}…")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn project(dir: &Path) {
        std::fs::write(
            dir.join(".localmind.toml"),
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
        )
        .unwrap();
    }

    #[test]
    fn parses_front_matter_and_body() {
        let text = "---\nname: my-lesson\ndescription: Prefer typed errors\nmetadata:\n  type: feedback\n---\n\nReturn a Result, never unwrap.";
        let parsed = parse_memory_file(text).expect("parses");
        assert_eq!(parsed.summary, "Prefer typed errors");
        assert_eq!(parsed.kind.as_deref(), Some("feedback"));
        assert_eq!(parsed.body, "Return a Result, never unwrap.");
    }

    #[test]
    fn a_file_without_front_matter_is_skipped() {
        assert!(parse_memory_file("just a note, no front matter").is_none());
        assert!(parse_memory_file("---\nname: x\n(never closes)").is_none());
    }

    #[test]
    fn imports_a_directory_as_pending_candidates_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        project(dir.path());
        let mem = dir.path().join("memory");
        std::fs::create_dir_all(&mem).unwrap();
        std::fs::write(
            mem.join("a.md"),
            "---\ndescription: Keep retry loops bounded\nmetadata:\n  type: feedback\n---\nStop after the attempt budget.",
        )
        .unwrap();
        std::fs::write(
            mem.join("b.md"),
            "---\nname: fixtures\ndescription: Prefer deterministic fixtures\n---\nSeed the RNG.",
        )
        .unwrap();
        // The index and a front-matter-less file are both skipped.
        std::fs::write(mem.join("MEMORY.md"), "- [a](a.md)\n").unwrap();
        std::fs::write(mem.join("c.md"), "no front matter here").unwrap();

        let importer = AgentMemoryImporter::new(dir.path());

        let dry = importer.import(&mem, "claude-code-memory", false).unwrap();
        assert_eq!(dry.total, 2);
        assert_eq!(dry.added, 2);
        assert_eq!(dry.skipped, 2);
        assert!(!dry.applied);

        let applied = importer.import(&mem, "claude-code-memory", true).unwrap();
        assert_eq!(applied.added, 2);
        assert!(applied.applied);

        // Every imported candidate is pending with the right source.
        let queue = ReviewQueue::open_project(dir.path()).unwrap();
        let items = queue.list().unwrap();
        assert_eq!(items.len(), 2);
        for item in &items {
            assert_eq!(item.state, ReviewState::Pending);
            assert_eq!(item.candidate.source.as_deref(), Some("claude-code-memory"));
        }

        // Re-import is idempotent: nothing new is added.
        let again = importer.import(&mem, "claude-code-memory", true).unwrap();
        assert_eq!(again.added, 0);
        assert_eq!(again.duplicate, 2);
        assert_eq!(queue.list().unwrap().len(), 2);
    }

    #[test]
    fn parses_top_level_bullets_joining_continuations_and_dropping_links() {
        let text = "# Lessons\n\n- A function documented and tested but with no caller\n  is a live bug, not spare capacity.\n- [Some plan](plan.md) — a link-list entry that is not a lesson\n- x\n- Read only the header of a big binary; never the tensor data.\n";
        let bullets = parse_lesson_bullets(text);
        assert_eq!(bullets.len(), 2, "{bullets:?}");
        assert!(bullets[0].contains("live bug, not spare capacity"));
        assert!(bullets[0].contains("no caller is a live bug"));
        assert!(bullets[1].starts_with("Read only the header"));
    }

    #[test]
    fn imports_a_lessons_file_as_pending_candidates() {
        let dir = tempfile::tempdir().unwrap();
        project(dir.path());
        let lessons = dir.path().join("lessons.md");
        std::fs::write(
            &lessons,
            "- Prefer typed errors over panics on library paths, always.\n- Pin the toolchain so CI and local agree on lints.\n",
        )
        .unwrap();

        let importer = AgentMemoryImporter::new(dir.path());
        let applied = importer
            .import_lessons(&lessons, "localhub-lessons", true)
            .unwrap();
        assert_eq!(applied.added, 2);

        let queue = ReviewQueue::open_project(dir.path()).unwrap();
        let items = queue.list().unwrap();
        assert_eq!(items.len(), 2);
        assert!(items
            .iter()
            .all(|item| item.candidate.source.as_deref() == Some("localhub-lessons")));

        // Idempotent re-run.
        let again = importer
            .import_lessons(&lessons, "localhub-lessons", true)
            .unwrap();
        assert_eq!(again.added, 0);
    }

    #[test]
    fn a_secret_in_a_memory_is_redacted_before_the_queue() {
        let dir = tempfile::tempdir().unwrap();
        project(dir.path());
        let mem = dir.path().join("memory");
        std::fs::create_dir_all(&mem).unwrap();
        std::fs::write(
            mem.join("leak.md"),
            "---\ndescription: A note with a key\n---\nThe token is sk-proj-abcdef0123456789abcdef0123456789abcdef0123456789.",
        )
        .unwrap();

        let importer = AgentMemoryImporter::new(dir.path());
        importer.import(&mem, "claude-code-memory", true).unwrap();
        let queue = ReviewQueue::open_project(dir.path()).unwrap();
        let item = &queue.list().unwrap()[0];
        let rationale = item.candidate.rationale.clone().unwrap_or_default();
        assert!(
            !rationale.contains("sk-proj-abcdef0123456789"),
            "secret survived import: {rationale}"
        );
    }
}
