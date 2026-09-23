//! The listen agent: OpenCode on this box, hub dials in. Nothing here phones home.

use std::path::PathBuf;
use std::process::Stdio;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::process::Command;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const DEFAULT_RELEASE: &str =
    "https://github.com/aedan/picrew-agent/releases/latest/download/picrew-agent-x86_64-unknown-linux-musl";

/// Inlined on first boot so a VM that cannot reach the hub still listens.
pub const LISTEN_PY: &str = include_str!("../listen.py");

#[derive(Clone)]
pub struct Agent {
    pub name: String,
    pub token: String,
    pub opencode: String,
    pub projects: PathBuf,
}

#[derive(Debug, Deserialize)]
pub struct Rpc {
    pub kind: String,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub body: Option<Value>,
    #[serde(default)]
    pub timeout: Option<f64>,
    #[serde(default)]
    pub payload: Option<Value>,
}

#[derive(Serialize)]
struct ErrBody {
    ok: bool,
    error: String,
}

pub fn router(agent: Agent) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/info", get(info))
        .route("/rpc", post(rpc))
        .with_state(agent)
}

fn authed(agent: &Agent, headers: &HeaderMap) -> bool {
    if agent.token.is_empty() {
        return false;
    }
    let mut got = headers
        .get("x-picrew-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim()
        .to_string();
    if let Some(auth) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        if let Some(rest) = auth.strip_prefix("Bearer ").or_else(|| auth.strip_prefix("bearer ")) {
            got = rest.trim().to_string();
        }
    }
    got == agent.token
}

fn deny() -> axum::response::Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(ErrBody {
            ok: false,
            error: "token".into(),
        }),
    )
        .into_response()
}

async fn healthz(State(agent): State<Agent>, headers: HeaderMap) -> impl IntoResponse {
    if !authed(&agent, &headers) {
        return deny();
    }
    (StatusCode::OK, Json(json!({ "ok": true, "name": agent.name }))).into_response()
}

async fn info(State(agent): State<Agent>, headers: HeaderMap) -> impl IntoResponse {
    if !authed(&agent, &headers) {
        return deny();
    }
    let repos = list_repos(&agent.projects);
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "name": agent.name,
            "repos": repos,
            "opencode": agent.opencode,
            "hostname": hostname(),
            "version": VERSION,
        })),
    )
        .into_response()
}

async fn rpc(
    State(agent): State<Agent>,
    headers: HeaderMap,
    Json(body): Json<Rpc>,
) -> impl IntoResponse {
    if !authed(&agent, &headers) {
        return deny();
    }
    match body.kind.as_str() {
        "opencode" => opencode(&agent, body).await.into_response(),
        "exec" => exec(&agent, body.payload.unwrap_or(json!({}))).await.into_response(),
        "config" => config(body.payload.unwrap_or(json!({}))).await.into_response(),
        "update" => update(body.payload.unwrap_or(json!({}))).await.into_response(),
        _ => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "unknown kind" })),
        )
            .into_response(),
    }
}

async fn opencode(agent: &Agent, body: Rpc) -> impl IntoResponse {
    let method = body.method.unwrap_or_else(|| "GET".into()).to_uppercase();
    let mut at = body.path.unwrap_or_else(|| "/".into());
    if !at.starts_with('/') {
        at.insert(0, '/');
    }
    let url = format!("{}{at}", agent.opencode.trim_end_matches('/'));
    let timeout = std::time::Duration::from_secs_f64(body.timeout.unwrap_or(120.0));
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .expect("reqwest");
    let mut req = match method.as_str() {
        "POST" => client.post(&url),
        "PUT" => client.put(&url),
        "DELETE" => client.delete(&url),
        "PATCH" => client.patch(&url),
        _ => client.get(&url),
    };
    if let Some(b) = body.body {
        req = req.json(&b);
    }
    match req.send().await {
        Ok(res) => {
            let status = res.status().as_u16();
            let text = res.text().await.unwrap_or_default();
            let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
            (
                StatusCode::OK,
                Json(json!({ "ok": status < 400, "status": status, "body": parsed })),
            )
                .into_response()
        }
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "ok": false, "error": err.to_string() })),
        )
            .into_response(),
    }
}

