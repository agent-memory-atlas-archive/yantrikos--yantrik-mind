# Release notes — v0.2.0

**yantrik-mind** is a ground-up Rust AI companion built on the YantrikDB typed-memory moat — typed beliefs with Bayesian revision, contradiction detection, research that revises its own memory, multi-LLM routing, persistent delegation, an NL planner, a parallel worker pool, a code sandbox, and a deterministic harm-gate — all in one binary. v0.2.0 makes it a harness on Yantrik OS: the Mind attaches to the desktop, acts on it, and shares the person's memory with other minds under per-person walls.

**Before upgrading an existing install, read [Upgrade from v0.1.x](#upgrade-from-v01x):** the engine moves forward and migrates `mind.db` on first start.

## What's new in v0.2.0

### A Yantrik OS harness
- **Attaches to the desktop** through the OS's minds' door (`YANTRIK_MIND_RUN`) and answers what is typed there. Built for one Mind per person: the state root, the door and the memory socket are all per person.
- **Acts on the desktop** through the OS's own tools (`os_describe`, `os_act`, `os_screen`, `web_*`). The loop reads before it acts. It treats a read taken before an act as stale. It knows an app that is still starting from one that is gone, and retries only when the world has changed. Tool calls show as cards on the desktop.
- **Confirms by consequence, not by tool.** A new `LocalControl` capability covers actions that stay on this machine. When a reversible, low-risk one is declared, it runs without a typed "yes". Reaching outward still asks. For the desktop's browser, the desktop's own card decides, and commitments (buy, send, delete) raise it every time.
- **Time and place are the person's.** "now" is the machine's timezone, and the weather defaults to the person's place, not London.
- **Model set from Settings.** The person picks the provider at first run, and the key goes in from *Settings → AI & Intelligence → Yantrik Mind → Model*. It never passes through a transcript or root.
- **Private mode.** When the desktop turns the Mind off, the Mind backs off and stays quiet.
- **Measured.** An arena runs every mind through the same door on the same tasks, judged by the desktop. The OS promotes a build only after the Mind gate passes. The latest reading is 7/7 with 0 false claims.

### Shared memory with walls
- **Memory over MCP** (`mind-memory-mcp`): one engine on the file, served by whoever owns it, over the person's unix socket (`SO_PEERCRED`).
- **Credentials vouched for by the desktop.** Every tool checks its grant. Another mind reads as an Agent: ordinary classes by default, never credentials.
- **Authorship.** Every belief records which mind wrote it, and the caller can only claim it. A mind can forget only what it alone wrote.
- **No inflation.** Another mind repeating a claim is not more evidence.

### The network
- **Every request honours the machine's egress proxy** (`HTTPS_PROXY`/`ALL_PROXY`). Loopback and `NO_PROXY` hosts go direct. All HTTP goes through one crate, `mind-net`, and a scan test fails the build on any request that bypasses it.
- **`web_fetch` no longer sends pages to a third-party reader by default.** The ladder is direct, then a local headless browser. The r.jina.ai reader runs only with `YM_WEB_READER=jina`, and text read through it says so.

### Harm-gate
- **The secret check now covers `LocalControl`,** so a credential typed into a note is refused like one sent outward. The change only makes the gate stricter, and a human reviewed it.

---

## What was new in v0.1.1

### `:beliefs [query]` — inspect the cognitive graph from the REPL

The new `:beliefs` REPL command (and its Telegram `/beliefs` equivalent) lets you see exactly what the mind knows and how confident it is:

```
:beliefs
• The latest stable Rust version is 1.96.0. (0.81)
• Pranab prefers terse replies (0.74)
• ...

:beliefs rust
• The latest stable Rust version is 1.96.0. (0.81)
```

- Lists the top 10 stored beliefs ranked by Bayesian confidence score, highest first.
- Accepts an optional query argument for semantic filtering — paraphrases work, not just keywords.
- Empty store returns `(no beliefs stored)` rather than a blank screen.
- This command was authored and merged by yantrik-mind's own bounded self-build pipeline (PR #2), making it the first capability the system added to itself.

---

## Full capability summary

### Typed memory that compounds
Beliefs are stored as typed nodes in YantrikDB's cognitive graph — not flat text. Each belief carries a Bayesian confidence score, a full evidence trail (`told` / `inferred` / `extracted` / `consolidated`), and contradiction edges to conflicting beliefs. Positive and negative evidence update confidence via log-odds revision. Consolidation (`consolidate` / `:consolidate`) distils recent conversation turns into durable typed beliefs and tracked commitments in a single LLM pass — the memory compounds instead of truncating. Semantic recall (bundled model2vec, dim 64, no external server) retrieves the right belief from a paraphrase with no shared keywords.

### Research that revises prior beliefs
`research and update X` recalls what the mind already believes near a topic, researches it live with cited sources (keyless DuckDuckGo + SSRF-guarded fetch), reconciles findings against priors, asserts new facts, applies negative Bayesian evidence to stale beliefs, and draws contradiction edges — all in one turn. `deep dive on X` fans multiple sub-agents in parallel, then runs an adversarial fact-check pass over the synthesis. Flat-RAG companions have no revisable typed belief to update and no evidence trail to grow.

### Multi-LLM resilient routing
Five OpenAI-compatible providers out of the box: NanoGPT, Ollama Cloud, MiniMax, OpenRouter, Grok. A `ChainBackend` falls over past errors and empty replies automatically. A per-function `Router` lets each role (`chat`, `research`, `util`, `verify`, `code`, `consolidate`) be pinned to a different provider and model via `YM_ROLE_<ROLE>` — without touching code.

### Agentic coder on Claude
`code: X` dispatches Claude Code via MiniMax's Anthropic-compatible endpoint into an isolated scratch directory with a secret-stripped child environment. Outward effects still require explicit confirmation.

### Parallel sub-agents
Bounded ReAct loops (step budget enforced) over a granted read-tool subset. `fan_out` runs tasks concurrently via the inference pool. Act-capable agents propose but cannot self-confirm outward actions — confirmation is always routed back to the user.

### Persistent delegation and monitors
Spoken commitments are extracted by consolidation into tracked tasks with due dates. Long-running monitors (`watch my inbox for X`, `watch <url> for X`, `watch my github for X`) persist across restarts via SQLite-backed recipes (idempotent; failed-visibly on recovery, never double-executed).

### NL planner (recipe engine)
Natural-language goals (`plan: X`, `automate X`) are turned into typed recipes with Think, Act, AskUser, WaitUntil, and WaitForCondition steps. AskUser pauses mid-execution and resumes with the next message.

### Code sandbox and semantic skill library
Isolated execution (user namespaces, no network, state dir masked) for Python, shell, and Rust. Green runs can be saved as named skills recalled by meaning rather than exact name. Skills are auto-quarantined on repeated failures. Remote execution (`worker python: …` / `worker shell: …`) fans work out to a pool of SSH workers.

### Deterministic, property-tested harm-gate
A single inviolable gate: no LLM in the loop, deny-by-default for governed capabilities, not overridable at runtime. Blocks weapons synthesis instructions, self-harm facilitation, malware deployment, credential exfiltration on any outward channel, writes to protected paths (`.ssh`, `/etc/`, `.env`, …), and mass-targeting (>5 recipients). Two normalisation passes resist obfuscation (whitespace/zero-width collapse + leet-folded squeeze). Monotonic toward safety: adding text to a denied intent can only deepen the denial. A checked-in adversarial corpus of jailbreaks and injections must stay denied — any regression is a build break. `execute()` re-checks the gate independently of `decide()` for defence in depth.

### Bounded self-build
The mind can open bounded draft pull requests against its own codebase: it compiles first (no build break → no PR), stages the diff, and posts via the GitHub API. `crates/mind-governance` is excluded from self-modification. v0.1.1 itself is the first release to include a capability shipped this way.

---

## Architecture

21 `mind-*` crates with a narrow-waist design around six contracts in `mind-types`: `Event`, `MemoryFacade`, `Candidate`/`ActionIntent`, `HarmGate`, `TurnContext`, `ActionRuntime`. The DAG is acyclic and enforced by review; `mind-core` holds only handles and the main loop — zero domain state.

The `MemoryFacade` runs on a dedicated thread (YantrikDB is `!Sync`) behind an async mpsc+oneshot client, with priority lanes (Interactive ≫ CommitmentDue ≫ Background ≫ Bulk) to bound head-of-line latency. The inference pool uses `spawn_blocking` + a semaphore to keep the async executor free during synchronous LLM calls.

Requires Rust 1.91+, edition 2021, multi-thread tokio. No GPU required for API-backed providers.

---

## Upgrade from v0.1.x

**The engine moves from yantrikdb 0.21.2 to 0.23.0, and `mind.db` migrates forward on first start** (schema 52 → 54). The move was checked against the published crates:
- belief revision, recall ranking and the bundled embedder are unchanged;
- sealed packs still mount;
- a copy of a live store migrated with every count unchanged.

0.21.2 can still open a migrated store, so rolling back the binary does not damage the file. Still, **back up `mind.db` before the first start:**

```sh
systemctl stop yantrik-mind
cp -p /var/lib/yantrik-mind/mind.db /var/lib/yantrik-mind/mind.db.pre-0.2.0
git pull
cargo build --release --locked -p mind-core
```

Behaviour that changes without configuration:
- **Outbound HTTP follows `HTTPS_PROXY`/`ALL_PROXY` when set.** With no proxy variables set, nothing changes.
- **`web_fetch` stops using the r.jina.ai reader.** Set `YM_WEB_READER=jina` to keep it for sites that block direct requests.
