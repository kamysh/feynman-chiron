# Feynman Chiron

Learn using the Feynman Technique with AI assistance. Explain concepts, get Socratic questions, track mastery in a knowledge graph.

## Requirements

- PostgreSQL with pgvector and Apache AGE extensions
- Emacs with org-mode
- API key: Anthropic or OpenAI (only for the agent — textbook ingestion embeds locally, no key needed)
- Nix (for building the Rust binaries), unless installing prebuilt release binaries

## Installation

Feynman Chiron isn't on MELPA — install directly from the GitHub repo, then build
the `chiron-rs` backend binary.

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
Both are built from the same `chiron-rs/` Nix workspace.

**Automatic (recommended):** just run `M-x feynman-chiron-start` (or
`M-x feynman-chiron-ingest-textbook`). If the binary it needs isn't found, Emacs
asks to install it, then does so itself — no shell commands needed:

- If you installed via a git clone/submodule that includes the `chiron-rs/`
  source tree and you have [Nix](https://nixos.org/) on `PATH`, it runs
  `nix build .#chiron-rs` (produces both binaries) and copies the result in.
- Otherwise it downloads the prebuilt binary for your platform from
  [Releases](https://github.com/kamysh/feynman-chiron/releases).

Either way the binaries land in `feynman-chiron-backend-install-dir` (default
`~/.emacs.d/bin/`) and are auto-detected from then on. You can also trigger
this directly: `M-x feynman-chiron-install-backend` (installs both).

Neither binary has any use outside this package, so they're deliberately kept
out of your shell `PATH` — these aren't general-purpose CLI tools to install
system-wide, they only exist as subprocesses the Emacs package talks to.

**Manual, if you'd rather not let Emacs run `nix build`/download things:** drop
the binaries at exactly `feynman-chiron-backend-install-dir`'s default location
(`~/.emacs.d/bin/chiron-rs`, `~/.emacs.d/bin/chiron-ingest`) and they're
auto-detected — no `PATH` or `feynman-chiron-backend-program` setup needed:

```bash
mkdir -p ~/.emacs.d/bin

# Prebuilt release binaries (substitute -linux-arm64 / -darwin-arm64 for
# other platforms):
for bin in chiron-rs chiron-ingest; do
  curl -fL -o ~/.emacs.d/bin/$bin \
    https://github.com/kamysh/feynman-chiron/releases/latest/download/$bin-linux-amd64
  chmod +x ~/.emacs.d/bin/$bin
done

# Or build from source (requires Nix) — one build produces both binaries:
cd feynman-chiron
nix build .#chiron-rs   # static binaries at result/bin/{chiron-rs,chiron-ingest}
install -m 755 result/bin/chiron-rs ~/.emacs.d/bin/chiron-rs
install -m 755 result/bin/chiron-ingest ~/.emacs.d/bin/chiron-ingest
```

For local development (dynamic build, faster iteration): `nix develop`, then
`cd chiron-rs && cargo build --release` — picked up automatically from
`chiron-rs/target/release/{chiron-rs,chiron-ingest}` next to the package source.

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

**Create schemas (as needed)** — via Emacs, `M-x feynman-chiron-create-schema`, or the CLI directly:

```bash
chiron-ingest create-schema "postgresql://host/chiron" learning math
```

Or manually:
```sql
CREATE SCHEMA learning;
CREATE SCHEMA math;
```

### 5. Ingest Textbooks (Optional)

`M-x feynman-chiron-ingest-textbook` (prompts for PDF path, textbook name, schema),
or the CLI directly:

```bash
chiron-ingest ingest --schema math "postgresql://host/chiron" ~/textbooks/book.pdf "book-name"
```

No API key needed — embeddings are generated locally (same MiniLM model
`chiron-rs` uses at query time), so ingestion works fully offline aside from
the database connection. Test retrieval with `M-x feynman-chiron-search-textbook`
or `chiron-ingest search --schema math "postgresql://host/chiron" "book-name" "your query"`.

## Configuration

Global settings (`M-x customize-group RET feynman-chiron RET`, or `setq`):

| Variable | Default | Purpose |
|---|---|---|
| `feynman-chiron-default-provider` | `anthropic` | `openai` or `anthropic`, used when a buffer doesn't override it |
| `feynman-chiron-openai-model` | `"gpt-4"` | Default OpenAI model name |
| `feynman-chiron-anthropic-model` | `"claude-sonnet-4-6"` | Default Anthropic model name |
| `feynman-chiron-openai-key` | `nil` | OpenAI API key: string, zero-arg function (e.g. `password-store-get`), or `nil` to fall back to `auth-source` (host `api.openai.com`) |
| `feynman-chiron-anthropic-key` | `nil` | Anthropic API key: string, zero-arg function, or `nil` to fall back to `auth-source` (host `api.anthropic.com`) |
| `feynman-chiron-backend-program` | `nil` | Path to the `chiron-rs` binary; `nil` auto-detects via `PATH`, then `chiron-rs/target/release/chiron-rs` next to the package, then `feynman-chiron-backend-install-dir`, then offers to install it |
| `feynman-chiron-backend-install-dir` | `~/.emacs.d/bin/` | Where `feynman-chiron-install-backend` installs the binary it builds or downloads (deliberately outside your shell `PATH` — the binary has no standalone use) |
| `feynman-chiron-endpoint-url` | `nil` | Base URL for an OpenAI-compatible endpoint (Groq, Mistral, Ollama, …) when provider is `openai`; defaults to `https://api.openai.com` |
| `feynman-chiron-backend-buffer` | `" *feynman-backend*"` | Name of the buffer holding the backend process's stderr/stdout |

Per-buffer settings, set via file-local variables (see the `algebra.org` example
below) or `.dir-locals.el` — these have no global default and configure what a
given learning session talks to:

| Variable | Purpose |
|---|---|
| `feynman-chiron-database-url` | PostgreSQL connection string, e.g. `"postgresql://host/chiron"` |
| `feynman-chiron-learning-schema` | Schema name for this session's progress/mastery tracking |
| `feynman-chiron-textbook-sources` | Alist of `(name . schema)` or `(name . (database-url . schema))` for RAG retrieval — see the "Advanced" example below |
| `feynman-chiron-provider` | Per-buffer override of `feynman-chiron-default-provider` |
| `feynman-chiron-model` | Per-buffer override of the provider's default model |

`feynman-chiron-database-url` is commonly set once, globally, via
`setq-default` (see the direnv tip below) rather than repeated per file.

## Usage

Create a learning file `algebra.org`:

```org
-*- mode: org -*-

#+TITLE: Learning Abstract Algebra

* Groups

I'm learning about groups in abstract algebra.

[Write your explanation here]


# Local Variables:
# feynman-chiron-database-url: "postgresql://host/chiron"
# feynman-chiron-learning-schema: "learning"
# feynman-chiron-textbook-sources: (("book-name" . "math"))
# End:
```

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
- `M-x feynman-chiron-start` - Start session
- `C-c C-c` - Submit explanation
- `C-c C-p` - Show progress
- `M-x feynman-chiron-create-schema` - Create a PostgreSQL schema
- `M-x feynman-chiron-ingest-textbook` - Ingest a PDF textbook
- `M-x feynman-chiron-search-textbook` - Test retrieval against an ingested textbook
- `M-x feynman-chiron-install-backend` - (Re)install the chiron-rs/chiron-ingest binaries

## Architecture

Everything is Rust — no Python anywhere in this package.

- **feynman-chiron.el**: Emacs interface
- **chiron-rs/agent/** (binary `chiron-rs`): the agent backend (retrieve → analyze
  → probe/evaluate pipeline), speaks the same stdin/stdout JSON protocol the Emacs
  frontend expects. Talks to Anthropic natively or any OpenAI-compatible endpoint.
  See `docs/superpowers/specs/2026-06-01-chiron-rs-design.md` for the design.
- **chiron-rs/ingest/** (binary `chiron-ingest`): offline textbook ingestion CLI
  (PDF → chunks → pgvector embeddings) and schema creation — not part of the live
  agent loop, invoked as its own subprocess.
- **chiron-rs/core/** (library `chiron-core`): shared code between the two binaries
  — the MiniLM embedder (`candle`) and PostgreSQL/pgvector storage layer.
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