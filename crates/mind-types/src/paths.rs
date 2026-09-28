//! Where this Mind keeps its state (E.ROOT1, yantrik-os #447).
//!
//! One root, resolved from what the Mind was started with -- never a path it guesses. On Yantrik OS
//! each person has their own Mind with its own state directory; a literal default would be somebody
//! else's state, or nobody's. A Mind with no root keeps its state-backed features off.

use std::path::{Path, PathBuf};

/// The state root: `YM_STATE_DIR`, else systemd's `$STATE_DIRECTORY` (first entry), else the folder
/// the database lives in (`YM_DB`). None for a Mind with none of these -- an in-memory dev mind.
pub fn state_root() -> Option<PathBuf> {
    root_from(|k| std::env::var(k).ok())
}

/// A state file or folder: the specific override `env` when set, else `relative` under the root.
pub fn state_file(env: &str, relative: &str) -> Option<PathBuf> {
    file_from(|k| std::env::var(k).ok(), env, relative)
}

/// A state file or folder that must exist even for a Mind with no state root: `state_file`, else
/// the same name under a folder private to this process (0700, gone with the temp dir). Such a Mind
/// still works -- it just keeps nothing -- and it never shares a guessed folder with another Mind.
pub fn state_or_scratch(env: &str, relative: &str) -> PathBuf {
    state_file(env, relative).unwrap_or_else(|| scratch_root().join(relative))
}

/// The state root, else the process-private folder (see `state_or_scratch`).
pub fn state_root_or_scratch() -> PathBuf {
    state_root().unwrap_or_else(scratch_root)
}

/// Every folder sandboxed code must not see: the state root and the database's folder, which can
/// differ (`YM_STATE_DIR` set elsewhere than `YM_DB`).
pub fn hidden_dirs() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for d in [state_root(), db_dir()].into_iter().flatten() {
        if !out.contains(&d) {
            out.push(d);
        }
    }
    out
}

fn scratch_root() -> PathBuf {
    let d = std::env::temp_dir().join(format!("yantrik-mind-{}", std::process::id()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&d);
    }
    #[cfg(not(unix))]
    {
        let _ = std::fs::create_dir_all(&d);
    }
    d
}

/// The folder for this Mind's own trust files (device store, console token): the database's folder
/// first -- where they have always been -- else the state root, else a folder private to this process.
pub fn trust_dir() -> PathBuf {
    db_dir().or_else(state_root).unwrap_or_else(scratch_root)
}

/// The folder the database lives in, when it is a file.
pub fn db_dir() -> Option<PathBuf> {
    db_dir_from(|k| std::env::var(k).ok())
}

fn set(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn db_dir_from(get: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let db = set(get("YM_DB"))?;
    if db.starts_with(":memory:") {
        return None;
    }
    Path::new(&db).parent().filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf)
}

fn root_from(get: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(d) = set(get("YM_STATE_DIR")) {
        return Some(PathBuf::from(d));
    }
    // systemd joins several StateDirectory= entries with ':'; the first is this unit's own.
    if let Some(d) = set(get("STATE_DIRECTORY")).and_then(|v| set(v.split(':').next().map(str::to_string))) {
        return Some(PathBuf::from(d));
    }
    db_dir_from(get)
}

fn file_from(get: impl Fn(&str) -> Option<String>, env: &str, relative: &str) -> Option<PathBuf> {
    if let Some(p) = set(get(env)) {
        return Some(PathBuf::from(p));
    }
    Some(root_from(get)?.join(relative))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn staging_resolves_where_it_always_did() {
        let e = env(&[("STATE_DIRECTORY", "/var/lib/yantrik-mind"), ("YM_DB", "/var/lib/yantrik-mind/mind.db")]);
        assert_eq!(root_from(&e), Some(PathBuf::from("/var/lib/yantrik-mind")));
        assert_eq!(file_from(&e, "YM_TAPE_PATH", "tape.jsonl"), Some(PathBuf::from("/var/lib/yantrik-mind/tape.jsonl")));
    }

    #[test]
    fn a_person_mind_lands_in_its_own_folder() {
        let e = env(&[("YM_DB", "/var/lib/yantrik-mind/1001/mind.db")]);
        assert_eq!(file_from(&e, "YM_WEB_DIR", "public"), Some(PathBuf::from("/var/lib/yantrik-mind/1001/public")));
        let e = env(&[("STATE_DIRECTORY", "/var/lib/yantrik-mind/1001:/var/lib/other")]);
        assert_eq!(root_from(&e), Some(PathBuf::from("/var/lib/yantrik-mind/1001")));
        let e = env(&[("YM_STATE_DIR", "/s"), ("STATE_DIRECTORY", "/t"), ("YM_DB", "/u/mind.db")]);
        assert_eq!(root_from(&e), Some(PathBuf::from("/s")), "the explicit root wins");
        let e = env(&[("YM_TAPE_PATH", "/elsewhere/tape.jsonl"), ("YM_DB", "/u/mind.db")]);
        assert_eq!(file_from(&e, "YM_TAPE_PATH", "tape.jsonl"), Some(PathBuf::from("/elsewhere/tape.jsonl")), "a specific override wins");
    }

    /// E.ROOT1: the literal never comes back. Every crate's code, outside comments and test modules
    /// (everything after a file's first `#[cfg(test)]`, and `tests.rs` files), may name the old
    /// shared folder only in the `ym setup` template -- which writes it into a config file a person
    /// then owns, and is never a fallback.
    #[test]
    fn no_code_guesses_the_state_folder() {
        const LITERAL: &str = concat!("/var/lib/", "yantrik-mind");
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
        let mut found: Vec<String> = Vec::new();
        let mut stack = vec![crates.clone()];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    if !matches!(p.file_name().and_then(|n| n.to_str()), Some("target" | "fixtures" | "tests")) {
                        stack.push(p);
                    }
                    continue;
                }
                if p.extension().and_then(|x| x.to_str()) != Some("rs") || p.file_name().and_then(|n| n.to_str()) == Some("tests.rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&p).unwrap();
                for (i, line) in text.lines().enumerate() {
                    if line.trim_start().starts_with("#[cfg(test)]") {
                        break;
                    }
                    let t = line.trim_start();
                    if t.starts_with("//") || t.starts_with('*') || !line.contains(LITERAL) {
                        continue;
                    }
                    let rel = p.strip_prefix(&crates).unwrap().to_string_lossy().replace('\\', "/");
                    found.push(format!("{rel}:{}", i + 1));
                }
            }
        }
        found.sort();
        let (template, rest): (Vec<_>, Vec<_>) = found.into_iter().partition(|f| f.starts_with("mind-core/src/setup.rs:"));
        assert!(rest.is_empty(), "a guessed state folder came back: {rest:?}");
        assert_eq!(template.len(), 2, "the `ym setup` template changed: {template:?}");
    }

    #[test]
    fn nothing_set_means_no_state_path() {
        assert_eq!(root_from(env(&[])), None);
        assert_eq!(root_from(env(&[("YM_DB", ":memory:")])), None);
        assert_eq!(root_from(env(&[("YM_DB", "mind.db")])), None, "a bare file name has no folder to trust");
        assert_eq!(file_from(env(&[("YM_STATE_DIR", "  ")]), "X", "x"), None, "blank is not set");
    }
}
