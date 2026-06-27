use assert_cmd::Command;
use std::fs;

#[test]
fn import_command_writes_redacted_transcript() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let transcript_path = temp_dir.path().join("session.txt");
    fs::write(
        &transcript_path,
        "fixed auth issue with token = sk-proj-abcdefghijklmnopqrstuvwxyz123456",
    )?;
    fs::write(
        temp_dir.path().join(".localmind.toml"),
        "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
    )?;

    let mut command = Command::cargo_bin("localmind")?;
    let output = command
        .arg("import")
        .arg(&transcript_path)
        .arg("--project")
        .arg(temp_dir.path())
        .arg("--source")
        .arg("open-ai-codex")
        .output()?;

    assert!(output.status.success());
    let sessions_dir = temp_dir.path().join(".localmind/sessions");
    let session_dir = fs::read_dir(sessions_dir)?
        .next()
        .ok_or("expected one imported session")??
        .path();
    let redacted = fs::read_to_string(session_dir.join("transcript.redacted.txt"))?;

    assert!(!redacted.contains("sk-proj-abcdefghijklmnopqrstuvwxyz123456"));
    assert!(redacted.contains("[REDACTED:openai_api_key]"));
    Ok(())
}

#[test]
fn closeout_command_writes_summary_and_candidates() -> Result<(), Box<dyn std::error::Error>> {
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
        .ok_or("missing imported session line")?;

    let closeout_output = Command::cargo_bin("localmind")?
        .arg("closeout")
        .arg(session_id)
        .arg("--project")
        .arg(temp_dir.path())
        .output()?;

    assert!(closeout_output.status.success());
    let sessions_dir = temp_dir.path().join(".localmind/sessions").join(session_id);
    assert!(sessions_dir.join("summary.json").exists());
    assert!(sessions_dir.join("candidates.json").exists());

    let list_output = Command::cargo_bin("localmind")?
        .arg("review")
        .arg("list")
        .arg("--project")
        .arg(temp_dir.path())
        .output()?;
    assert!(list_output.status.success());
    let list_stdout = String::from_utf8(list_output.stdout)?;
    let item_id = list_stdout
        .lines()
        .find_map(|line| line.split('\t').next())
        .ok_or("missing review item id")?;

    let inspect_output = Command::cargo_bin("localmind")?
        .arg("review")
        .arg("inspect")
        .arg(item_id)
        .arg("--project")
        .arg(temp_dir.path())
        .output()?;
    assert!(inspect_output.status.success());

    let accept_output = Command::cargo_bin("localmind")?
        .arg("review")
        .arg("accept")
        .arg(item_id)
        .arg("--project")
        .arg(temp_dir.path())
        .arg("--reviewer")
        .arg("test")
        .output()?;
    assert!(accept_output.status.success());

    let promote_output = Command::cargo_bin("localmind")?
        .arg("promote")
        .arg(item_id)
        .arg("--project")
        .arg(temp_dir.path())
        .output()?;
    assert!(promote_output.status.success());

    let search_output = Command::cargo_bin("localmind")?
        .arg("search")
        .arg("deterministic fixtures")
        .arg("--project")
        .arg(temp_dir.path())
        .output()?;
    assert!(search_output.status.success());
    assert!(String::from_utf8(search_output.stdout)?.contains(item_id));

    let audit_output = Command::cargo_bin("localmind")?
        .arg("audit")
        .arg("--project")
        .arg(temp_dir.path())
        .output()?;
    assert!(audit_output.status.success());
    assert!(String::from_utf8(audit_output.stdout)?.contains("MemoryPromoted"));
    Ok(())
}

