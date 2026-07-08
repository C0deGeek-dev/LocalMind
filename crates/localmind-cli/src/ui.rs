//! `localmind ui`: a local web app for reviewing and managing memory.
//!
//! A synchronous `tiny_http` server (no async runtime) bound to `127.0.0.1`,
//! exposing a small JSON API that calls the **same** store methods the CLI
//! does — `ReviewQueue::decide`, `MemoryPersistence::{promote_review_item,
//! record_review_item_audit, search}` — so there is no logic duplication and no
//! way to bypass the review gate. The frontend is one self-contained HTML file
//! embedded at build time.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use localmind_core::{MemoryEntryId, ReviewAction, ReviewDecision, ReviewItemId};
use localmind_store::{MemoryPersistence, ReviewQueue};
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

/// The self-contained review dashboard, embedded at build time.
const INDEX_HTML: &str = include_str!("ui/index.html");

pub fn serve(project: PathBuf, port: u16, open: bool, token: Option<String>) -> Result<()> {
    let addr = format!("127.0.0.1:{port}");
    let server = Server::http(&addr).map_err(|error| anyhow!("failed to bind {addr}: {error}"))?;
    let url = format!("http://{addr}");
    println!("LocalMind review UI: {url}");
    println!("Project: {}", project.display());
    if token.is_some() {
        println!("Token required (pass ?token=... in the browser URL).");
    }
    println!("Press Ctrl+C to stop.");
    if open {
        open_browser(&url);
    }

    for mut request in server.incoming_requests() {
        let response = route(&project, token.as_deref(), &mut request);
        let _ = request.respond(response);
    }
    Ok(())
}

fn route(
    project: &Path,
    token: Option<&str>,
    request: &mut Request,
) -> Response<Cursor<Vec<u8>>> {
    let method = request.method().clone();
    let raw_url = request.url().to_string();
    let (path, query) = raw_url.split_once('?').unwrap_or((raw_url.as_str(), ""));

    // Token gate (localhost bind is the primary control; token adds LAN safety).
    if let Some(expected) = token {
        if query_param(query, "token").as_deref() != Some(expected) {
            return json_response(401, &json!({ "error": "invalid or missing token" }));
        }
    }

    let mut body = String::new();
    let _ = request.as_reader().read_to_string(&mut body);

    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    let result = match (&method, segments.as_slice()) {
        (Method::Get, [""] | ["index.html"]) => return html_response(INDEX_HTML),
        (Method::Get, ["api", "status"]) => api_status(project),
        (Method::Get, ["api", "review"]) => api_review_list(project, query),
        (Method::Get, ["api", "review", id]) => api_review_get(project, id),
        (Method::Post, ["api", "review", "bulk"]) => api_bulk(project, &body),
        (Method::Post, ["api", "review", id, action]) => {
            api_review_action(project, id, action, &body)
        }
        (Method::Get, ["api", "memory"]) => api_memory_search(project, query),
        (Method::Delete, ["api", "memory", id]) => api_memory_delete(project, id),
        _ => Err(anyhow!("not found: {method:?} {path}")),
    };

    match result {
        Ok(value) => json_response(200, &value),
        Err(error) => json_response(400, &json!({ "error": error.to_string() })),
    }
}

fn api_status(project: &Path) -> Result<Value> {
    let queue = ReviewQueue::open_project(project)?;
    let pending = queue
        .list()?
        .into_iter()
        .filter(|item| format!("{:?}", item.state) == "Pending")
        .count();
    let persistence = MemoryPersistence::open_project(project)?;
    let accepted = persistence.list_memory()?.len();
    Ok(json!({
        "project": project.display().to_string(),
        "pending": pending,
        "accepted": accepted,
    }))
}

fn api_review_list(project: &Path, query: &str) -> Result<Value> {
    let want_state = query_param(query, "state");
    let queue = ReviewQueue::open_project(project)?;
    let items: Vec<Value> = queue
        .list()?
        .into_iter()
        .filter(|item| match &want_state {
            Some(state) => format!("{:?}", item.state).eq_ignore_ascii_case(state),
            None => true,
        })
        .map(|item| review_item_json(&item))
        .collect();
    Ok(json!({ "items": items }))
}

fn api_review_get(project: &Path, id: &str) -> Result<Value> {
    let queue = ReviewQueue::open_project(project)?;
    match queue.get(&ReviewItemId::new(id))? {
        Some(item) => Ok(review_item_json(&item)),
        None => Err(anyhow!("review item not found: {id}")),
    }
}

/// One review action: accept (accept + promote to memory), reject, defer, edit,
/// or promote. Accept and promote both write durable memory through the same
/// `promote_review_item` path the CLI uses.
fn api_review_action(project: &Path, id: &str, action: &str, body: &str) -> Result<Value> {
    let payload: Value = if body.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(body)?
    };
    let reviewer = payload
        .get("reviewer")
        .and_then(Value::as_str)
        .unwrap_or("ui")
        .to_string();
    let note = payload
        .get("note")
        .and_then(Value::as_str)
        .map(str::to_string);

    let persistence = MemoryPersistence::open_project(project)?;
    let item_id = ReviewItemId::new(id);

    match action {
        "accept" => {
            decide(project, &item_id, ReviewAction::Accept, reviewer, note, None)?;
            let entry = persistence.promote_review_item(&item_id)?;
            Ok(json!({ "id": id, "state": "Accepted", "promoted": entry.id.to_string() }))
        }
        "reject" => {
            let state = decide(project, &item_id, ReviewAction::Reject, reviewer, note, None)?;
            Ok(json!({ "id": id, "state": state }))
        }
        "defer" => {
            let state = decide(
                project,
                &item_id,
                ReviewAction::MarkTemporary,
                reviewer,
                note,
                None,
            )?;
            Ok(json!({ "id": id, "state": state }))
        }
        "edit" => {
            let replacement = payload
                .get("replacement")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("edit requires a `replacement` field"))?
                .to_string();
            let state = decide(
                project,
                &item_id,
                ReviewAction::Edit,
                reviewer,
                note,
                Some(replacement),
            )?;
            Ok(json!({ "id": id, "state": state }))
        }
        "promote" => {
            let entry = persistence.promote_review_item(&item_id)?;
            Ok(json!({ "id": id, "promoted": entry.id.to_string() }))
        }
        other => Err(anyhow!("unknown action: {other}")),
    }
}

