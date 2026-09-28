//! The machine's memory over MCP — as a library, so the owner of the memory file can serve it.
//!
//! # Who serves this
//!
//! Exactly one process may have the memory file open with a live engine: a second one does not
//! see the first one's writes (proven 2026-09-16 — its recall misses them while its stats count
//! them). So this endpoint is served by whichever process owns the file:
//!
//! - **Yantrik Mind, when it is the active mind.** It already owns the file; it serves this from
//!   its own `MemoryHandle`, so an agent calling `recall` here and the mind recalling in a turn are
//!   reading one engine.
//! - **The standalone `yantrik-memory` binary, otherwise.** When Hermes (or no mind) is active,
//!   Mind is stopped and this binary owns the file instead.
//!
//! Same address, same token file, same tools either way; a client cannot tell which is running and
//! does not need to. The service manager, not good intentions, keeps the two from overlapping —
//! the standalone unit declares `Conflicts=` with Mind's.

pub mod door;
mod server;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context};
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};

pub use server::{Caller, MemoryServer};

/// The address clients look for when nothing says otherwise.
pub const DEFAULT_BIND: &str = "127.0.0.1:7440";

/// Where the token lives: beside the memory file, so both possible owners of one file agree on it
/// without being told.
pub fn default_token_path(db_path: &str) -> PathBuf {
    Path::new(db_path).with_file_name("yantrik-memory.token")
}

/// Parse and check a bind address. Loopback only: this endpoint hands a person's memory to anyone
/// holding the token, over plain HTTP.
pub fn parse_bind(bind: &str) -> anyhow::Result<SocketAddr> {
    let addr: SocketAddr = bind.parse().with_context(|| format!("bad bind address `{bind}`"))?;
    if !addr.ip().is_loopback() {
        return Err(anyhow!("the memory server must bind a loopback address, got {addr}"));
    }
    Ok(addr)
}

