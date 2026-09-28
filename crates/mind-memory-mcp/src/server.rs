//! The tools, over one `MemoryHandle`.
//!
//! Two halves, one engine instance:
//!
//! - **Beliefs** — what a mind has come to hold true, with evidence and confidence. These go
//!   through this mind's own belief code, so scope, sensitivity, versioning and tombstones apply.
//! - **Memories** — flat records a mind writes and recalls by meaning, in a namespace.
//!
//! `recall` returns both at once, labelled, because a mind asking what it knows about something
//! should not have to know which half the answer was filed under.

use mind_memory::{MemoryHandle, MemoryHit, MemoryWrite, WrittenBy, WRITTEN_BY_KEY};
use mind_types::{AccessContext, AgentFooting, Belief, BeliefAssertion, MemoryFacade, RecallQuery};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{schemars, tool, tool_router, ErrorData as McpError, ServerHandler};
use serde_json::{json, Value};

/// Who is calling, as the server knows it (E.GRANT2).
#[derive(Clone, Debug)]
pub enum Caller {
    /// Whoever holds the machine's one memory token -- today's only caller, with the machine
    /// owner's full view, until the desktop's per-mind credentials are validated (#447 item 3).
    MachineToken,
    /// Another mind, on a credential the desktop validated: its own footing and its own grants.
    Agent(AgentFooting),
}

fn ok(v: Value) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(&v).unwrap_or_default(),
    )]))
}

fn fail(what: &str, e: impl std::fmt::Debug) -> McpError {
    McpError::internal_error(format!("{what} failed: {e:?}"), None)
}

fn belief_json(b: &Belief) -> Value {
    json!({
        "kind": "belief",
        "id": b.id,
        "statement": b.statement,
        "confidence": (b.confidence * 1000.0).round() / 1000.0,
        "evidence_count": b.evidence_count,
        "provenance": b.provenance,
        "status": b.status,
        "updated_ms": b.updated_ms,
    })
}

fn memory_json(m: &MemoryHit) -> Value {
    // E.STAMP1: who wrote it, as the server stamped it -- shown on its own, and taken out of the
    // caller's metadata so the two are never confused. `source` stays what the writer said.
    let mut metadata = m.metadata.clone();
    let written_by = metadata
        .as_object_mut()
        .and_then(|o| o.remove(WRITTEN_BY_KEY))
        .unwrap_or_else(|| json!("before stamping"));
    json!({
        "kind": "memory",
        "rid": m.rid,
        "text": m.text,
        "score": (m.score * 10000.0).round() / 10000.0,
        "memory_type": m.memory_type,
        "namespace": m.namespace,
        "domain": m.domain,
        "source": m.source,
        "created_at": m.created_at,
        "importance": m.importance,
        "written_by": written_by,
        "metadata": metadata,
        "why_retrieved": m.why_retrieved,
    })
}

// ── Inputs ────────────────────────────────────────────────────────────────────────────

