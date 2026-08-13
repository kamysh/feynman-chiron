# Feynman Chiron

Learn using the Feynman Technique with AI assistance. Explain concepts, get Socratic questions, track mastery in a knowledge graph.

`M-x feynman-chiron-menu` is the entry point — a `transient` menu listing
every command this package has, so you don't need to remember individual
`M-x` names. Schema and textbook-name prompts complete against what's
actually in the database (via `chiron-ingest list-schemas`/`list-textbooks`),
not blind free text.

## Requirements

- PostgreSQL with pgvector and Apache AGE extensions
- Emacs with org-mode
- `transient` (ships with Emacs 28+ / commonly already installed via magit)
- API key: Anthropic or OpenAI (only for the agent — textbook ingestion embeds locally, no key needed)
- Network access to github.com the first time the package loads (to download
  the prebuilt `chiron-rs`/`chiron-ingest` binaries — automatic, see below)
  and to huggingface.co the first time either binary actually runs (see
  [Embedding model](#embedding-model) below) — neither needed after that

Nix is **not** required. Building from source is a manual developer workflow
only (see "For local development" below) — most people never need it.

## Installation

Feynman Chiron isn't on MELPA — install directly from the GitHub repo. The
backend binaries install themselves automatically the first time Emacs is
idle after loading the package — nothing else to run.

### 1. Get the Emacs package

Pick whichever matches your setup:

**`use-package` + `:vc` (Emacs 30+, no extra package manager needed):**

```elisp
(use-package feynman-chiron
  :vc (:url "https://github.com/kamysh/feynman-chiron" :branch "main")
  :custom
  (feynman-chiron-database-url "postgresql://host/chiron"))
```

**`package-vc-install` (Emacs 29+):**

```elisp
(package-vc-install "https://github.com/kamysh/feynman-chiron")
(require 'feynman-chiron)
```

**straight.el:**

```elisp
(straight-use-package
 '(feynman-chiron :type git :host github :repo "kamysh/feynman-chiron"))
```

**elpaca:**

```elisp
(use-package feynman-chiron
  :elpaca (:host github :repo "kamysh/feynman-chiron"))
```

**Manual clone (or as a submodule of your `.emacs.d`):**

```bash
git clone https://github.com/kamysh/feynman-chiron.git ~/.emacs.d/site-lisp/feynman-chiron
```

```elisp
(add-to-list 'load-path "~/.emacs.d/site-lisp/feynman-chiron")
(require 'feynman-chiron)
```

### 2. Get the Rust binaries

The Emacs package is only the frontend — it drives two separate Rust binaries as
subprocesses: `chiron-rs` (the agent) and `chiron-ingest` (textbook ingestion).

**This is automatic — there is nothing to run.** Shortly after the package
loads (once Emacs is idle), it downloads the prebuilt release binary for your
platform from [Releases](https://github.com/kamysh/feynman-chiron/releases)
for anything missing, into `feynman-chiron-backend-install-dir` (default
`bin/` inside this package's own checkout, e.g.
`~/.emacs.d/elpa/feynman-chiron/bin/` — no separate `.gitignore` entry
needed, and it's cleaned up automatically if the checkout is ever deleted
and recloned), with no prompt. The same thing happens if a binary there
is ever found to be a different version than the currently-loaded package —
you never need to remember to update it yourself. `M-x feynman-chiron-install-backend`
exists if you want to force a (re)install right now instead of waiting for
the next idle moment.

Neither binary has any use outside this package, so they're deliberately kept
out of your shell `PATH` — these aren't general-purpose CLI tools to install
system-wide, they only exist as subprocesses the Emacs package talks to.

**Manual, if you'd rather install them yourself:** drop the binaries at
exactly `feynman-chiron-backend-install-dir`'s default location
(`bin/chiron-rs`, `bin/chiron-ingest` inside the package checkout) and
they're auto-detected — no `PATH` or `feynman-chiron-backend-program`
setup needed:

```bash
cd ~/.emacs.d/elpa/feynman-chiron   # or wherever this package is checked out
mkdir -p bin
# Substitute -linux-arm64 / -darwin-arm64 for other platforms:
for bin in chiron-rs chiron-ingest; do
  curl -fL -o bin/$bin \
    https://github.com/kamysh/feynman-chiron/releases/latest/download/$bin-linux-amd64
  chmod +x bin/$bin
done
```

**For local development** (building from source instead of downloading —
requires [Nix](https://nixos.org/), never done automatically): `nix develop`,
then `cd chiron-rs && cargo build --release` — picked up automatically from
`chiron-rs/target/release/{chiron-rs,chiron-ingest}` next to the package
source, ahead of any auto-installed binary. A fully static release build is
`nix build .#chiron-rs` (produces both binaries at `result/bin/`).

### 3. Configure

At minimum, set a database URL and (if the binary isn't on `PATH`) the backend
program path:

```elisp
(setq feynman-chiron-database-url "postgresql://host/chiron")
(setq feynman-chiron-backend-program "~/.emacs.d/bin/chiron-rs")  ; omit if auto-detected
```

API keys: set `feynman-chiron-anthropic-key` / `feynman-chiron-openai-key`
directly, or leave them `nil` to fall back to `auth-source` (e.g. `~/.authinfo.gpg`
with `machine api.anthropic.com login apikey password sk-ant-...`). See
[Configuration](#configuration) below for the full variable reference.

### 4. Database Setup

**On PostgreSQL server:**

```sql
CREATE DATABASE chiron;

\c chiron
CREATE EXTENSION vector;
CREATE EXTENSION age;
```

**Create schemas (as needed)** with `M-x feynman-chiron-create-schema`
(prompts for the database URL and schema name; runs `chiron-ingest` under the
hood — that binary is a subprocess of this package, not a tool you invoke
yourself). Or manually:

```sql
CREATE SCHEMA learning;
CREATE SCHEMA math;
```

### 5. Ingest Textbooks (Optional)

`M-x feynman-chiron-ingest-textbook` (prompts for PDF path, textbook name, schema,
and embedding model).

No API key needed — embeddings are generated locally. See
[Embedding model](#embedding-model) below: the *first* run of either binary
needs network access to fetch the model, after which ingestion needs only
the database connection.

## Embedding model

`chiron-rs` and `chiron-ingest` embed text locally via `candle` — no API
calls, no API key — using any BERT-family sentence-embedding model on
Hugging Face Hub. The **default** is
[`sentence-transformers/all-MiniLM-L6-v2`](https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2)
(384-dim), but the model is a **per-project setting**, not a fixed constant:
set `feynman-chiron-embedding-model` (buffer-local, file-local-variable) or
pass a model explicitly to `M-x feynman-chiron-ingest-textbook`. A schema's
embedding model is fixed on its *first* ingest and persisted server-side
(`embedding_config` table) — later ingests into the same schema with a
*different* model are rejected with a clear error rather than silently
migrated, since a migration would mean discarding every previously-embedded
chunk's vector. To switch a project's model, ingest into a new (or emptied)
schema instead. `M-x feynman-chiron-search-textbook` always reads back
whichever model a schema actually used, so query and stored vectors can
never mismatch — there's no model flag for search.

Loading a model (`Embedder::new`, `chiron-rs/core/src/embeddings.rs`) fetches
`config.json`, `tokenizer.json`, and `model.safetensors` from Hugging Face
Hub through the `hf-hub` crate the first time that specific model is used,
caching them under `hf-hub`'s default location (`~/.cache/huggingface/hub`
on Linux/macOS, `HF_HOME` if set). That first load per model needs network
access to huggingface.co; every run after that is served from the local
cache with no network needed. There is currently no way to pre-seed or
relocate this cache from Emacs — it's whatever `hf-hub` does by default.

## Configuration

Global settings (`M-x customize-group RET feynman-chiron RET`, or `setq`):

| Variable | Default | Purpose |
|---|---|---|
| `feynman-chiron-default-provider` | `anthropic` | `openai` or `anthropic`, used when a buffer doesn't override it |
| `feynman-chiron-openai-model` | `"gpt-4"` | Default OpenAI model name |
| `feynman-chiron-anthropic-model` | `"claude-sonnet-4-6"` | Default Anthropic model name |
| `feynman-chiron-openai-key` | `nil` | OpenAI API key: string, zero-arg function (e.g. `password-store-get`), or `nil` to fall back to `auth-source` (host `api.openai.com`) |
| `feynman-chiron-anthropic-key` | `nil` | Anthropic API key: string, zero-arg function, or `nil` to fall back to `auth-source` (host `api.anthropic.com`) |
| `feynman-chiron-backend-program` | `nil` | Path to the `chiron-rs` binary; `nil` auto-detects via `PATH`, then `chiron-rs/target/release/chiron-rs` next to the package, then `feynman-chiron-backend-install-dir`, then downloads it automatically |
| `feynman-chiron-backend-install-dir` | `bin/` inside the package checkout | Where the prebuilt binary is downloaded to (deliberately outside your shell `PATH` — the binary has no standalone use) |
| `feynman-chiron-endpoint-url` | `nil` | Base URL override for whichever provider is active — an OpenAI-compatible endpoint (Groq, Mistral, Ollama, …) when provider is `openai`, or a local Anthropic-API-compatible proxy (e.g. Meridian) when provider is `anthropic`; `nil` uses each provider's own real API endpoint |
| `feynman-chiron-backend-buffer` | `" *feynman-backend*"` | Name of the buffer holding the backend process's stderr/stdout |
| `feynman-chiron-response-timeout` | `60` | Seconds to wait for a response from the agent. A real LLM call — especially through a CLI-proxying endpoint that spawns a subprocess per request — can take well over 10s |

Per-buffer settings, set via file-local variables (see the `algebra.org` example
below) or `.dir-locals.el` — these have no global default and configure what a
given learning session talks to:

| Variable | Purpose |
|---|---|
| `feynman-chiron-database-url` | PostgreSQL connection string, e.g. `"postgresql://host/chiron"` |
| `feynman-chiron-learning-schema` | Schema name for this session's progress/mastery tracking |
| `feynman-chiron-textbook-sources` | Alist of `(name . schema)` or `(name . (database-url . schema))` for RAG retrieval — see the "Advanced" example below |
| `feynman-chiron-embedding-model` | Hugging Face Hub model id for `feynman-chiron-ingest-textbook` on this project (default when nil: `sentence-transformers/all-MiniLM-L6-v2`). Only takes effect on a schema's first ingest — see [Embedding model](#embedding-model) above |
| `feynman-chiron-provider` | Per-buffer override of `feynman-chiron-default-provider` |
| `feynman-chiron-model` | Per-buffer override of the provider's default model |

`feynman-chiron-database-url` is commonly set once, globally, via
`setq-default` (see the direnv tip below) rather than repeated per file.

## Usage

`feynman-chiron-mode` is a minor mode on top of `org-mode` — there is no
separate session buffer, and nothing is ever locked read-only. One org
heading is one concept; its subtree is your explanation, written and
revised exactly like any other org text.

Create a learning file `algebra.org`:

```org
-*- mode: org -*-

#+TITLE: Learning Abstract Algebra

* Groups

I'm learning about groups in abstract algebra — a group is a set with
a binary operation satisfying closure, associativity, identity, and
inverses.


# Local Variables:
# feynman-chiron-database-url: "postgresql://host/chiron"
# feynman-chiron-learning-schema: "learning"
# feynman-chiron-textbook-sources: (("book-name" . "math"))
# End:
```

Put point under the `* Groups` heading and press `C-c C-c`
(`feynman-chiron-submit`). This sends the heading text as the concept
and the current subtree text as the conversation so far, and appends
Chiron's response as a `Chiron: ` turn, followed by a `You: ` prompt
for your reply:

```org
* Groups
:PROPERTIES:
:ID:       3f9e2c1a-...
:END:

I'm learning about groups in abstract algebra — a group is a set with
a binary operation satisfying closure, associativity, identity, and
inverses.

Chiron: Good start. One gap: you haven't said why the operation needs
to be well-defined on the set (closure) rather than just "some
operation" — can you give an example of a set + operation that fails
closure?

You:
```

Write your reply after `You: ` (that's where point lands after each
response) and press `C-c C-c` again to continue. Each submit sends the
*whole* transcript, never stripped or truncated — Chiron sees its own
prior turns as context, so it won't repeat a question you already
answered. The heading's `:ID:` property (created automatically on
first submit, via `org-id`) is the stable identifier your progress is
tracked under — renaming or moving the heading doesn't lose it.

**Tip:** Set `feynman-chiron-database-url` once in `~/.emacs.d/init.el` or via direnv:

```bash
# ~/learning/.envrc
export CHIRON_DATABASE_URL="postgresql://host/chiron"
```

Then in Emacs config:

```elisp
(setq-default feynman-chiron-database-url (getenv "CHIRON_DATABASE_URL"))
```

Now each org file only needs to specify the schema and textbook sources.

**Advanced:** If a textbook is on a different server, specify both database and schema:

```org
# feynman-chiron-textbook-sources: (("local-book" . "math") ("remote-book" . ("postgresql://other-server/chiron" . "physics")))
```

In Emacs:
- `M-x feynman-chiron-menu` - Command menu (everything below, one entry point)
- `M-x feynman-chiron-start` - Enable `feynman-chiron-mode` in the current org buffer and ready the backend
- `C-c C-c` - Submit the org subtree at point (`feynman-chiron-submit`)
- `C-c C-m` - Command menu (with `feynman-chiron-mode` enabled)
- `M-x feynman-chiron-create-schema` - Create a PostgreSQL schema (schema-name prompt completes against what already exists)
- `M-x feynman-chiron-ingest-textbook` - Ingest a PDF textbook (schema prompt completes against existing schemas)
- `M-x feynman-chiron-search-textbook` - Test retrieval against an ingested textbook (schema and textbook-name prompts complete against what's actually in the database)
- `M-x feynman-chiron-install-backend` - (Re)install the chiron-rs/chiron-ingest binaries

## Architecture

Everything is Rust — no Python anywhere in this package.

- **feynman-chiron.el**: Emacs interface
- **chiron-rs/agent/** (binary `chiron-rs`): the agent backend (retrieve → analyze
  → probe/evaluate pipeline), speaks the same stdin/stdout JSON protocol the Emacs
  frontend expects. Talks to Anthropic natively or any OpenAI-compatible endpoint.
  See `docs/superpowers/specs/2026-06-01-chiron-rs-design.md` for the design.
- **chiron-rs/ingest/** (binary `chiron-ingest`): offline textbook ingestion
  (PDF → chunks → pgvector embeddings) and schema creation. Not part of the live
  agent loop — invoked only as a subprocess of the `M-x feynman-chiron-create-schema`
  / `-ingest-textbook` / `-search-textbook` Emacs commands, not run by hand.
- **chiron-rs/core/** (library `chiron-core`): shared code between the two binaries
  — the configurable-model embedder (`candle`, see [Embedding model](#embedding-model))
  and PostgreSQL/pgvector storage layer.
- Per-buffer backend: each org file gets its own `chiron-rs` process
- Per-file configuration: databases specified in file-local variables

## Files

- `feynman-chiron.el` - Emacs interface
- `chiron-rs/` - Rust workspace: `agent/` (chiron-rs), `ingest/` (chiron-ingest),
  `core/` (shared library)
- `flake.nix` - Nix development environment + static binary build (both binaries)
- `.envrc.example` - Example direnv configuration
- `.dir-locals.el.example` - Example Emacs directory-local settings
- `WORKFLOW.md` - Comprehensive multi-project workflow guide

## License

Apache License 2.0 — see [LICENSE](LICENSE).
Contributions are subject to the [Contributor License Agreement](CLA.md).