#[test]
fn localpilot_fixture_exports_context_and_skill_draft() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let transcript_path = temp_dir.path().join("localpilot-session.txt");
    fs::write(
        &transcript_path,
        "LocalPilot session bundle.\ntoken = sk-proj-abcdefghijklmnopqrstuvwxyz123456\nLesson: Prefer localpilot context memory.\nWorkflow: Create skill for release checklist.\n",
    )?;
    fs::write(
        temp_dir.path().join(".localmind.toml"),
        "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
    )?;

    let import_output = Command::cargo_bin("localmind")?
        .arg("import")
        .arg(&transcript_path)
        .arg("--project")
        .arg(temp_dir.path())
        .arg("--source")
        .arg("localpilot")
        .output()?;
    assert!(import_output.status.success());
    let import_stdout = String::from_utf8(import_output.stdout)?;
    let session_id = import_stdout
        .lines()
        .find_map(|line| line.strip_prefix("Imported session "))
        .ok_or("missing imported session line")?;
    let redacted = fs::read_to_string(
        temp_dir
            .path()
            .join(".localmind/sessions")
            .join(session_id)
            .join("transcript.redacted.txt"),
    )?;
    assert!(!redacted.contains("sk-proj-abcdefghijklmnopqrstuvwxyz123456"));

    let closeout_output = Command::cargo_bin("localmind")?
        .arg("closeout")
        .arg(session_id)
        .arg("--project")
        .arg(temp_dir.path())
        .output()?;
    assert!(closeout_output.status.success());

    let list_output = Command::cargo_bin("localmind")?
        .arg("review")
        .arg("list")
        .arg("--project")
        .arg(temp_dir.path())
        .output()?;
    assert!(list_output.status.success());
    let list_stdout = String::from_utf8(list_output.stdout)?;
    let memory_item_id = review_item_id(&list_stdout, "Prefer localpilot context memory")?;
    let skill_item_id = review_item_id(&list_stdout, "Workflow: Create skill")?;

    assert!(Command::cargo_bin("localmind")?
        .arg("review")
        .arg("accept")
        .arg(memory_item_id)
        .arg("--project")
        .arg(temp_dir.path())
        .output()?
        .status
        .success());
    assert!(Command::cargo_bin("localmind")?
        .arg("promote")
        .arg(memory_item_id)
        .arg("--project")
        .arg(temp_dir.path())
        .output()?
        .status
        .success());
    assert!(Command::cargo_bin("localmind")?
        .arg("review")
        .arg("accept")
        .arg(skill_item_id)
        .arg("--project")
        .arg(temp_dir.path())
        .output()?
        .status
        .success());

    let generate_output = Command::cargo_bin("localmind")?
        .arg("skills")
        .arg("generate")
        .arg("--project")
        .arg(temp_dir.path())
        .output()?;
    assert!(generate_output.status.success());
    let generate_stdout = String::from_utf8(generate_output.stdout)?;
    let draft_id = generate_stdout
        .lines()
        .find_map(|line| line.split('\t').next())
        .ok_or("missing draft id")?;

    let inspect_output = Command::cargo_bin("localmind")?
        .arg("skills")
        .arg("inspect")
        .arg(draft_id)
        .arg("--project")
        .arg(temp_dir.path())
        .output()?;
    assert!(inspect_output.status.success());
    assert!(String::from_utf8(inspect_output.stdout)?.contains("Disabled: true"));

    let export_output = Command::cargo_bin("localmind")?
        .arg("skills")
        .arg("export")
        .arg(draft_id)
        .arg("--project")
        .arg(temp_dir.path())
        .output()?;
    assert!(export_output.status.success());
    let exported_skill = String::from_utf8(export_output.stdout)?;
    assert!(exported_skill.contains("disabled: true"));
    assert!(exported_skill.contains("Trigger Conditions"));

    let context_output = Command::cargo_bin("localmind")?
        .arg("context")
        .arg("export")
        .arg("localpilot context")
        .arg("--target")
        .arg("localpilot")
        .arg("--project")
        .arg(temp_dir.path())
        .output()?;
    assert!(context_output.status.success());
    let context = String::from_utf8(context_output.stdout)?;
    assert!(context.contains("LocalPilot built-in context"));
    assert!(context.contains(memory_item_id));
    assert!(context.contains(draft_id));

    let audit_output = Command::cargo_bin("localmind")?
        .arg("audit")
        .arg("--project")
        .arg(temp_dir.path())
        .output()?;
    assert!(audit_output.status.success());
    let audit = String::from_utf8(audit_output.stdout)?;
    assert!(audit.contains("ReviewDecisionRecorded"));
    assert!(audit.contains("MemoryPromoted"));
    assert!(audit.contains("SkillDraftCreated"));
    assert!(audit.contains("ContextPackExported"));
    Ok(())
}

fn review_item_id<'a>(
    list_stdout: &'a str,
    summary_fragment: &str,
) -> Result<&'a str, Box<dyn std::error::Error>> {
    list_stdout
        .lines()
        .find(|line| line.contains(summary_fragment))
        .and_then(|line| line.split('\t').next())
        .ok_or_else(|| format!("missing review item for {summary_fragment}").into())
}