fn api_bulk(project: &Path, body: &str) -> Result<Value> {
    let payload: Value = serde_json::from_str(body)?;
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("bulk requires an `action` field"))?;
    let ids = payload
        .get("ids")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("bulk requires an `ids` array"))?;
    let reviewer = payload
        .get("reviewer")
        .and_then(Value::as_str)
        .unwrap_or("ui");

    let mut done = 0usize;
    let mut errors: Vec<Value> = Vec::new();
    for id_value in ids {
        let Some(id) = id_value.as_str() else {
            continue;
        };
        let single = json!({ "reviewer": reviewer });
        match api_review_action(project, id, action, &single.to_string()) {
            Ok(_) => done += 1,
            Err(error) => errors.push(json!({ "id": id, "error": error.to_string() })),
        }
    }
    Ok(json!({ "action": action, "done": done, "errors": errors }))
}

fn api_memory_search(project: &Path, query: &str) -> Result<Value> {
    let q = query_param(query, "query").unwrap_or_default();
    let persistence = MemoryPersistence::open_project(project)?;
    let results: Vec<Value> = persistence
        .search(&q)?
        .into_iter()
        .map(|result| {
            json!({
                "memory_id": result.memory_id.to_string(),
                "score": result.score,
                "path": result.path.display().to_string(),
                "category": result.category,
                "snippet": result.snippet,
            })
        })
        .collect();
    Ok(json!({ "results": results }))
}

fn api_memory_delete(project: &Path, id: &str) -> Result<Value> {
    let persistence = MemoryPersistence::open_project(project)?;
    let deleted = persistence.delete_memory(&MemoryEntryId::new(id), "ui")?;
    if deleted {
        Ok(json!({ "id": id, "deleted": true }))
    } else {
        Err(anyhow!("accepted memory not found: {id}"))
    }
}

fn decide(
    project: &Path,
    item_id: &ReviewItemId,
    action: ReviewAction,
    reviewer: String,
    note: Option<String>,
    replacement_summary: Option<String>,
) -> Result<String> {
    let persistence = MemoryPersistence::open_project(project)?;
    let queue = ReviewQueue::open_project(project)?;
    let item = queue.decide(ReviewDecision {
        item_id: item_id.clone(),
        action,
        reviewer,
        decided_at: None,
        note,
        replacement_summary,
        evidence: Vec::new(),
    })?;
    persistence.record_review_item_audit(&item)?;
    Ok(format!("{:?}", item.state))
}

fn review_item_json(item: &localmind_store::ReviewQueueItem) -> Value {
    json!({
        "id": item.id.to_string(),
        "state": format!("{:?}", item.state),
        "session": item.session_id.to_string(),
        "summary": item.candidate.summary(),
        "category": format!("{:?}", item.candidate.category),
        "confidence": item.candidate.confidence.value(),
        "rationale": item.candidate.rationale.clone(),
        "replacement": item.replacement_summary.clone(),
        "note": item.note.clone(),
    })
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        if name == key {
            Some(percent_decode(value))
        } else {
            None
        }
    })
}

/// Minimal `application/x-www-form-urlencoded` decode (`+` → space, `%XX` → byte).
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    Err(_) => {
                        out.push(bytes[index]);
                        index += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn json_response(status: u16, value: &Value) -> Response<Cursor<Vec<u8>>> {
    let mut response = Response::from_string(value.to_string()).with_status_code(status);
    if let Ok(header) = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]) {
        response = response.with_header(header);
    }
    response
}

fn html_response(html: &str) -> Response<Cursor<Vec<u8>>> {
    let mut response = Response::from_string(html.to_string());
    if let Ok(header) = Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]) {
        response = response.with_header(header);
    }
    response
}

fn open_browser(url: &str) {
    // Best-effort convenience only; a failure to open is never fatal.
    #[cfg(windows)]
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

#[cfg(test)]
mod tests {
    use super::{percent_decode, query_param};

    #[test]
    fn query_param_extracts_and_decodes() {
        assert_eq!(
            query_param("state=Pending&token=a%20b", "state").as_deref(),
            Some("Pending")
        );
        assert_eq!(
            query_param("state=Pending&token=a%20b", "token").as_deref(),
            Some("a b")
        );
        assert_eq!(query_param("state=Pending", "missing"), None);
    }

    #[test]
    fn percent_decode_handles_plus_and_hex() {
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("%2Fdocs%2Fx"), "/docs/x");
        assert_eq!(percent_decode("plain"), "plain");
    }

    #[test]
    fn index_html_is_embedded_and_self_contained() {
        // No external asset references — the page must be self-contained.
        assert!(super::INDEX_HTML.contains("LocalMind Review"));
        assert!(!super::INDEX_HTML.contains("src=\"http"));
        assert!(!super::INDEX_HTML.contains("cdn"));
    }
}
