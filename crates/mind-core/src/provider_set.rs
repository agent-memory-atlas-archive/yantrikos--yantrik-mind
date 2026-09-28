//! The person sets their Mind's model -- key included -- from the desktop's Settings (E.PROV1).
//!
//! Pranab's decision: every person picks their own Mind's provider, and entering it is their
//! consent. A key must never pass through a chat transcript (the desktop shows the chat and reports
//! it to other agents), and a root helper would add a root code path that parses a secret while
//! gating nothing (every process of the person could run it). So the Mind takes the setting itself:
//! `POST /provider` on the person's memory socket (E.SOCK1 -- never on TCP), accepted only from the
//! person's own uid, written into the Mind's own 0600 settings file. The desktop's page
//! (Settings -> AI & Intelligence -> Yantrik Mind -> Model) sends it straight here and keeps nothing.

use std::path::{Path, PathBuf};

use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use mind_memory_mcp::PeerUid;
use serde::Deserialize;

/// Where the desktop sends a person to set this Mind's model.
pub const SETTINGS_PLACE: &str = "Settings \u{2192} AI & Intelligence \u{2192} Yantrik Mind \u{2192} Model";

/// What the desktop sends.
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderSet {
    pub provider: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    /// May the person's private context go to this (cloud) provider? Explicit, per provider.
    #[serde(default)]
    pub private_context: bool,
}

/// What was saved -- never the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub provider: String,
    pub model: Option<String>,
}

/// The cloud providers the default chain runs, with the variable naming each one's model.
const CLOUD: &[(&str, &str)] = &[("nanogpt", "YM_MODEL"), ("ollama-cloud", "YM_OLLAMA_MODEL"), ("minimax", "YM_MINIMAX_MODEL")];

/// One line, no control characters: a value cannot add lines to the settings file. The error never
/// quotes the value.
fn one_line(v: &str, what: &str) -> Result<String, String> {
    let v = v.trim();
    if v.is_empty() {
        return Err(format!("{what} is empty"));
    }
    if v.chars().any(|c| c.is_control()) || v.len() > 1024 {
        return Err(format!("{what} must be a single line of text"));
    }
    Ok(v.to_string())
}

fn model_name(v: &str) -> Result<String, String> {
    let v = one_line(v, "the model")?;
    if v.len() > 200 || !v.chars().all(|c| c.is_ascii_alphanumeric() || "._:/@-".contains(c)) {
        return Err("the model name has characters a model name does not have".into());
    }
    Ok(v)
}

/// Save the setting into the Mind's own settings file, keeping everything else in it.
pub fn apply(env_path: &Path, req: &ProviderSet) -> Result<Saved, String> {
    let existing = std::fs::read_to_string(env_path)
        .unwrap_or_else(|_| "# Yantrik Mind's own settings. Yantrik OS never reads this file.\n".to_string());
    let upsert = crate::setup::upsert_env_line;
    let provider = req.provider.trim().to_string();
    let (body, model) = if provider == "ollama" {
        let url = req
            .base_url
            .as_deref()
            .and_then(crate::first_run::parse_address)
            .ok_or("a local model needs the address of the machine running Ollama")?;
        let model = model_name(req.model.as_deref().ok_or("a local model needs the model's name")?)?;
        let mut body = upsert(&existing, "YM_LOCAL_OLLAMA_URL", &url);
        body = upsert(&body, "YM_LOCAL_OLLAMA_MODEL", &model);
        if let Some(k) = req.key.as_deref().filter(|k| !k.trim().is_empty()) {
            body = upsert(&body, "YM_LOCAL_OLLAMA_KEY", &one_line(k, "the key")?);
        }
        (body, Some(model))
    } else if let Some((_, model_var)) = CLOUD.iter().find(|(p, _)| *p == provider) {
        if req.base_url.as_deref().is_some_and(|u| !u.trim().is_empty()) {
            return Err(format!("{provider} is reached at its own address; a base_url is not taken"));
        }
        let key = one_line(req.key.as_deref().unwrap_or_default(), "the key")?;
        if key.len() < 8 {
            return Err("that key is too short to be a key".into());
        }
        let (_, key_var, _) = mind_inference::provider_catalog(&provider).ok_or("unknown provider")?;
        let mut body = upsert(&existing, key_var, &key);
        let model = match req.model.as_deref().filter(|m| !m.trim().is_empty()) {
            Some(m) => {
                let m = model_name(m)?;
                body = upsert(&body, model_var, &m);
                Some(m)
            }
            None => None,
        };
        body = upsert(&body, "YM_PRIVATE_PROVIDERS", &private_list(&existing, &provider, req.private_context));
        (body, model)
    } else {
        return Err("unknown provider -- this Mind takes ollama, nanogpt, ollama-cloud or minimax".into());
    };
    crate::setup::write_env_600(&env_path.display().to_string(), &body).map_err(|e| format!("could not save: {e}"))?;
    Ok(Saved { provider, model })
}

