//! Request files to a running mount: the mount process holds the workspace
//! lock, so drive history runs inside it and the namespace stays consistent
//! with what the mounted drive shows.
//!
//! `<workspace>/drive-history/requests/<id>.json` (written atomically) is
//! picked up by the mount's maintenance loop within seconds, renamed to
//! `<id>.working` while it runs, and answered in `responses/<id>.json`.
//! A mount of an older RPool never answers: the client times out and removes
//! its unclaimed request, so nothing runs later by surprise.
use super::ops::Op;
use crate::prelude::*;
use std::time::{Duration, Instant};

pub(crate) const REQUEST_VERSION: u32 = 1;

/// A request's JSON result and notes for stderr.
pub(crate) type Answer = (Value, Vec<String>);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Request {
    pub version: u32,
    pub id: String,
    pub pool: String,
    pub op: Op,
    pub created_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Response {
    pub id: String,
    pub ok: bool,
    #[serde(default)]
    pub value: Value,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub notes: Vec<String>,
}

pub(crate) fn root(workspace: &Path) -> PathBuf {
    workspace.join("drive-history")
}
fn requests(workspace: &Path) -> PathBuf {
    root(workspace).join("requests")
}
fn responses(workspace: &Path) -> PathBuf {
    root(workspace).join("responses")
}
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Writes a request; returns its id.
pub(crate) fn send(workspace: &Path, pool: &str, op: &Op) -> Result<String> {
    let id = crate::monitor::registry::new_id();
    let request = Request {
        version: REQUEST_VERSION,
        id: id.clone(),
        pool: pool.into(),
        op: op.clone(),
        created_unix: crate::utils::now_unix(),
    };
    let dir = requests(workspace);
    fs::create_dir_all(&dir)?;
    fs::create_dir_all(responses(workspace))?;
    // Temp name without `.json`: the mount only claims complete requests.
    let temp = dir.join(format!("{id}.tmp"));
    fs::write(&temp, serde_json::to_vec(&request)?)?;
    fs::rename(&temp, dir.join(format!("{id}.json")))?;
    Ok(id)
}

/// Waits for the answer of `id` (polling every `poll`).
pub(crate) fn wait(
    workspace: &Path,
    id: &str,
    timeout: Duration,
    poll: Duration,
) -> Result<Response> {
    let answer = responses(workspace).join(format!("{id}.json"));
    let pending = requests(workspace).join(format!("{id}.json"));
    let started = Instant::now();
    loop {
        if let Ok(bytes) = fs::read(&answer) {
            if let Ok(response) = serde_json::from_slice::<Response>(&bytes) {
                let _ = fs::remove_file(&answer);
                return Ok(response);
            }
        }
        if started.elapsed() >= timeout {
            // Unclaimed: withdraw it so it never runs unexpectedly later.
            if fs::remove_file(&pending).is_ok() {
                bail!("the mount did not answer (an RPool version without drive history, or busy); request withdrawn, nothing changed");
            }
            bail!(
                "the mount is still working on the request; its result will be in {}",
                answer.display()
            );
        }
        std::thread::sleep(poll);
    }
}

/// Sends `op` to the mount of `workspace` and returns its answer.
pub(crate) fn submit(workspace: &Path, pool: &str, op: &Op) -> Result<Answer> {
    let timeout = if op.writes_drive() {
        Duration::from_secs(6 * 3600)
    } else {
        Duration::from_secs(300)
    };
    let id = send(workspace, pool, op)?;
    let response = wait(workspace, &id, timeout, Duration::from_millis(250))?;
    if !response.ok {
        bail!(
            "{}",
            response
                .error
                .unwrap_or_else(|| "drive history request failed".into())
        );
    }
    Ok((response.value, response.notes))
}

/// Mount side: claims and answers every waiting request with `handle`.
/// Returns how many were answered.
pub(crate) fn serve(
    workspace: &Path,
    handle: &dyn Fn(&Request) -> Result<Answer>,
) -> Result<usize> {
    let dir = requests(workspace);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Ok(0);
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()
                .and_then(|n| n.strip_suffix(".json"))
                .filter(|id| valid_id(id))
                .map(str::to_owned)
        })
        .collect();
    ids.sort();
    let mut answered = 0;
    for id in ids {
        let claimed = dir.join(format!("{id}.working"));
        if fs::rename(dir.join(format!("{id}.json")), &claimed).is_err() {
            continue; // withdrawn by the client meanwhile
        }
        let response = match fs::read(&claimed)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| Ok(serde_json::from_slice::<Request>(&bytes)?))
        {
            Ok(request) if request.version != REQUEST_VERSION || request.id != id => {
                failure(&id, "unsupported drive history request".into())
            }
            Ok(request) => match handle(&request) {
                Ok((value, notes)) => Response {
                    id: id.clone(),
                    ok: true,
                    value,
                    error: None,
                    notes,
                },
                Err(error) => failure(&id, format!("{error:#}")),
            },
            Err(error) => failure(&id, format!("unreadable request: {error:#}")),
        };
        let out = responses(workspace);
        fs::create_dir_all(&out)?;
        let temp = out.join(format!("{id}.tmp"));
        fs::write(&temp, serde_json::to_vec(&response)?)?;
        fs::rename(&temp, out.join(format!("{id}.json")))?;
        let _ = fs::remove_file(&claimed);
        answered += 1;
    }
    Ok(answered)
}

fn failure(id: &str, error: String) -> Response {
    Response {
        id: id.into(),
        ok: false,
        value: Value::Null,
        error: Some(error),
        notes: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_answers_requests_and_unanswered_requests_are_withdrawn() {
        let ws = tempfile::tempdir().unwrap();
        let id = send(ws.path(), "p", &Op::TrashList).unwrap();
        let served = serve(ws.path(), &|request| {
            assert_eq!((request.pool.as_str(), &request.op), ("p", &Op::TrashList));
            Ok((serde_json::json!([1]), vec!["note".into()]))
        })
        .unwrap();
        assert_eq!(served, 1);
        let response = wait(
            ws.path(),
            &id,
            Duration::from_secs(1),
            Duration::from_millis(1),
        )
        .unwrap();
        assert!(response.ok && response.value == serde_json::json!([1]));
        // Errors travel back as text.
        let id = send(ws.path(), "p", &Op::TrashList).unwrap();
        serve(ws.path(), &|_| bail!("boom")).unwrap();
        let response = wait(
            ws.path(),
            &id,
            Duration::from_secs(1),
            Duration::from_millis(1),
        )
        .unwrap();
        assert_eq!(response.error.as_deref(), Some("boom"));
        // Nobody serves: the request is withdrawn and never runs later.
        let id = send(ws.path(), "p", &Op::TrashList).unwrap();
        let error = wait(
            ws.path(),
            &id,
            Duration::from_millis(20),
            Duration::from_millis(5),
        )
        .unwrap_err();
        assert!(format!("{error}").contains("withdrawn"));
        assert_eq!(serve(ws.path(), &|_| unreachable!()).unwrap(), 0);
    }
}
