# Feynman Chiron

Learn using the Feynman Technique with AI assistance. Explain concepts, get Socratic questions, track mastery in a knowledge graph.

## Requirements

- PostgreSQL with pgvector and Apache AGE extensions
- Python environment (use flake.nix)
- Emacs with org-mode
- API key: Anthropic or OpenAI

## Installation

### 1. Python Environment

```bash
cd feynman-chiron-package
nix develop
```

### 2. Emacs Configuration

Add to `~/.emacs.d/init.el`:

```elisp
(add-to-list 'load-path "/path/to/feynman-chiron-package")
(require 'feynman-chiron)

(setq feynman-chiron-anthropic-key (getenv "ANTHROPIC_API_KEY"))
(setq feynman-chiron-openai-key (getenv "OPENAI_API_KEY"))
(setq feynman-chiron-backend-script
      "/path/to/feynman-chiron-package/chiron_agent.py")
```

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
- **chiron_agent.py**: LangGraph workflow (retrieve → analyze → probe/evaluate)
- **chiron_storage.py**: PostgreSQL storage (pgvector for RAG, Apache AGE for knowledge graph)
- Per-buffer backend: each org file gets its own Python process
- Per-file configuration: databases specified in file-local variables

## Files

- `feynman-chiron.el` - Emacs interface
- `chiron_agent.py` - Backend agent
- `chiron_storage.py` - Storage + CLI for ingestion
- `flake.nix` - Nix development environment
- `.envrc.example` - Example direnv configuration
- `.dir-locals.el.example` - Example Emacs directory-local settings
- `WORKFLOW.md` - Comprehensive multi-project workflow guide