/// `YM_PRIVATE_PROVIDERS` with `provider` added (consent) or removed (no consent).
fn private_list(existing: &str, provider: &str, consent: bool) -> String {
    let current = existing
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("YM_PRIVATE_PROVIDERS="))
        .unwrap_or_default();
    let mut items: Vec<String> = current
        .split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty() && x != provider)
        .collect();
    if consent {
        items.push(provider.to_string());
    }
    items.join(",")
}

#[derive(Clone)]
struct Setter {
    person_uid: u32,
    env_path: PathBuf,
    supervised: bool,
}

/// The route, for the person's socket only.
pub fn router(person_uid: u32, env_path: PathBuf, supervised: bool) -> Router {
    Router::new()
        .route("/provider", post(set_provider))
        .with_state(Setter { person_uid, env_path, supervised })
}

async fn set_provider(
    State(s): State<Setter>,
    ext: axum::http::Extensions,
    Json(req): Json<ProviderSet>,
) -> (StatusCode, Json<serde_json::Value>) {
    // The connection's peer uid, set for the person's socket only (E.SOCK1); absent anywhere else.
    let peer = ext.get::<ConnectInfo<PeerUid>>().and_then(|ConnectInfo(p)| p.0);
    if peer != Some(s.person_uid) {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "saved": false, "error": "only the person may set this Mind's model" })),
        );
    }
    match apply(&s.env_path, &req) {
        Ok(saved) => {
            eprintln!("[provider] the person set this Mind's model: {} {}", saved.provider, saved.model.as_deref().unwrap_or("(its default)"));
            if s.supervised {
                // Reply first, then restart with the new model (the unit starts it again).
                tokio::spawn(async {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    std::process::exit(0);
                });
            }
            (
                StatusCode::OK,
                Json(serde_json::json!({ "saved": true, "provider": saved.provider, "model": saved.model, "restarting": s.supervised })),
            )
        }
        Err(e) => (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "saved": false, "error": e }))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str, body: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ym-prov-{}-{name}.env", std::process::id()));
        std::fs::write(&p, body).unwrap();
        p
    }
    fn set(provider: &str, base_url: Option<&str>, model: Option<&str>, key: Option<&str>, private: bool) -> ProviderSet {
        ProviderSet {
            provider: provider.into(),
            base_url: base_url.map(str::to_string),
            model: model.map(str::to_string),
            key: key.map(str::to_string),
            private_context: private,
        }
    }

    #[test]
    fn the_setting_lands_in_the_right_lines_and_keeps_the_rest() {
        let p = tmp("ok", "YM_DB=/x/mind.db\nYM_PRIVATE_PROVIDERS=ollama-local\n");
        apply(&p, &set("nanogpt", None, Some("deepseek/deepseek-v4-pro"), Some("sk-abcdef123456"), true)).unwrap();
        let env = std::fs::read_to_string(&p).unwrap();
        assert!(env.contains("YM_DB=/x/mind.db\n"), "{env}");
        assert!(env.contains("NANOGPT_KEY=sk-abcdef123456\n"), "{env}");
        assert!(env.contains("YM_MODEL=deepseek/deepseek-v4-pro\n"), "{env}");
        assert!(env.contains("YM_PRIVATE_PROVIDERS=ollama-local,nanogpt\n"), "consent adds the provider: {env}");
        apply(&p, &set("nanogpt", None, None, Some("sk-abcdef123456"), false)).unwrap();
        let env = std::fs::read_to_string(&p).unwrap();
        assert!(env.contains("YM_PRIVATE_PROVIDERS=ollama-local\n"), "no consent removes it: {env}");
        apply(&p, &set("ollama", Some("192.168.4.35"), Some("qwen3.5:9b"), None, false)).unwrap();
        let env = std::fs::read_to_string(&p).unwrap();
        assert!(env.contains("YM_LOCAL_OLLAMA_URL=http://192.168.4.35:11434\n") && env.contains("YM_LOCAL_OLLAMA_MODEL=qwen3.5:9b\n"), "{env}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn nothing_can_add_a_line_and_a_refusal_changes_nothing() {
        let before = "YM_DB=/x/mind.db\n";
        let p = tmp("inject", before);
        let refused = [
            set("nanogpt", None, None, Some("sk-abcdef123456\nYM_X=1"), false),
            set("nanogpt", None, Some("m\nYM_X=1"), Some("sk-abcdef123456"), false),
            set("nanogpt", None, None, None, false),
            set("nanogpt", Some("https://evil.example"), None, Some("sk-abcdef123456"), false),
            set("openai", None, None, Some("sk-abcdef123456"), false),
            set("ollama", None, Some("m"), None, false),
        ];
        for r in &refused {
            let e = apply(&p, r).unwrap_err();
            assert!(!e.contains("sk-abcdef"), "an error quoted the key: {e}");
        }
        assert_eq!(std::fs::read_to_string(&p).unwrap(), before, "a refused setting changed the file");
        let _ = std::fs::remove_file(&p);
    }

    /// On unix a new settings file is the owner's alone from its first byte (it holds keys).
    #[cfg(unix)]
    #[test]
    fn a_new_settings_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let p = std::env::temp_dir().join(format!("ym-prov-{}-mode.env", std::process::id()));
        let _ = std::fs::remove_file(&p);
        apply(&p, &set("minimax", None, None, Some("mm-secret-key-1234"), false)).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_file(&p);
    }

    /// Only the person's uid is served, and no reply carries the key.
    #[tokio::test]
    async fn only_the_person_sets_it_and_the_key_is_never_echoed() {
        use tower::ServiceExt;
        let p = tmp("route", "");
        let call = |uid: Option<u32>| {
            let app = router(1000, p.clone(), false);
            async move {
                let mut req = axum::http::Request::builder()
                    .method("POST")
                    .uri("/provider")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"provider":"minimax","key":"mm-secret-key-1234","private_context":false}"#))
                    .unwrap();
                req.extensions_mut().insert(ConnectInfo(PeerUid(uid)));
                let res = app.oneshot(req).await.unwrap();
                let status = res.status();
                let body = axum::body::to_bytes(res.into_body(), 1 << 16).await.unwrap();
                (status, String::from_utf8_lossy(&body).into_owned())
            }
        };
        let (status, body) = call(Some(1001)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "", "another account's request changed the file");
        let (status, body) = call(Some(1000)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(!body.contains("mm-secret"), "the reply carried the key: {body}");
        assert!(std::fs::read_to_string(&p).unwrap().contains("MINIMAX_API_KEY=mm-secret-key-1234\n"));
        let _ = std::fs::remove_file(&p);
    }
}
