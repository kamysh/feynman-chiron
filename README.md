# Feynman Chiron

Learn using the Feynman Technique with AI assistance. Explain concepts, get Socratic questions, track mastery in a knowledge graph.

## Requirements

- PostgreSQL with pgvector and Apache AGE extensions
- Python environment (use flake.nix)
- Emacs with org-mode
- API key: Anthropic or OpenAI

## Installation

### 1. Build the agent backend

```bash
cd feynman-chiron
nix build .#chiron-rs   # static musl binary at result/bin/chiron-rs
```

Or for local development (dynamic build):

```bash
nix develop
cd chiron-rs && cargo build --release
```

### 2. Emacs Configuration

Add to `~/.emacs.d/init.el`:

```elisp
(add-to-list 'load-path "/path/to/feynman-chiron")
(require 'feynman-chiron)

(setq feynman-chiron-anthropic-key (getenv "ANTHROPIC_API_KEY"))
(setq feynman-chiron-openai-key (getenv "OPENAI_API_KEY"))
(setq feynman-chiron-backend-program
      "/path/to/feynman-chiron/chiron-rs/target/release/chiron-rs")
```

`feynman-chiron-backend-program` may be left unset (`nil`) if `chiron-rs` is on `PATH`
(e.g. installed via the nix flake) — it's then auto-detected, falling back to
`chiron-rs/target/release/chiron-rs` inside the package directory.

### 3. Database Setup

**On PostgreSQL server:**

```sql
CREATE DATABASE chiron;

\c chiron
CREATE EXTENSION vector;
CREATE EXTENSION age;
```

**Create schemas (as needed):**

```bash
# Create schemas when you need them
python3 chiron_storage.py create-schema "postgresql://host/chiron" learning math
```

Or manually:
```sql
CREATE SCHEMA learning;
CREATE SCHEMA math;
```

### 4. Ingest Textbooks (Optional)

```bash
export OPENAI_API_KEY="sk-..."
python3 chiron_storage.py ingest \
  "postgresql://host/chiron" --schema math \
  ~/textbooks/book.pdf \
  "book-name"
```

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

## Architecture

- **feynman-chiron.el**: Emacs interface
- **chiron-rs/**: Rust agent backend (retrieve → analyze → probe/evaluate pipeline),
  speaks the same stdin/stdout JSON protocol the Emacs frontend expects. Embeds a
  MiniLM sentence-transformer via `candle` for retrieval; talks to Anthropic natively
  or any OpenAI-compatible endpoint. See `docs/superpowers/specs/2026-06-01-chiron-rs-design.md`
  for the design.
- **chiron_storage.py**: standalone Python CLI for offline textbook ingestion
  (PDF → chunks → pgvector embeddings) and schema creation — not part of the live
  agent loop.
- Per-buffer backend: each org file gets its own `chiron-rs` process
- Per-file configuration: databases specified in file-local variables

## Files

- `feynman-chiron.el` - Emacs interface
- `chiron-rs/` - Rust agent backend (the live runtime)
- `chiron_storage.py` - Offline ingestion CLI (create-schema / ingest)
- `flake.nix` - Nix development environment + static binary build
- `.envrc.example` - Example direnv configuration
- `.dir-locals.el.example` - Example Emacs directory-local settings
- `WORKFLOW.md` - Comprehensive multi-project workflow guide

## License

Apache License 2.0 — see [LICENSE](LICENSE).
Contributions are subject to the [Contributor License Agreement](CLA.md).