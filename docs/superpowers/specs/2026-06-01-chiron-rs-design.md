# chiron-rs: Rust Rewrite of the Feynman Chiron Backend

**Date:** 2026-06-01  
**Status:** Approved

---

## Overview

Replace `chiron_agent.py` + `chiron_storage.py` with a single self-contained Rust binary at
`feynman-chiron-package/chiron-rs/`. The Emacs frontend (`feynman-chiron.el`) is unchanged —
the binary speaks the same stdin/stdout JSON protocol as the Python backend.

### Goals

- Eliminate Python runtime and langchain/langgraph dependency chain
- Embed MiniLM sentence transformer (384 dims) via pure-Rust candle — no Python subprocess for embeddings
- Support Anthropic Messages API and Groq (OpenAI-compatible) as LLM providers
- Preserve full feature parity: RAG, knowledge graph, checkpoints, mastery tracking

### Non-goals

- GPU acceleration (CPU-only candle backend)
- Dynamic graph topology (fixed four-node pipeline)
- Configurable retry/backoff on LLM errors (fail fast, surface to Emacs)

---

## Architecture

The learning pipeline is a fixed four-stage sequence with one conditional branch:

```
stdin JSON command
    │
    ▼
main.rs (readline loop)
    │
    ▼
agent.rs  process_explanation()
    │
    ├─ retrieve()   ── embeddings.rs (candle MiniLM)
    │                  storage.rs    (pgvector similarity search)
    │
    ├─ analyze()    ── llm.rs        (Anthropic / Groq HTTP)
    │
    ├─ [gaps?] ──── probe()    ── llm.rs
    │           └── evaluate() ── llm.rs + storage.rs
    │
    ▼
stdout JSON response
```

No graph framework. Each stage is a plain `async fn` taking owned state and returning `Result<ChironState>`.

---

## Module Structure

### `src/types.rs`

All shared data types. No logic.

```rust
pub enum Provider {
    Anthropic { api_key: String, model: String },
    Groq      { api_key: String, model: String },
}

pub enum Stage { Initial, Analyze, Probe, Evaluate, Complete }

pub struct Gap {
    pub kind:  String,
    pub issue: String,
}

pub struct ChironState {
    pub concept:           String,
    pub textbook_context:  String,
    pub explanations:      Vec<String>,
    pub gaps:              Vec<Gap>,
    pub stage:             Stage,
    pub mastered_concepts: HashMap<String, serde_json::Value>,
    pub thread_id:         String,
    pub textbook_sources:  Vec<String>,  // names; agent resolves to pool
}

// stdin commands
pub enum Command {
    Ready,
    Process { concept: String, explanation: String,
              textbook_sources: Vec<String>, thread_id: String },
    GetMastered { thread_id: String },
    Reset,
}

// stdout responses
pub struct Response {
    pub success: bool,
    pub response: Option<String>,
    pub error:    Option<String>,
    // ...other fields per command
}
```

### `src/llm.rs`

Single function. Dispatches on `Provider` variant.

```rust
pub async fn chat(provider: &Provider, messages: &[Message]) -> Result<String>
```

- `Provider::Anthropic` → POST `https://api.anthropic.com/v1/messages`
  - Headers: `x-api-key`, `anthropic-version: 2023-06-01`, `content-type`
  - Body: `{ model, max_tokens: 1024, system, messages }`
  - Extract `.content[0].text`

- `Provider::Groq` → POST `https://api.groq.com/openai/v1/chat/completions`
  - Headers: `Authorization: Bearer <key>`
  - Body: OpenAI chat completions format
  - Extract `.choices[0].message.content`

No SDK. Raw `reqwest` + `serde_json`. Timeouts: 30 s connect, 120 s read.

### `src/embeddings.rs`

Loaded once at startup. Cached model weights via `hf-hub` (default cache `~/.cache/huggingface/hub`).

