use assert_cmd::Command;

#[test]
fn help_mentions_localmind() -> Result<(), Box<dyn std::error::Error>> {
    let mut command = Command::cargo_bin("localmind")?;

    let output = command.arg("--help").output()?;
    let stdout = std::str::from_utf8(&output.stdout)?;

    assert!(output.status.success());
    assert!(stdout.contains("Local-first learning engine"));
    Ok(())
}
