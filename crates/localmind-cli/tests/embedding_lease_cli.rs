#![allow(clippy::unwrap_used)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

fn write_project(project: &Path, endpoint: &str) {
    std::fs::create_dir_all(project.join("docs")).unwrap();
    std::fs::write(
        project.join("docs").join("guide.md"),
        "# Guide\n\nUse the exact owned embedding lifetime.\n",
    )
    .unwrap();
    std::fs::write(
        project.join(".localmind.toml"),
        format!(
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n\
             \n[inference]\nembedding_base_url = \"{endpoint}\"\n\
             embedding_model = \"test-embed\"\ntimeout_secs = 1\n"
        ),
    )
    .unwrap();
}

fn command(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_localmind"));
    command
        .env_remove("USERPROFILE")
        .env("HOME", home)
        .env("LOCALMIND_GLOBAL_ROOT", "@project");
    command
}

#[test]
fn unreachable_best_effort_ingest_then_required_backfill_is_actionable() {
    let work = tempfile::tempdir().unwrap();
    let project = work.path().join("project");
    let home = work.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let probe = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", probe.local_addr().unwrap());
    drop(probe);
    write_project(&project, &endpoint);

    let ingest = command(&home)
        .args([
            "ingest",
            "docs",
            project.join("docs").to_str().unwrap(),
            "--project",
            project.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        ingest.status.success(),
        "{}",
        String::from_utf8_lossy(&ingest.stderr)
    );
    assert!(String::from_utf8_lossy(&ingest.stdout).contains("(0 embedded)"));
    let ingest_stderr = String::from_utf8_lossy(&ingest.stderr);
    assert!(ingest_stderr.contains("`localbox embed-serve`"));
    assert!(ingest_stderr.contains("lexical/no-vector fallback"));

    let backfill = command(&home)
        .args(["backfill", "--project", project.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!backfill.status.success());
    let backfill_stderr = String::from_utf8_lossy(&backfill.stderr);
    assert!(backfill_stderr.contains("`localbox embed-serve`"));
    assert!(backfill_stderr.contains("embedding_base_url + embedding_model"));

    let lease_root = home.join(".local-llm").join("embed-leases");
    assert!(!lease_root.join("started-by-localpilot").exists());
    assert_eq!(lease_count(&lease_root), 0);
}

#[test]
fn owned_server_is_leased_for_the_full_cli_lifetime_and_drop_releases_it() {
    let work = tempfile::tempdir().unwrap();
    let project = work.path().join("project");
    let home = work.path().join("home");
    let lease_root = home.join(".local-llm").join("embed-leases");
    std::fs::create_dir_all(&lease_root).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let endpoint = format!("http://{address}");
    write_project(&project, &endpoint);
    std::fs::write(
        lease_root.join("started-by-localpilot"),
        format!(
            "{{\n  \"schema\": 1,\n  \"owner\": \"localpilot\",\n  \
             \"endpoint\": \"{address}\",\n  \"server_pid\": {},\n  \
             \"phase\": \"active\"\n}}\n",
            std::process::id()
        ),
    )
    .unwrap();

    let (request_seen_tx, request_seen_rx) = mpsc::channel();
    let (respond_tx, respond_rx) = mpsc::channel();
    let server = std::thread::spawn(move || loop {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = stream.read(&mut buffer).unwrap_or(0);
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        if request.is_empty() {
            continue;
        }
        request_seen_tx.send(()).unwrap();
        respond_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let body = "{\"data\":[{\"embedding\":[1.0,0.0]}]}";
        write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        break;
    });

    let child = command(&home)
        .args([
            "ingest",
            "docs",
            project.join("docs").to_str().unwrap(),
            "--project",
            project.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    request_seen_rx
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    assert_eq!(lease_count(&lease_root), 1, "lease must exist mid-request");
    respond_tx.send(()).unwrap();
    let output = child.wait_with_output().unwrap();
    server.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Ingested"),
        "the command must complete after the held request is released; stdout={:?}; stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        lease_count(&lease_root),
        0,
        "RAII drop must release the lease"
    );
    assert!(
        lease_root.join("started-by-localpilot").exists(),
        "LocalMind never clears ownership or stops the server"
    );
}

fn lease_count(root: &Path) -> usize {
    std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "lease"))
        .count()
}
