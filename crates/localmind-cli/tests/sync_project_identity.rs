//! The project key a sync exchange depends on must be visible where the user
//! checks their sync setup, and a weak one must say so.
//!
//! `[sync] project_key` is documented as the path-independent project identity.
//! Before LocalHub#87 nothing read it: `ProjectIdentity::resolve` had no
//! production caller, so the key was parsed, validated, and then ignored — the
//! same defect class the issue names for `foreign_env_weight`. This test binds
//! `.localmind.toml` (and the git remote it falls back to) to what
//! `localmind sync status` prints, so the resolution cannot silently detach
//! again.

use assert_cmd::Command;
use std::error::Error;
use std::fs;
use std::path::Path;

fn write_config(project: &Path, extra: &str) -> Result<(), Box<dyn Error>> {
    fs::write(
        project.join(".localmind.toml"),
        format!("[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n{extra}"),
    )?;
    Ok(())
}

fn sync_status(project: &Path, folder: &Path, json: bool) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut command = Command::cargo_bin("localmind")?;
    command
        .arg("sync")
        .arg("status")
        .arg("--project")
        .arg(project)
        .arg("--folder")
        .arg(folder);
    if json {
        command.arg("--json");
    }
    let output = command.output()?;
    assert!(output.status.success(), "{output:?}");
    Ok(output.stdout)
}

fn sync_status_text(project: &Path, folder: &Path) -> Result<String, Box<dyn Error>> {
    Ok(String::from_utf8(sync_status(project, folder, false)?)?)
}

#[test]
fn sync_status_reports_the_project_key_and_flags_a_weak_one() -> Result<(), Box<dyn Error>> {
    let project = tempfile::tempdir()?;
    let folder = tempfile::tempdir()?;

    // 1. No pinned key and no git remote: the directory-name fallback, reported
    //    as the weak source it is.
    write_config(project.path(), "")?;
    let weak = sync_status_text(project.path(), folder.path())?;
    assert!(
        weak.contains("(directory_name)"),
        "the fallback source must be named: {weak}"
    );
    assert!(
        weak.contains("weak"),
        "a fallback key must be flagged, not just printed: {weak}"
    );
    assert!(
        weak.contains("[sync] project_key"),
        "the warning must name the key that fixes it: {weak}"
    );

    // 2. A git origin is stable enough to agree across machines on its own, and
    //    normalizes so an HTTPS clone and an SSH clone resolve identically.
    let git = project.path().join(".git");
    fs::create_dir_all(&git)?;
    fs::write(
        git.join("config"),
        "[core]\n\tbare = false\n[remote \"origin\"]\n\turl = git@github.com:C0deGeek-dev/LocalMind.git\n",
    )?;
    let derived = sync_status_text(project.path(), folder.path())?;
    assert!(
        derived.contains("github.com/c0degeek-dev/localmind (git_remote)"),
        "the normalized remote key must be reported: {derived}"
    );
    assert!(
        !derived.contains("weak"),
        "a git-remote key is stable and must not be flagged: {derived}"
    );

    // 3. An explicit key outranks the remote, and the JSON surface carries the
    //    same three facts as the text one.
    write_config(
        project.path(),
        "\n[sync]\nproject_key = \"github.com/org/repo\"\n",
    )?;
    let json: serde_json::Value =
        serde_json::from_slice(&sync_status(project.path(), folder.path(), true)?)?;
    assert_eq!(json["project_key"], "github.com/org/repo");
    assert_eq!(json["project_key_source"], "explicit");
    assert_eq!(json["project_key_stable"], true);
    Ok(())
}
