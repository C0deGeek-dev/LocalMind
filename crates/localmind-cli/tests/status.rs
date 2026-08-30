use assert_cmd::Command;
use std::fs;

/// `status` must count only items still awaiting review, and it must accept
/// `--project` like its sibling commands. Reproduces the live flow where a
/// project's only candidate was promoted end-to-end yet `status` kept
/// reporting it as pending.
#[test]
fn status_counts_only_pending_items_after_a_promotion() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let transcript_path = temp_dir.path().join("session.txt");
    fs::write(
        &transcript_path,
        "Fixed bug.\nLesson: Prefer deterministic CLI fixtures.\n",
    )?;
    fs::write(
        temp_dir.path().join(".localmind.toml"),
        "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
    )?;

    // Import + closeout to enqueue one review candidate.
    let import_output = Command::cargo_bin("localmind")?
        .arg("import")
        .arg(&transcript_path)
        .arg("--project")
        .arg(temp_dir.path())
        .output()?;
    assert!(import_output.status.success());
    let stdout = String::from_utf8(import_output.stdout)?;
    let session_id = stdout
        .lines()
        .find_map(|line| line.strip_prefix("Imported session "))
        .ok_or("no session id in import output")?
        .trim()
        .to_string();
    let closeout = Command::cargo_bin("localmind")?
        .arg("closeout")
        .arg(&session_id)
        .arg("--project")
        .arg(temp_dir.path())
        .output()?;
    assert!(closeout.status.success());

    let pending_line = |output: &[u8]| -> String {
        String::from_utf8_lossy(output)
            .lines()
            .find(|line| line.starts_with("review:"))
            .unwrap_or_default()
            .to_string()
    };

    // One candidate awaits review.
    let before = Command::cargo_bin("localmind")?
        .arg("status")
        .arg("--project")
        .arg(temp_dir.path())
        .output()?;
    assert!(before.status.success());
    assert!(
        pending_line(&before.stdout).contains("1 candidate(s) pending"),
        "expected 1 pending before review, got: {}",
        pending_line(&before.stdout)
    );

    // Accept + promote it end-to-end.
    let list = Command::cargo_bin("localmind")?
        .arg("review")
        .arg("list")
        .arg("--project")
        .arg(temp_dir.path())
        .output()?;
    let list_stdout = String::from_utf8(list.stdout)?;
    let item_id = list_stdout
        .lines()
        .next()
        .and_then(|line| line.split('\t').next())
        .ok_or("no review item listed")?
        .to_string();
    let accept = Command::cargo_bin("localmind")?
        .arg("review")
        .arg("accept")
        .arg(&item_id)
        .arg("--project")
        .arg(temp_dir.path())
        .output()?;
    assert!(accept.status.success());
    let promote = Command::cargo_bin("localmind")?
        .arg("promote")
        .arg(&item_id)
        .arg("--project")
        .arg(temp_dir.path())
        .output()?;
    assert!(promote.status.success());

    // The queue still holds the (accepted) item, but nothing is pending.
    let after = Command::cargo_bin("localmind")?
        .arg("status")
        .arg("--project")
        .arg(temp_dir.path())
        .output()?;
    assert!(after.status.success());
    assert!(
        pending_line(&after.stdout).contains("0 candidate(s) pending"),
        "a promoted item must not count as pending, got: {}",
        pending_line(&after.stdout)
    );
    Ok(())
}