/// Read the token, or create one readable only by this user.
pub fn load_or_create_token(path: &Path) -> anyhow::Result<String> {
    if let Ok(existing) = std::fs::read_to_string(path) {
        let t = existing.trim().to_string();
        if t.len() >= 32 {
            return Ok(t);
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut bytes = [0u8; 32];
    use std::io::Read;
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .context("reading /dev/urandom for the token")?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(path, &token)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(token)
}

/// Resolves when the process is asked to stop: Ctrl-C, or SIGTERM from the service manager.
///
/// SIGTERM matters more than it looks. Switching minds is the service manager stopping one owner
/// of the memory file and starting the other, and it stops things with SIGTERM. Waiting only on
/// Ctrl-C would leave the default SIGTERM action — immediate death, mid-write — as the way every
/// handover ends.
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Compare without leaking how many leading bytes matched.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Who may present what (E.DOOR2): the machine's one token, and -- when this Mind knows whose it is
/// and where the door is -- another mind's `mem-` credential, which the desktop must vouch for.
#[derive(Clone)]
pub struct Auth {
    token: String,
    validator: Option<std::sync::Arc<door::Validator>>,
}

impl Auth {
    /// The machine token only.
    pub fn token_only(token: String) -> Self {
        Self { token, validator: None }
    }
    /// The machine token, and credentials the desktop validates.
    pub fn with_validator(token: String, validator: door::Validator) -> Self {
        Self { token, validator: Some(std::sync::Arc::new(validator)) }
    }
    /// From this process's environment: credentials are accepted only when the unit names the
    /// person (`YANTRIK_PERSON_UID`) and the door (`YANTRIK_MIND_RUN`). Otherwise token only.
    pub fn from_env(token: String) -> Self {
        #[cfg(unix)]
        {
            let uid = std::env::var("YANTRIK_PERSON_UID").ok().and_then(|v| v.trim().parse::<u32>().ok());
            let run = std::env::var("YANTRIK_MIND_RUN").ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
            if let (Some(uid), Some(run)) = (uid, run) {
                let socket = PathBuf::from(run).join("app-shell.sock");
                tracing::info!(person_uid = uid, door = %socket.display(), "other minds' memory credentials are validated by the desktop");
                return Self::with_validator(token, door::Validator::over_door(socket, uid));
            }
        }
        Self::token_only(token)
    }

    /// E.SOCK1: the caller a request stands for. `peer` is the connection's peer uid when it came
    /// over the person's unix socket (`Some(None)`: the kernel would not say), `None` over TCP. On
    /// the socket only the person, presenting a credential the desktop validates, is served -- the
    /// machine token is not accepted there.
    fn caller_for(&self, presented: &str, peer: Option<Option<u32>>) -> Option<server::Caller> {
        match peer {
            None => self.caller(presented),
            Some(uid) => {
                let v = self.validator.as_ref()?;
                if uid != Some(v.person_uid()) || !presented.starts_with("mem-") {
                    return None;
                }
                v.footing(presented).map(server::Caller::Agent)
            }
        }
    }

    /// The caller a bearer stands for, or None.
    fn caller(&self, presented: &str) -> Option<server::Caller> {
        if constant_time_eq(presented.as_bytes(), self.token.as_bytes()) {
            return Some(server::Caller::MachineToken);
        }
        if presented.starts_with("mem-") {
            let footing = self.validator.as_ref()?.footing(presented)?;
            return Some(server::Caller::Agent(footing));
        }
        None
    }
}

/// E.SOCK1: the peer uid of a connection to the person's socket, as the kernel reports it.
#[derive(Clone, Copy, Debug)]
pub struct PeerUid(pub Option<u32>);

#[cfg(unix)]
impl axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, tokio::net::UnixListener>> for PeerUid {
    fn connect_info(stream: axum::serve::IncomingStream<'_, tokio::net::UnixListener>) -> Self {
        PeerUid(stream.io().peer_cred().ok().map(|c| c.uid()))
    }
}

async fn require_token(State(auth): State<Auth>, mut req: Request, next: Next) -> Response {
    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("")
        .to_string();
    let peer = req
        .extensions()
        .get::<axum::extract::ConnectInfo<PeerUid>>()
        .map(|axum::extract::ConnectInfo(p)| p.0);
    match auth.caller_for(&presented, peer) {
        Some(caller) => {
            // E.DOOR2: the caller rides with the request; rmcp hands its parts to the tool call.
            req.extensions_mut().insert(caller);
            next.run(req).await
        }
        None => (StatusCode::UNAUTHORIZED, "a valid bearer token is required").into_response(),
    }
}

/// Serve the memory over MCP until `shutdown` resolves. `served_by` names the owner in `/health`,
/// so a person checking which process has the memory can see it.
pub async fn serve_http(
    mem: mind_memory::MemoryHandle,
    bind: SocketAddr,
    token_path: &Path,
    served_by: &'static str,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let auth = Auth::from_env(load_or_create_token(token_path)?);
    let app = app(mem, auth, served_by);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding the memory server to {bind}"))?;
    tracing::info!(addr = %bind, token_file = %token_path.display(), served_by, "memory served over MCP at /mcp");
    axum::serve(listener, app).with_graceful_shutdown(shutdown).await?;
    Ok(())
}

/// E.SOCK1: serve the memory on the person's unix socket until `shutdown` resolves. Only the
/// person's processes, presenting a credential the desktop validates, are served; without a
/// person and a door in the environment there is no one it could serve, so it does not start.
#[cfg(unix)]
pub async fn serve_unix(
    mem: mind_memory::MemoryHandle,
    socket: &Path,
    token_path: &Path,
    served_by: &'static str,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    let auth = Auth::from_env(load_or_create_token(token_path)?);
    if auth.validator.is_none() {
        return Err(anyhow!(
            "not serving memory on {}: YANTRIK_PERSON_UID and YANTRIK_MIND_RUN must name the person and the door",
            socket.display()
        ));
    }
    match std::fs::symlink_metadata(socket) {
        Ok(m) if m.file_type().is_socket() => std::fs::remove_file(socket)?,
        Ok(_) => return Err(anyhow!("{} exists and is not a socket -- not touching it", socket.display())),
        Err(_) => {}
    }
    let listener = tokio::net::UnixListener::bind(socket)
        .with_context(|| format!("binding the memory socket {}", socket.display()))?;
    // The directory (0750, group = the person) and each connection's peer uid are the gates.
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o666))?;
    tracing::info!(socket = %socket.display(), served_by, "memory served over MCP on the person's socket at /mcp");
    let app = app(mem, auth, served_by).into_make_service_with_connect_info::<PeerUid>();
    axum::serve(listener, app).with_graceful_shutdown(shutdown).await?;
    Ok(())
}