```rust
pub struct Embedder {
    model:     BertModel,
    tokenizer: Tokenizer,
}

impl Embedder {
    pub fn new() -> Result<Self>
        // hf-hub downloads "sentence-transformers/all-MiniLM-L6-v2"
        // candle Device::Cpu

    pub fn embed(&self, text: &str) -> Result<Vec<f32>>
        // tokenize → BertModel forward pass → mean-pool CLS→EOS → L2-normalize
        // output: 384-dimensional f32 vector
}
```

`candle-core`, `candle-nn`, `candle-transformers` provide the BERT forward pass.
`hf-hub` + `tokenizers` handle download and tokenization.

### `src/storage.rs`

Thin `sqlx::PgPool` wrapper. Schema init on first connection (same logic as Python `_init_schema`):
- `CREATE EXTENSION IF NOT EXISTS vector`
- `CREATE EXTENSION IF NOT EXISTS age`
- Auto-migrate `textbook_chunks.embedding` column if dim ≠ 384
- `CREATE TABLE IF NOT EXISTS` for `textbook_chunks`, `agent_checkpoints`, `learning_sessions`

Public interface:

```rust
pub async fn init_schema(pool: &PgPool) -> Result<()>

pub async fn search_textbook(
    pool: &PgPool, embedding: &[f32],
    names: &[String], k: i32,
) -> Result<Vec<TextbookChunk>>

pub async fn record_mastery(
    pool: &PgPool, thread_id: &str,
    concept: &str, score: i32, explanation: &str,
) -> Result<()>

pub async fn get_mastered_concepts(
    pool: &PgPool, thread_id: &str,
) -> Result<Vec<MasteredConcept>>

pub async fn save_checkpoint(
    pool: &PgPool, thread_id: &str,
    checkpoint_id: &str, state: &serde_json::Value,
) -> Result<()>

pub async fn load_checkpoint(
    pool: &PgPool, thread_id: &str,
) -> Result<Option<serde_json::Value>>
```

AGE graph calls (`create_concept_node`, `record_mastery` graph update) executed via raw
`sqlx::query` with `ag_catalog.cypher(...)` — same Cypher strings as Python, parameterised
where AGE allows, string-interpolated for node names (same as Python, acceptable for internal data).

Vector similarity via `pgvector`'s `<=>` operator. `pgvector` Rust crate provides the
`PgVector` type and sqlx encode/decode.

### `src/agent.rs`

Four async functions, one public entry point:

```rust
async fn retrieve(state: ChironState, connections: &TextbookConnections,
                  embedder: &Embedder) -> Result<ChironState>

async fn analyze(state: ChironState, provider: &Provider) -> Result<ChironState>

async fn probe(state: ChironState, provider: &Provider) -> Result<ChironState>

async fn evaluate(state: ChironState, provider: &Provider,
                  pool: &PgPool) -> Result<ChironState>

pub async fn process_explanation(
    concept: &str, explanation: &str,
    textbook_sources: &[String], thread_id: &str,
    provider: &Provider, connections: &TextbookConnections,
    embedder: &Embedder, pool: &PgPool,
) -> Result<ProcessResult>
```

Pipeline:

```rust
let s = retrieve(state, connections, embedder).await?;
let s = analyze(s, provider).await?;
let s = match s.stage {
    Stage::Probe    => probe(s, provider).await?,
    Stage::Evaluate => evaluate(s, provider, pool).await?,
    _               => s,
};
save_checkpoint(pool, thread_id, &uuid(), &to_value(&s)?).await?;
Ok(ProcessResult::from(s))
```

LLM prompts are identical to the Python versions (string literals in each function).
JSON gap parsing: attempt `serde_json::from_str`, fall back to `[]` on failure.

### `src/main.rs`

Startup:

```rust
// Read env: CHIRON_PROVIDER, CHIRON_MODEL, CHIRON_DATABASE_URL,
//           CHIRON_LEARNING_SCHEMA, CHIRON_TEXTBOOK_SOURCES (JSON),
//           OPENAI_API_KEY / ANTHROPIC_API_KEY / GROQ_API_KEY

let embedder   = Embedder::new()?;
let pool       = PgPoolOptions::new().connect(&db_url).await?;
init_schema(&pool).await?;
let connections = build_textbook_connections(&textbook_sources_json, &pool).await;

println!("READY provider={} db={} schema={}", provider_name, db_url, schema);

for line in stdin().lock().lines() {
    let cmd: Command = serde_json::from_str(&line?)?;
    let resp = handle_command(cmd, &ctx).await;
    println!("{}", serde_json::to_string(&resp)?);
    stdout().flush()?;
}
```