async fn exec(agent: &Agent, payload: Value) -> impl IntoResponse {
    let command = payload.get("command").and_then(|v| v.as_str()).unwrap_or("");
    if command.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "command required" })),
        )
            .into_response();
    }
    let args: Vec<String> = payload
        .get("args")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let cwd = payload
        .get("cwd")
        .and_then(|v| v.as_str())
        .unwrap_or("/");
    let timeout_ms = payload
        .get("timeoutMs")
        .and_then(|v| v.as_u64())
        .unwrap_or(60_000);
    let mut cmd = Command::new(command);
    cmd.args(&args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(env) = payload.get("env").and_then(|v| v.as_object()) {
        for (k, v) in env {
            if let Some(s) = v.as_str() {
                cmd.env(k, s);
            }
        }
    }
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "ok": false, "error": "could not start" })),
            )
                .into_response();
        }
    };
    match tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        child.wait_with_output(),
    )
    .await
    {
        Ok(Ok(out)) => {
            let stdout = tail(&String::from_utf8_lossy(&out.stdout), 20_000);
            let stderr = tail(&String::from_utf8_lossy(&out.stderr), 20_000);
            (
                StatusCode::OK,
                Json(json!({
                    "ok": true,
                    "body": {
                        "code": out.status.code().unwrap_or(-1),
                        "stdout": stdout,
                        "stderr": stderr,
                        "repos": list_repos(&agent.projects),
                    }
                })),
            )
                .into_response()
        }
        Ok(Err(err)) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "ok": false, "error": err.to_string() })),
        )
            .into_response(),
        Err(_) => (
            StatusCode::GATEWAY_TIMEOUT,
            Json(json!({ "ok": false, "error": "timeout" })),
        )
            .into_response(),
    }
}

async fn config(payload: Value) -> impl IntoResponse {
    if let Some(cfg) = payload.get("opencodeConfig") {
        let path = dirs_config().join("opencode/opencode.json");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string_pretty(cfg) {
            let _ = std::fs::write(&path, text);
        }
    }
    (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
}

pub async fn update(payload: Value) -> impl IntoResponse {
    match do_update(payload.get("url").and_then(|v| v.as_str())).await {
        Ok(msg) => (StatusCode::OK, Json(json!({ "ok": true, "note": msg, "version": VERSION }))).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "ok": false, "error": e })),
        )
            .into_response(),
    }
}

pub async fn do_update(url: Option<&str>) -> Result<String, String> {
    let url = url
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_RELEASE);
    let dest = std::env::current_exe().map_err(|e| e.to_string())?;
    let tmp = dest.with_extension("new");
    let bytes = reqwest::get(url)
        .await
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map_err(|e| e.to_string())?;
    if bytes.len() < 10_000 {
        return Err("download was too small to be the agent".into());
    }
    tokio::fs::write(&tmp, &bytes).await.map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(&tmp).map_err(|e| e.to_string())?.permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(&tmp, p).map_err(|e| e.to_string())?;
    }
    tokio::fs::rename(&tmp, &dest).await.map_err(|e| e.to_string())?;
    let _ = Command::new("systemctl")
        .args(["restart", "picrew-agent"])
        .status()
        .await;
    Ok(format!("replaced {} with latest from GitHub", dest.display()))
}

pub async fn auto_update_loop() {
    loop {
        tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
        match latest_tag().await {
            Ok(tag) if tag != VERSION && tag != format!("v{VERSION}") => {
                tracing::info!(tag, "newer picrew-agent on GitHub, updating");
                if let Err(e) = do_update(None).await {
                    tracing::warn!(error = %e, "self-update failed");
                }
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "could not check GitHub for agent updates"),
        }
    }
}

async fn latest_tag() -> Result<String, String> {
    let client = reqwest::Client::builder()
        .user_agent("picrew-agent")
        .build()
        .map_err(|e| e.to_string())?;
    let v: Value = client
        .get("https://api.github.com/repos/aedan/picrew-agent/releases/latest")
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    v.get("tag_name")
        .and_then(|t| t.as_str())
        .map(|s| s.trim_start_matches('v').to_string())
        .ok_or_else(|| "no tag_name".into())
}

fn dirs_config() -> PathBuf {
    if let Some(h) = std::env::var_os("HOME") {
        return PathBuf::from(h).join(".config");
    }
    PathBuf::from("/root/.config")
}

fn list_repos(root: &std::path::Path) -> Vec<String> {
    let _ = std::fs::create_dir_all(root);
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                let name = e.file_name();
                if !name.to_string_lossy().starts_with('.') {
                    out.push(e.path().display().to_string());
                }
            }
        }
    }
    out.sort();
    out
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string()))
        .unwrap_or_else(|_| "box".into())
}

fn tail(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        s[s.len() - n..].to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn healthz_needs_token() {
        let app = router(Agent {
            name: "box".into(),
            token: "secret".into(),
            opencode: "http://127.0.0.1:4096".into(),
            projects: PathBuf::from("/tmp"),
        });
        let res = app
            .clone()
            .oneshot(Request::builder().uri("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .header("X-PiCrew-Token", "secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["name"], "box");
    }
}