fn d_memory_type() -> String { "semantic".into() }
fn d_importance() -> f64 { 0.5 }
fn d_namespace() -> String { "default".into() }
fn d_domain() -> String { "general".into() }
fn d_source() -> String { "mcp".into() }
fn d_top_k() -> usize { 8 }
fn d_include() -> String { "all".into() }
fn d_direction() -> String { "supports".into() }
fn d_strength() -> f64 { 1.0 }
fn d_provenance() -> String { "told".into() }
fn d_limit() -> usize { 10 }
fn d_weight() -> f64 { 1.0 }

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RememberInput {
    /// What to remember, as a self-contained sentence.
    pub text: String,
    /// semantic (a fact), episodic (something that happened), procedural (how to do something).
    #[serde(default = "d_memory_type")]
    pub memory_type: String,
    /// 0.0 trivial to 1.0 critical.
    #[serde(default = "d_importance")]
    pub importance: f64,
    /// Whose memory this is. "default" is shared by every mind on this machine.
    #[serde(default = "d_namespace")]
    pub namespace: String,
    /// e.g. general, work, health, preference.
    #[serde(default = "d_domain")]
    pub domain: String,
    /// Who is writing: the mind's name, "user", "system".
    #[serde(default = "d_source")]
    pub source: String,
    /// Your own fields, returned with the memory on recall — e.g. `{"source": "extracted"}` to mark
    /// an unconfirmed guess. At most 16 KB.
    pub metadata: Option<serde_json::Map<String, Value>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RecallInput {
    /// A short natural-language question or topic.
    pub query: String,
    /// How many results from each half.
    #[serde(default = "d_top_k")]
    pub top_k: usize,
    /// Narrow flat memories to one namespace. Beliefs are not namespaced.
    pub namespace: Option<String>,
    /// all, beliefs, or memories.
    #[serde(default = "d_include")]
    pub include: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BelieveInput {
    /// The proposition, as a plain sentence.
    pub statement: String,
    /// supports or contradicts.
    #[serde(default = "d_direction")]
    pub direction: String,
    /// How strong this piece of evidence is, roughly 0.5 weak to 3.0 very strong.
    #[serde(default = "d_strength")]
    pub strength: f64,
    /// Where the evidence came from, e.g. "user said so on 2026-09-16".
    pub source: Option<String>,
    /// told, observed, inferred, extracted.
    #[serde(default = "d_provenance")]
    pub provenance: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BeliefsInput {
    /// Words the belief should contain.
    pub query: String,
    #[serde(default = "d_limit")]
    pub limit: usize,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct StatementInput {
    /// The belief's statement, exactly as returned by `beliefs` or `recall`.
    pub statement: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ForgetInput {
    /// memory (by rid) or belief (by statement).
    pub kind: String,
    /// The memory's rid, or the belief's statement.
    pub target: String,
    /// Why — kept with the tombstone. Beliefs only.
    pub reason: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RelateInput {
    pub src: String,
    pub dst: String,
    /// e.g. works_on, lives_in, prefers.
    pub rel: String,
    #[serde(default = "d_weight")]
    pub weight: f64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ReflectInput {
    /// What to reflect on.
    pub question: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NoInput {}

// ── Server ────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct MemoryServer {
    mem: MemoryHandle,
    /// E.STAMP1/E.GRANT2: who is calling, as the server knows it.
    caller: Caller,
    /// E.DOOR2: served over HTTP, where every call must carry the caller its request was
    /// authenticated as -- a call without one is refused, never served as `caller`.
    http: bool,
    tool_router: ToolRouter<MemoryServer>,
}

impl MemoryServer {
    pub fn new(mem: MemoryHandle) -> Self {
        Self { mem, caller: Caller::MachineToken, http: false, tool_router: Self::tool_router() }
    }

    /// E.DOOR2: served over HTTP -- each call runs as the caller its request was authenticated as.
    pub fn over_http(mem: MemoryHandle) -> Self {
        Self { mem, caller: Caller::MachineToken, http: true, tool_router: Self::tool_router() }
    }

    /// E.DOOR2: the server this call runs on -- carrying the caller its HTTP request was
    /// authenticated as. Over HTTP a call with none is refused.
    fn for_call(&self, parts: Option<&axum::http::request::Parts>) -> Result<Self, McpError> {
        match parts.and_then(|p| p.extensions.get::<Caller>()) {
            Some(caller) => Ok(Self { caller: caller.clone(), ..self.clone() }),
            None if !self.http => Ok(self.clone()),
            None => Err(McpError::invalid_request("refused: this call carries no authenticated caller".to_string(), None)),
        }
    }

    /// E.GRANT2: the same server, speaking to another mind. Only a validated footing gets here.
    pub fn for_agent(mem: MemoryHandle, footing: AgentFooting) -> Self {
        Self { mem, caller: Caller::Agent(footing), http: false, tool_router: Self::tool_router() }
    }

    /// The footing every read runs under.
    fn ctx(&self) -> AccessContext {
        match &self.caller {
            Caller::MachineToken => AccessContext::operator_audit(),
            Caller::Agent(f) => AccessContext::Agent(f.clone()),
        }
    }

    /// Who a write is stamped as.
    fn written_by(&self) -> WrittenBy {
        match &self.caller {
            Caller::MachineToken => WrittenBy::machine_token(),
            Caller::Agent(f) => WrittenBy { mind: f.mind().to_string(), via: "mcp".into() },
        }
    }

    /// E.GRANT2: refuse before the engine is touched when this caller lacks `grant`. The refusal
    /// names the grant and nothing else.
    fn need(&self, grant: &str) -> Result<(), McpError> {
        match &self.caller {
            Caller::MachineToken => Ok(()),
            Caller::Agent(f) if f.has(grant) => Ok(()),
            Caller::Agent(_) => Err(McpError::invalid_params(
                format!("refused: this mind has no `{grant}` grant -- the person can give it in Settings -> Memory"),
                None,
            )),
        }
    }

    /// E.GRANT2: may this caller forget `target`? The machine token may forget anything, as
    /// today. Another mind only what it alone wrote.
    async fn may_forget(&self, kind: &str, target: &str) -> Result<(), McpError> {
        let Caller::Agent(f) = &self.caller else {
            return Ok(());
        };
        let own = match kind {
            "memory" => matches!(
                self.mem.memory_written_by(target).await.map_err(|e| fail("forget memory", e))?,
                Some(Some(w)) if w.mind == f.mind() && w.via == "mcp"
            ),
            "belief" => match self.mem.explain_belief(target, &self.ctx()).await.map_err(|e| fail("forget belief", e))? {
                Some((belief, evidence)) => {
                    let authors = self.mem.belief_authors(&belief.statement).await.map_err(|e| fail("forget belief", e))?;
                    let stamped: u64 = authors.iter().map(|a| a.evidence).sum();
                    !authors.is_empty()
                        && authors.iter().all(|a| a.mind == f.mind() && a.via == "mcp")
                        && stamped >= evidence.len() as u64
                }
                None => false,
            },
            _ => return Ok(()), // the tool itself refuses an unknown kind
        };
        if own {
            Ok(())
        } else {
            Err(McpError::invalid_params(
                "refused: this was not written by this mind alone -- forgetting it is the person's, in Settings -> Memory"
                    .to_string(),
                None,
            ))
        }
    }
}

#[tool_router]
impl MemoryServer {
    #[tool(description = "Store a flat memory — a fact, an event, or a how-to — recalled later by meaning. Returns its rid.")]
    async fn remember(&self, Parameters(i): Parameters<RememberInput>) -> Result<CallToolResult, McpError> {
        self.need("remember")?;
        let rid = self
            .mem
            .remember_memory(MemoryWrite {
                written_by: self.written_by(),
                text: i.text,
                memory_type: i.memory_type,
                importance: i.importance.clamp(0.0, 1.0),
                namespace: i.namespace,
                domain: i.domain,
                source: i.source,
                metadata: i.metadata.map(Value::Object).unwrap_or(Value::Null),
            })
            .await
            .map_err(|e| {
                // The caller's to fix, so said as invalid params: an agent that reads every
                // internal error as "memory is down" would stop saving anything over one bad write.
                if e.is_memory_write_gate_refusal() || matches!(e, mind_types::MindError::Invalid(_)) {
                    McpError::invalid_params(format!("remember refused: {e}"), None)
                } else {
                    fail("remember", e)
                }
            })?;
        ok(json!({ "rid": rid, "status": "stored" }))
    }

    #[tool(description = "Recall what this machine knows about something: beliefs and memories together, each labelled with its kind. Call this before answering anything that depends on the person, their work, or earlier conversations.")]
    async fn recall(&self, Parameters(i): Parameters<RecallInput>) -> Result<CallToolResult, McpError> {
        self.need("recall_ordinary")?;
        let top_k = i.top_k.clamp(1, 50);
        let want_beliefs = matches!(i.include.as_str(), "all" | "beliefs");
        let want_memories = matches!(i.include.as_str(), "all" | "memories");

        let beliefs = if want_beliefs {
            self.mem
                .recall_typed(RecallQuery { text: i.query.clone(), top_k, kind: None }, &self.ctx())
                .await
                .map_err(|e| fail("recall beliefs", e))?
        } else {
            Vec::new()
        };
        let memories = if want_memories {
            self.mem
                .recall_memories(&i.query, top_k, i.namespace.as_deref(), &self.ctx())
                .await
                .map_err(|e| fail("recall memories", e))?
        } else {
            Vec::new()
        };

        // Interleaved by rank, not merged by score. The two halves are scored by different
        // rankers on different scales, and sorting one number against the other would present a
        // comparison that means nothing. Each half keeps its own order; each result carries its
        // own score and says which half it came from.
        let belief_rows: Vec<Value> = beliefs
            .iter()
            .map(|r| {
                json!({
                    "kind": "belief",
                    "id": r.item.id,
                    "statement": r.item.text,
                    "confidence": (r.item.confidence * 1000.0).round() / 1000.0,
                    "evidence_count": r.item.evidence_count,
                    "score": (r.score * 10000.0).round() / 10000.0,
                    "why": r.why,
                })
            })
            .collect();
        let memory_rows: Vec<Value> = memories.iter().map(memory_json).collect();
        let mut results = Vec::with_capacity(belief_rows.len() + memory_rows.len());
        let (mut b, mut m) = (belief_rows.into_iter(), memory_rows.into_iter());
        loop {
            let (nb, nm) = (b.next(), m.next());
            if nb.is_none() && nm.is_none() {
                break;
            }
            results.extend(nb);
            results.extend(nm);
        }
        ok(json!({ "query": i.query, "count": results.len(), "results": results }))
    }

    #[tool(description = "Add evidence for or against a belief. Creates the belief if it is new; otherwise updates its confidence. Use for things the person has told you or you have established, not for passing remarks.")]
    async fn believe(&self, Parameters(i): Parameters<BelieveInput>) -> Result<CallToolResult, McpError> {
        self.need("believe")?;
        let polarity = match i.direction.as_str() {
            "supports" => 1.0,
            "contradicts" => -1.0,
            other => {
                return Err(McpError::invalid_params(
                    format!("direction must be supports or contradicts, got `{other}`"),
                    None,
                ))
            }
        };
        let belief = self
            .mem
            .remember_as_belief_by(
                BeliefAssertion {
                    statement: i.statement,
                    polarity,
                    weight: i.strength.clamp(0.0, 6.0),
                    source_event: i.source,
                    provenance: i.provenance,
                },
                self.written_by(),
            )
            .await
            .map_err(|e| fail("believe", e))?;
        ok(belief_json(&belief))
    }

    #[tool(description = "List beliefs whose statement contains these words, most confident first.")]
    async fn beliefs(&self, Parameters(i): Parameters<BeliefsInput>) -> Result<CallToolResult, McpError> {
        self.need("recall_ordinary")?;
        let found = self
            .mem
            .beliefs_matching_n(&i.query, i.limit.clamp(1, 100), &self.ctx())
            .await
            .map_err(|e| fail("beliefs", e))?;
        ok(json!({ "count": found.len(), "beliefs": found.iter().map(belief_json).collect::<Vec<_>>() }))
    }

    #[tool(description = "Show a belief with every piece of evidence behind it.")]
    async fn explain(&self, Parameters(i): Parameters<StatementInput>) -> Result<CallToolResult, McpError> {
        self.need("recall_ordinary")?;
        match self.mem.explain_belief(&i.statement, &self.ctx()).await.map_err(|e| fail("explain", e))? {
            Some((belief, evidence)) => {
                // E.STAMP1: who put the evidence there, and how much of it nobody stamped.
                let authors = self.mem.belief_authors(&belief.statement).await.map_err(|e| fail("explain authors", e))?;
                let stamped: u64 = authors.iter().map(|a| a.evidence).sum();
                ok(json!({
                "belief": belief_json(&belief),
                "contributors": authors,
                "unattributed_evidence": (evidence.len() as u64).saturating_sub(stamped),
                "evidence": evidence.iter().map(|e| json!({
                    "weight": e.weight,
                    "polarity": e.polarity,
                    "source": e.source_event,
                    "excerpt": e.excerpt,
                })).collect::<Vec<_>>(),
            }))
            }
            None => ok(json!({ "found": false, "statement": i.statement })),
        }
    }

    #[tool(description = "Forget a memory (by rid) or a belief (by statement). Nothing is erased: it is tombstoned and stops being recalled.")]
    async fn forget(&self, Parameters(i): Parameters<ForgetInput>) -> Result<CallToolResult, McpError> {
        self.may_forget(&i.kind, &i.target).await?;
        let done = match i.kind.as_str() {
            "memory" => self.mem.forget_memory(&i.target).await.map_err(|e| fail("forget memory", e))?,
            "belief" => match i.reason.as_deref() {
                Some(r) => self.mem.forget_with_reason(&i.target, r).await,
                None => self.mem.forget(&i.target).await,
            }
            .map_err(|e| fail("forget belief", e))?,
            other => {
                return Err(McpError::invalid_params(
                    format!("kind must be memory or belief, got `{other}`"),
                    None,
                ))
            }
        };
        ok(json!({ "forgotten": done, "kind": i.kind, "target": i.target }))
    }

    #[tool(description = "Open contradictions between beliefs, most severe first.")]
    async fn conflicts(&self, Parameters(_): Parameters<NoInput>) -> Result<CallToolResult, McpError> {
        self.need("recall_ordinary")?;
        let found = self.mem.conflicts(&self.ctx()).await.map_err(|e| fail("conflicts", e))?;
        ok(json!({ "count": found.len(), "conflicts": found }))
    }

    #[tool(description = "Record a relationship between two things, e.g. Pranab --lives_in--> Bentonville.")]
    async fn relate(&self, Parameters(i): Parameters<RelateInput>) -> Result<CallToolResult, McpError> {
        self.need("believe")?;
        self.mem
            .relate(&i.src, &i.dst, &i.rel, i.weight)
            .await
            .map_err(|e| fail("relate", e))?;
        ok(json!({ "related": true, "src": i.src, "rel": i.rel, "dst": i.dst }))
    }

    #[tool(description = "Reflect on a question: the relevant beliefs, open conflicts, goals and preferences, summarised.")]
    async fn reflect(&self, Parameters(i): Parameters<ReflectInput>) -> Result<CallToolResult, McpError> {
        self.need("recall_ordinary")?;
        let r = self.mem.reflect(&i.question, &self.ctx()).await.map_err(|e| fail("reflect", e))?;
        ok(json!({
            "summary": r.summary,
            "beliefs": r.beliefs.iter().map(belief_json).collect::<Vec<_>>(),
            "open_conflicts": r.open_conflicts,
            "goals": r.goals.iter().map(|g| &g.text).collect::<Vec<_>>(),
            "preferences": r.preferences.iter().map(|p| &p.text).collect::<Vec<_>>(),
        }))
    }
}

// E.DOOR2: written out rather than generated by `#[tool_handler]`, so each call is served on a
// clone carrying the caller its HTTP request was authenticated as.
impl ServerHandler for MemoryServer {
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let server = self.for_call(context.extensions.get::<axum::http::request::Parts>())?;
        let tcc = rmcp::handler::server::tool::ToolCallContext::new(&server, request, context);
        server.tool_router.call(tcc).await
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult { tools: self.tool_router.list_all(), meta: None, next_cursor: None })
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tool_router.get(name).cloned()
    }

    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("yantrik-memory", env!("CARGO_PKG_VERSION")))
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_instructions(
                "This is the machine's memory, shared by every mind that runs on it. What you \
                 store here, the next mind will know.\n\n\
                 - Before answering anything that depends on the person, their work, or an earlier \
                 conversation, call `recall`. It returns beliefs and memories together.\n\
                 - When the person tells you something durable about themselves, their work or \
                 their preferences, call `believe` with direction supports.\n\
                 - When something happens or you learn a fact worth keeping, call `remember`.\n\
                 - When the person corrects you, call `believe` with direction contradicts on the \
                 old statement, then `believe` the new one.",
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(r: CallToolResult) -> Value {
        let t = &r.content[0].as_text().expect("a text result").text;
        serde_json::from_str(t).unwrap()
    }
    fn input<T: serde::de::DeserializeOwned>(v: Value) -> Parameters<T> {
        Parameters(serde_json::from_value(v).unwrap())
    }

    /// E.STAMP1: a memory written through the server says who wrote it, as the server knows it;
    /// a caller cannot say it for itself; a record from before stamping says so.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_memory_says_who_wrote_it_and_the_caller_cannot() {
        let s = MemoryServer::new(MemoryHandle::spawn(":memory:", 64).unwrap());
        s.remember(input(json!({"text": "The bike is kept in the garage", "source": "hermes", "metadata": {"kind": "fact"}})))
            .await
            .unwrap();
        let got = text(s.recall(input(json!({"query": "where is the bike kept", "include": "memories"}))).await.unwrap());
        let m = &got["results"][0];
        assert_eq!(m["written_by"]["mind"], "machine-token", "{m}");
        assert_eq!(m["written_by"]["via"], "mcp", "{m}");
        assert_eq!(m["source"], "hermes", "the writer's own word is kept");
        assert!(m["metadata"].get(WRITTEN_BY_KEY).is_none(), "the stamp leaked into the caller's metadata: {m}");

        let forged = s
            .remember(input(json!({"text": "Forged", "metadata": {WRITTEN_BY_KEY: {"mind": "yantrik-mind", "via": "self"}}})))
            .await;
        assert!(forged.is_err(), "a caller set its own author");

        let old = MemoryHit {
            rid: "r".into(),
            text: "t".into(),
            score: 0.0,
            memory_type: "semantic".into(),
            namespace: "default".into(),
            domain: "general".into(),
            source: "mcp".into(),
            created_at: 0.0,
            importance: 0.5,
            metadata: json!({"source": "mcp"}),
            why_retrieved: vec![],
        };
        assert_eq!(memory_json(&old)["written_by"], "before stamping");
    }

    /// E.STAMP1: a belief lists who put its evidence there -- a client through the server, and
    /// the Mind in its own turns -- with nothing unattributed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_belief_lists_who_put_its_evidence_there() {
        let mem = MemoryHandle::spawn(":memory:", 64).unwrap();
        let s = MemoryServer::new(mem.clone());
        let said = "The person prefers tea in the morning";
        s.believe(input(json!({"statement": said, "source": "hermes"}))).await.unwrap();
        mem.remember_as_belief(BeliefAssertion {
            statement: said.into(),
            polarity: 1.0,
            weight: 1.0,
            source_event: None,
            provenance: "told".into(),
        })
        .await
        .unwrap();
        let got = text(s.explain(input(json!({"statement": said}))).await.unwrap());
        let who: Vec<(String, u64)> = got["contributors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| (a["mind"].as_str().unwrap().to_string(), a["evidence"].as_u64().unwrap()))
            .collect();
        assert_eq!(who, vec![("machine-token".to_string(), 1), ("yantrik-mind".to_string(), 1)], "{got}");
        assert_eq!(got["unattributed_evidence"], 0, "{got}");
    }

    fn agent(mem: &MemoryHandle, grants: &[&str]) -> MemoryServer {
        let answer = json!({"v": 1, "person_uid": 1000, "mind": "hermes", "attach": "hermes:c1@s1", "grants": grants, "valid_for_ms": 2000});
        MemoryServer::for_agent(mem.clone(), AgentFooting::from_validation(&answer, 1000).unwrap().0)
    }

    /// E.GRANT2: an agent with no grants is refused on every tool but forget, before the engine
    /// is touched; with `recall_ordinary` it reads, and still cannot see a Health belief.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn every_tool_asks_for_its_grant() {
        let mem = MemoryHandle::spawn(":memory:", 64).unwrap();
        let bare = agent(&mem, &[]);
        let refused = |r: Result<CallToolResult, McpError>| r.err().map(|e| e.message.to_string()).unwrap_or_default();
        assert!(refused(bare.remember(input(json!({"text": "x"}))).await).contains("`remember`"));
        assert!(refused(bare.recall(input(json!({"query": "x"}))).await).contains("`recall_ordinary`"));
        assert!(refused(bare.believe(input(json!({"statement": "x"}))).await).contains("`believe`"));
        assert!(refused(bare.beliefs(input(json!({"query": "x"}))).await).contains("`recall_ordinary`"));
        assert!(refused(bare.explain(input(json!({"statement": "x"}))).await).contains("`recall_ordinary`"));
        assert!(refused(bare.conflicts(input(json!({}))).await).contains("`recall_ordinary`"));
        assert!(refused(bare.relate(input(json!({"src": "a", "dst": "b", "rel": "r"}))).await).contains("`believe`"));
        assert!(refused(bare.reflect(input(json!({"question": "x"}))).await).contains("`recall_ordinary`"));

        let health = "The dentist appointment is on Friday";
        mem.remember_as_belief(BeliefAssertion { statement: health.into(), polarity: 1.0, weight: 2.0, source_event: None, provenance: "told".into() })
            .await
            .unwrap();
        mem.set_belief_sensitivity(health, mind_types::Sensitivity::Health).await.unwrap();
        let reader = agent(&mem, &["recall_ordinary"]);
        let got = text(reader.beliefs(input(json!({"query": "dentist Friday"}))).await.unwrap());
        assert_eq!(got["count"], 0, "an agent without recall_health saw a Health belief: {got}");
        let owner = MemoryServer::new(mem.clone());
        let got = text(owner.beliefs(input(json!({"query": "dentist Friday"}))).await.unwrap());
        assert_eq!(got["count"], 1, "the machine token's view changed: {got}");
    }

    /// E.GRANT2: an agent's writes carry its own name, and it forgets only what it alone wrote.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_mind_forgets_only_what_it_alone_wrote() {
        let mem = MemoryHandle::spawn(":memory:", 64).unwrap();
        let hermes = agent(&mem, &["recall_ordinary", "remember", "believe"]);
        let owner = MemoryServer::new(mem.clone());

        let mine = text(hermes.remember(input(json!({"text": "Hermes noted the porch light is broken"}))).await.unwrap())["rid"].as_str().unwrap().to_string();
        let theirs = text(owner.remember(input(json!({"text": "The porch step is loose"}))).await.unwrap())["rid"].as_str().unwrap().to_string();
        let got = text(hermes.recall(input(json!({"query": "porch light broken", "include": "memories"}))).await.unwrap());
        let row = got["results"].as_array().unwrap().iter().find(|r| r["rid"] == mine.as_str()).cloned().unwrap();
        assert_eq!(row["written_by"]["mind"], "hermes", "{row}");

        assert!(hermes.forget(input(json!({"kind": "memory", "target": theirs}))).await.is_err(), "forgot a memory it did not write");
        assert!(hermes.forget(input(json!({"kind": "memory", "target": mine}))).await.is_ok(), "could not forget its own memory");

        let solo = "The garden hose lives in the shed";
        let shared = "The recycling goes out on Thursday";
        hermes.believe(input(json!({"statement": solo}))).await.unwrap();
        hermes.believe(input(json!({"statement": shared}))).await.unwrap();
        mem.remember_as_belief(BeliefAssertion { statement: shared.into(), polarity: 1.0, weight: 1.0, source_event: None, provenance: "told".into() })
            .await
            .unwrap();
        assert!(hermes.forget(input(json!({"kind": "belief", "target": shared}))).await.is_err(), "forgot a belief the Mind also holds");
        assert!(hermes.forget(input(json!({"kind": "belief", "target": solo}))).await.is_ok(), "could not forget a belief only it asserted");
        assert!(owner.forget(input(json!({"kind": "belief", "target": shared}))).await.is_ok(), "the machine token's forget changed");
    }

    /// E.DOOR2: over HTTP a call runs as the caller its request carries, and one carrying none is
    /// refused -- never served as the server's default.
    #[test]
    fn an_http_call_without_a_caller_is_refused() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mem = rt.block_on(async { MemoryHandle::spawn(":memory:", 8).unwrap() });
        let http = MemoryServer::over_http(mem.clone());
        let (mut parts, _) = axum::http::Request::builder().body(()).unwrap().into_parts();
        assert!(http.for_call(None).is_err(), "a call with no request was served");
        assert!(http.for_call(Some(&parts)).is_err(), "a request with no caller was served");
        let answer = json!({"v": 1, "person_uid": 1000, "mind": "hermes", "attach": "a", "grants": []});
        parts.extensions.insert(Caller::Agent(AgentFooting::from_validation(&answer, 1000).unwrap().0));
        let served = http.for_call(Some(&parts)).unwrap();
        assert!(matches!(served.caller, Caller::Agent(ref f) if f.mind() == "hermes"));
        assert!(matches!(MemoryServer::new(mem).for_call(None).unwrap().caller, Caller::MachineToken), "stdio keeps its caller");
    }

    /// E.STAMP1: a stale versioned update is dropped, and adds no stamp.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_dropped_update_is_not_stamped() {
        let mem = MemoryHandle::spawn(":memory:", 64).unwrap();
        let a = || BeliefAssertion {
            statement: "The printer is on the second floor".into(),
            polarity: 1.0,
            weight: 1.0,
            source_event: None,
            provenance: "told".into(),
        };
        mem.remember_as_belief_versioned(a(), 5).await.unwrap();
        mem.remember_as_belief_versioned(a(), 3).await.unwrap();
        let authors = mem.belief_authors("The printer is on the second floor").await.unwrap();
        assert_eq!(authors.len(), 1);
        assert_eq!(authors[0].evidence, 1, "the dropped update was stamped: {authors:?}");
    }
}