/// The whole HTTP surface: `/mcp` behind the authentication middleware, and `/health`.
pub fn app(mem: mind_memory::MemoryHandle, auth: Auth, served_by: &'static str) -> Router {
    let mcp = StreamableHttpService::new(
        move || Ok(MemoryServer::over_http(mem.clone())),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default(),
    );
    Router::new()
        .nest_service("/mcp", mcp)
        .route_layer(middleware::from_fn_with_state(auth, require_token))
        // After the layer, so the health check needs no token: it says only that memory is being
        // served and by whom — what a service manager or a client's first probe needs.
        .route(
            "/health",
            get(move || async move {
                axum::Json(serde_json::json!({ "ok": true, "server": "yantrik-memory", "served_by": served_by }))
            }),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// E.DOOR2 end to end, through rmcp's real streamable-HTTP service: an MCP session whose calls
    /// carry a `mem-` credential reach the TOOL as that Agent (refused for a grant it lacks), and the
    /// machine token's calls reach it as the machine token. This is the proof the two halves meet --
    /// that rmcp carries the request's parts, with the caller in them, into the tool call.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_mcp_call_reaches_the_tool_as_the_vouched_caller() {
        use tower::ServiceExt;
        let validator = door::Validator::with_asker(
            Box::new(|sha: &str| {
                Ok((sha == door::credential_sha256("mem-good")).then(|| {
                    serde_json::json!({"v": 1, "person_uid": 1000, "mind": "hermes", "attach": "hermes:c1@s1", "grants": [], "valid_for_ms": 2000})
                }))
            }),
            1000,
        );
        let app = app(mind_memory::MemoryHandle::spawn(":memory:", 8).unwrap(), Auth::with_validator("tok".into(), validator), "test");
        async fn post(app: &Router, bearer: &str, session: Option<&str>, body: serde_json::Value) -> (StatusCode, Option<String>, String) {
            let mut req = axum::http::Request::builder()
                .method("POST")
                .uri("/mcp")
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json, text/event-stream");
            if let Some(s) = session {
                req = req.header("mcp-session-id", s);
            }
            let res = app.clone().oneshot(req.body(axum::body::Body::from(body.to_string())).unwrap()).await.unwrap();
            let status = res.status();
            let session = res.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()).map(str::to_string);
            let body = tokio::time::timeout(std::time::Duration::from_secs(10), axum::body::to_bytes(res.into_body(), 1 << 20))
                .await
                .expect("the MCP answer never finished")
                .unwrap();
            (status, session, String::from_utf8_lossy(&body).into_owned())
        }
        let (status, session, body) = post(&app, "tok", None, serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}
        })).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let session = session.expect("no mcp-session-id");
        let (status, _, _) = post(&app, "tok", Some(&session), serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).await;
        assert!(status.is_success(), "{status}");
        let call = serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "beliefs", "arguments": {"query": "anything"}}});
        let (_, _, as_agent) = post(&app, "mem-good", Some(&session), call.clone()).await;
        assert!(as_agent.contains("recall_ordinary"), "the agent's call did not reach the tool as the agent: {as_agent}");
        let (_, _, as_machine) = post(&app, "tok", Some(&session), call).await;
        assert!(
            as_machine.contains(r#""result":{"content""#) && !as_machine.contains("recall_ordinary"),
            "the machine token's call changed: {as_machine}"
        );
    }

    fn vouching(person: u32) -> door::Validator {
        door::Validator::with_asker(
            Box::new(move |sha: &str| {
                Ok((sha == door::credential_sha256("mem-good")).then(|| {
                    serde_json::json!({"v": 1, "person_uid": person, "mind": "hermes", "attach": "hermes:c1@s1", "grants": [], "valid_for_ms": 2000})
                }))
            }),
            person,
        )
    }

    /// E.SOCK1, the rule: on the person's socket only the person, with a credential the desktop
    /// vouches for, is served; the machine token is not accepted there; TCP is unchanged.
    #[test]
    fn on_the_socket_only_the_person_with_a_credential_is_served() {
        let auth = Auth::with_validator("tok".into(), vouching(1000));
        let who = |presented: &str, peer: Option<Option<u32>>| match auth.caller_for(presented, peer) {
            Some(server::Caller::Agent(f)) => format!("agent:{}", f.mind()),
            Some(server::Caller::MachineToken) => "machine".into(),
            None => "refused".into(),
        };
        assert_eq!(who("mem-good", Some(Some(1000))), "agent:hermes");
        assert_eq!(who("mem-good", Some(Some(1001))), "refused", "another account presented the person's credential");
        assert_eq!(who("mem-good", Some(None)), "refused", "a peer the kernel would not name");
        assert_eq!(who("tok", Some(Some(1000))), "refused", "the machine token on the socket");
        assert_eq!(who("mem-unknown", Some(Some(1000))), "refused");
        assert_eq!(who("tok", None), "machine", "TCP changed");
        assert_eq!(who("mem-good", None), "agent:hermes", "TCP changed");
        assert_eq!(Auth::token_only("tok".into()).caller_for("mem-good", Some(Some(1000))).map(|_| ()), None, "no validator, no one");
    }

    /// E.SOCK1 on a real socket (unix): a request from this process's own uid -- standing in for the
    /// person -- with a vouched credential is served; without a bearer it is 401.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_socket_serves_the_person_and_no_one_else() {
        use std::io::{Read, Write};
        let me = unsafe { libc_getuid() };
        let dir = std::env::temp_dir().join(format!("ym-sock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("memory.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let app = app(mind_memory::MemoryHandle::spawn(":memory:", 8).unwrap(), Auth::with_validator("tok".into(), vouching(me)), "test")
            .into_make_service_with_connect_info::<PeerUid>();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let ask = move |bearer: Option<&str>| {
            let mut s = std::os::unix::net::UnixStream::connect(&sock).unwrap();
            let auth = bearer.map(|b| format!("Authorization: Bearer {b}\r\n")).unwrap_or_default();
            let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#;
            write!(
                s,
                "POST /mcp HTTP/1.1\r\nHost: x\r\n{auth}Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            s.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
            let mut out = String::new();
            let _ = s.read_to_string(&mut out);
            out.lines().next().unwrap_or_default().to_string()
        };
        let (with, without, token) =
            tokio::task::spawn_blocking(move || (ask(Some("mem-good")), ask(None), ask(Some("tok")))).await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(with.contains(" 200 "), "the person with a credential was not served: {with}");
        assert!(without.contains(" 401 "), "no bearer was served: {without}");
        assert!(token.contains(" 401 "), "the machine token was served on the person's socket: {token}");
    }

    #[cfg(unix)]
    unsafe fn libc_getuid() -> u32 {
        extern "C" {
            fn getuid() -> u32;
        }
        getuid()
    }

    /// E.DOOR2 through the real middleware: the machine token is the machine token; a `mem-`
    /// credential is the Agent the desktop vouched for, and nothing else gets in.
    #[tokio::test]
    async fn each_request_is_served_as_the_caller_the_desktop_vouched_for() {
        use tower::ServiceExt;
        let validator = door::Validator::with_asker(
            Box::new(|sha: &str| {
                if sha == door::credential_sha256("mem-good") {
                    Ok(Some(serde_json::json!({"v": 1, "person_uid": 1000, "mind": "hermes", "attach": "hermes:c1@s1", "grants": [], "valid_for_ms": 2000})))
                } else {
                    Ok(None)
                }
            }),
            1000,
        );
        let who = |auth: Auth| {
            Router::new()
                .route(
                    "/mcp",
                    get(|req: Request| async move {
                        match req.extensions().get::<server::Caller>() {
                            Some(server::Caller::MachineToken) => "machine".to_string(),
                            Some(server::Caller::Agent(f)) => format!("agent:{}", f.mind()),
                            None => "nobody".to_string(),
                        }
                    }),
                )
                .route_layer(middleware::from_fn_with_state(auth, require_token))
        };
        async fn call(app: Router, bearer: Option<&str>) -> (StatusCode, String) {
            let mut req = axum::http::Request::builder().uri("/mcp");
            if let Some(b) = bearer {
                req = req.header(header::AUTHORIZATION, format!("Bearer {b}"));
            }
            let res = app.oneshot(req.body(axum::body::Body::empty()).unwrap()).await.unwrap();
            let status = res.status();
            let body = axum::body::to_bytes(res.into_body(), 1 << 16).await.unwrap();
            (status, String::from_utf8_lossy(&body).into_owned())
        }
        let with = Auth::with_validator("tok".into(), validator);
        assert_eq!(call(who(with.clone()), Some("tok")).await, (StatusCode::OK, "machine".into()));
        assert_eq!(call(who(with.clone()), Some("mem-good")).await, (StatusCode::OK, "agent:hermes".into()));
        assert_eq!(call(who(with.clone()), Some("mem-unknown")).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(call(who(with.clone()), None).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(call(who(Auth::token_only("tok".into())), Some("mem-good")).await.0, StatusCode::UNAUTHORIZED, "a credential was accepted with no validator");
    }

    #[test]
    fn tokens_compare_exactly() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"", b"a"));
    }

    #[test]
    fn only_loopback_binds_are_accepted() {
        assert!(parse_bind("127.0.0.1:7440").is_ok());
        assert!(parse_bind("[::1]:7440").is_ok());
        assert!(parse_bind("0.0.0.0:7440").is_err());
        assert!(parse_bind("192.168.4.65:7440").is_err());
    }

    /// Both possible owners of one file must find the same token without being configured.
    #[test]
    fn the_token_sits_beside_the_memory_file() {
        assert_eq!(
            default_token_path("/home/u/.local/share/yantrik-mind/mind.db"),
            PathBuf::from("/home/u/.local/share/yantrik-mind/yantrik-memory.token")
        );
    }
}