`TextbookConnections` is `HashMap<String, PgPool>` — one pool per textbook schema (may share
the same host as learning schema).

---

## Cargo.toml Dependencies

```toml
[dependencies]
tokio         = { version = "1", features = ["full"] }
serde         = { version = "1", features = ["derive"] }
serde_json    = "1"
reqwest       = { version = "0.12", features = ["json", "rustls-tls"], default-features = false }
sqlx          = { version = "0.8", features = ["postgres", "runtime-tokio-rustls", "uuid", "json"] }
pgvector      = { version = "0.4", features = ["sqlx"] }
candle-core   = { version = "0.7", features = [] }
candle-nn     = "0.7"
candle-transformers = "0.7"
hf-hub        = "0.3"
tokenizers    = { version = "0.19", default-features = false, features = ["onig"] }
uuid          = { version = "1", features = ["v4"] }
anyhow        = "1"
```

---

## Error Handling

`anyhow::Result` throughout. Errors surface to the main loop which serialises them as
`{ "success": false, "error": "<msg>" }` — same shape as Python. The Emacs frontend already
handles this format (`feynman-chiron--process-response`).

Embedder init failure is fatal (process exits, Emacs sees no READY signal → timeout).
LLM HTTP failures return `{ "success": false, "error": "..." }` to Emacs.
Storage failures in `record_mastery` are logged to stderr and swallowed (same as Python).

---

## Nix Integration

New flake input alongside the existing Python env:

```nix
chiron-rs = pkgs.rustPlatform.buildRustPackage {
  pname = "chiron-rs";
  version = "0.1.0";
  src = ./chiron-rs;
  cargoLock.lockFile = ./chiron-rs/Cargo.lock;
  buildInputs = [ pkgs.openssl pkgs.postgresql ];
  nativeBuildInputs = [ pkgs.pkg-config ];
};
```

`devShells.default` gains `chiron-rs` in `buildInputs` and drops the Python deps
(`langgraph`, `langchain-*`). `sentence-transformers`, `pytest`, `pytest-mock` also removed
from the Python env (no Python backend to test). The `chiron_storage.py` and
`chiron_agent.py` files are deleted.

---

## Elisp Changes

`feynman-chiron.el` change: one line in `feynman-chiron--start-backend` — the `program`
argument switches from `python3 chiron_agent.py` to `chiron-rs` (resolved via PATH when
installed from the nix flake, or an absolute path via `feynman-chiron-backend-program`
customvar).

No protocol changes — READY, command, response format is identical.

---

## Testing

### Rust unit tests (`cargo test`)

- `embeddings.rs`: `test_embed_returns_384_dims`, `test_embed_normalised`
- `llm.rs`: mock `reqwest` via `wiremock`; test Anthropic and Groq request/response parsing
- `storage.rs`: integration tests against a real Postgres (skipped if `TEST_DATABASE_URL` unset)
- `agent.rs`: mock `Provider` and `Embedder`; test pipeline routing (gaps → probe, no gaps → evaluate)

### Elisp ERT

Existing `tests/feynman-chiron-test.el` tests buffer-local vars — unchanged.
Add `test-backend-starts-with-rust-binary` once binary is on PATH.

---

## Migration Path

1. Build `chiron-rs` binary and verify READY signal on stdout
2. Point `feynman-chiron-backend-program` at the new binary for testing
3. Delete `chiron_agent.py`, `chiron_storage.py`, `tests/test_chiron_agent.py`, `tests/test_chiron_storage.py`
4. Update `flake.nix` to build Rust package and drop Python backend deps
5. Update `feynman-chiron.el` default `feynman-chiron-backend-program`
