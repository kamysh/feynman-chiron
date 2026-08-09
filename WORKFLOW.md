# Feynman Chiron - Multi-Project Workflow

How to work on multiple learning projects with Feynman Chiron.

## Architecture Overview

**Key design:** Each learning file is independent. You configure databases per-file using Emacs file-local variables.

```
Emacs
├── Buffer: algebra.org
│   ├── chiron-rs backend (PID 1234)
│   └── Databases: postgresql://server/learning + postgresql://server/math_textbooks
│
├── Buffer: quantum.org
│   ├── chiron-rs backend (PID 1235)
│   └── Databases: postgresql://server/learning + postgresql://server/physics_textbooks
```

Each org file specifies its own database configuration. No global config needed.

## Recommended Setup

### Database Strategy

**Recommended: Single Database with Schemas**

```
PostgreSQL Server
└── chiron (one database)
    ├── learning (schema)
    │   ├── knowledge_graph (AGE)
    │   ├── learning_sessions
    │   └── agent_checkpoints
    │
    ├── math (schema)
    │   └── textbook_chunks (pgvector)
    │
    ├── physics (schema)
    │   └── textbook_chunks (pgvector)
    │
    └── cs (schema)
        └── textbook_chunks (pgvector)
```

**Benefits:**
- Clean separation via schemas
- Single database to backup
- Easier permissions management
- Standard PostgreSQL practice

**Alternative: Single schema with metadata**

```
PostgreSQL Server
└── chiron
    └── public (default schema)
        ├── knowledge_graph (AGE)
        ├── learning_sessions
        ├── textbook_chunks (pgvector with domain column)
```

Use domain/textbook_name columns to separate instead of schemas.

### Directory Structure

```
~/learning/
├── mathematics/
│   ├── algebra.org
│   ├── topology.org
│   └── .dir-locals.el     # Shared config (optional)
│
├── physics/
│   └── quantum.org
│
└── computer-science/
    └── algorithms.org
```

## Setup Workflow

### 1. Install Feynman Chiron

```bash
# Clone (or symlink) the package into your Emacs directory
git clone git@github.com:kamysh/feynman-chiron.git ~/.emacs.d/my/feynman-chiron
cd ~/.emacs.d/my/feynman-chiron && nix build .#chiron-rs

# Add to ~/.emacs.d/init.el
(add-to-list 'load-path "~/.emacs.d/my/feynman-chiron")
(require 'feynman-chiron)
```

### 2. Setup PostgreSQL Database

On your PostgreSQL server:

```sql
-- Create database and enable extensions
CREATE DATABASE chiron;

\c chiron
CREATE EXTENSION vector;
CREATE EXTENSION age;
```

**Create schemas as needed** — `chiron-ingest` was already installed by step 1's
`nix build .#chiron-rs` (it builds both binaries), so no separate environment
setup is needed here:

```bash
mkdir ~/learning
cd ~/learning

# Create the learning schema (required)
chiron-ingest create-schema \
  "$CHIRON_DATABASE_URL" learning

# Create schemas for textbooks as you need them
chiron-ingest create-schema \
  "$CHIRON_DATABASE_URL" math

# Or create multiple at once
chiron-ingest create-schema \
  "$CHIRON_DATABASE_URL" physics cs
```

**Or create schemas manually in PostgreSQL:**

```sql
\c chiron
CREATE SCHEMA learning;
CREATE SCHEMA math;
```

### 3. Setup direnv (Optional but Recommended)

Copy the example and edit:

```bash
cp /path/to/feynman-chiron-package/.envrc.example ~/learning/.envrc
# Edit ~/learning/.envrc with your database URL
# (API keys optional if already in Emacs config)
```

Then activate:

```bash
cd ~/learning
direnv allow
```

Now the database URL is set automatically when you enter ~/learning.

**Using .pgpass for passwords:**

Instead of putting the password in the URL, you can use `~/.pgpass`:

```bash
# ~/.pgpass (chmod 600)
server:5432:chiron:user:password
# or with wildcards:
server:*:*:user:password
```

Then omit password from URL:
```bash
export CHIRON_DATABASE_URL="postgresql://user@server:5432/chiron"
```

Add to `~/.emacs.d/init.el`:

```elisp
;; Read database URL from environment if set
(setq-default feynman-chiron-database-url (getenv "CHIRON_DATABASE_URL"))
```

### 4. Ingest Textbooks

```bash
cd ~/learning  # direnv loads CHIRON_DATABASE_URL from .envrc here

# Ingest math textbooks
chiron-ingest ingest \
  "$CHIRON_DATABASE_URL" --schema math \
  ~/textbooks/dummit-foote.pdf "dummit-foote"

chiron-ingest ingest \
  "$CHIRON_DATABASE_URL" --schema math \
  ~/textbooks/munkres.pdf "munkres"

# Ingest physics textbooks
chiron-ingest ingest \
  "$CHIRON_DATABASE_URL" --schema physics \
  ~/textbooks/griffiths.pdf "griffiths"
```

**Without direnv:**

```bash
cd ~/learning

# Ingest textbooks (uses ~/.pgpass for password)
chiron-ingest ingest \
  "postgresql://user@server/chiron" --schema math \
  ~/textbooks/dummit-foote.pdf "dummit-foote"

chiron-ingest ingest \
  "postgresql://user@server/chiron" --schema math \
  ~/textbooks/munkres.pdf "munkres"
```

### 5. Create Learning Files

Example: `~/learning/mathematics/algebra.org`

```org
-*- mode: org -*-

#+TITLE: Learning Abstract Algebra

* Groups

I'm learning about groups in abstract algebra.

A group is a set G with a binary operation...

[Your explanation]


# Local Variables:
# feynman-chiron-learning-schema: "learning"
# feynman-chiron-textbook-sources: (("dummit-foote" . "math"))
# feynman-chiron-provider: anthropic
# End:
```

Example: `~/learning/physics/quantum.org`

```org
-*- mode: org -*-

#+TITLE: Learning Quantum Mechanics

* Wave Functions

I'm learning about wave functions.

[Your explanation]


# Local Variables:
# feynman-chiron-learning-schema: "learning"
# feynman-chiron-textbook-sources: (("griffiths" . "physics"))
# feynman-chiron-provider: anthropic
# End:
```

## Daily Workflow

### Starting a Learning Session

```bash
# 1. Enter learning directory (direnv sets CHIRON_DATABASE_URL, if configured)
cd ~/learning

# 2. Open Emacs with learning file
emacs mathematics/algebra.org

# 3. In Emacs
M-x feynman-chiron-start
# Write explanation
# Press C-c C-c to submit
```

No `nix develop`/environment-activation step is needed — the `chiron-rs`/
`chiron-ingest` binaries are already installed (once, via step 1's
`nix build .#chiron-rs` or Emacs's auto-install) and run directly as
subprocesses.

### Working on Multiple Subjects

Open multiple org files - each gets its own backend:

```
Emacs: 3 buffers open simultaneously
├── algebra.org    → Backend PID 1234 → chiron/learning + chiron/math
├── quantum.org    → Backend PID 1235 → chiron/learning + chiron/physics
└── algorithms.org → Backend PID 1236 → chiron/learning + chiron/cs
```

**Complete independence!** Each file configures its own databases.

### Switching Projects

Just open the file:

```
M-x find-file ~/learning/physics/quantum.org
M-x feynman-chiron-start
```

File-local variables automatically configure the correct databases.

## Configuration Patterns

### Shared Configuration with direnv and .dir-locals.el

**Option 1: direnv for base database (recommended)**

Create `~/learning/.envrc`:

```bash
export CHIRON_DATABASE_URL="postgresql://user:pass@server:5432/chiron"
```

Then in Emacs, read from environment:

`~/.emacs.d/init.el`:

```elisp
;; Read database URL from environment if set
(setq-default feynman-chiron-database-url (getenv "CHIRON_DATABASE_URL"))
```

Now all org files inherit the base database URL.

**Option 2: .dir-locals.el for shared settings**

`~/learning/.dir-locals.el`:

```elisp
((org-mode . ((feynman-chiron-database-url . "postgresql://user:pass@server:5432/chiron")
              (feynman-chiron-learning-schema . "learning"))))
```

`~/learning/mathematics/.dir-locals.el`:

```elisp
((org-mode . ((feynman-chiron-textbook-sources . (("dummit-foote" . "math")
                                                   ("munkres" . "math"))))))
```

Then individual files only override what's different:

```org
# Local Variables:
# feynman-chiron-provider: openai
# End:
```

### Textbook Source Formats

**Format 1: Just schema name (simple, most common)**

Uses the base database URL configured globally:

```org
# feynman-chiron-textbook-sources: (("dummit-foote" . "math") ("lang" . "math") ("artin" . "math"))
```

**Format 2: Full database URL with schema (for remote textbooks)**

Specify a different database server for specific textbooks:

```org
# feynman-chiron-textbook-sources: (("dummit-foote" . "math") ("remote-textbook" . ("postgresql://library-server/books" . "advanced-math")))
```

Format: `("textbook-name" . ("database-url" . "schema-name"))`

**Format 3: Mixed (flexible)**

Mix local and remote textbooks:

```org
# For group theory that appears in math AND physics, with one remote source
# feynman-chiron-textbook-sources: (("dummit-foote" . "math") ("griffiths" . "physics") ("advanced-groups" . ("postgresql://research-server/chiron" . "research")))
```

This allows you to:
- Use local textbooks from different schemas
- Use textbooks from completely different database servers
- Mix both in the same learning file

## Advanced Usage

### Knowledge Graph Queries

View all concepts you've mastered:

```sql
-- Connect to database
psql "postgresql://server/chiron"

-- Set schema
SET search_path TO learning;

-- Query knowledge graph
SELECT * FROM cypher('knowledge_graph', $$
  MATCH (s:Student)-[m:MASTERED]->(c:Concept)
  RETURN s.thread_id, c.name, m.score
  ORDER BY m.score DESC
$$) as (thread_id agtype, concept agtype, score agtype);
```

### Progress Tracking

```sql
-- View learning sessions
SET search_path TO learning;

SELECT
  thread_id,
  concept,
  mastered,
  score,
  created_at
FROM learning_sessions
ORDER BY created_at DESC
LIMIT 20;

-- Filter by subject
SELECT concept, score, created_at
FROM learning_sessions
WHERE thread_id LIKE '%algebra%'
  AND mastered = TRUE;
```

### Textbook Search

Test semantic search:

```bash
chiron-ingest search \
  "$CHIRON_DATABASE_URL" --schema math \
  "dummit-foote" \
  "subgroup normal quotient"

# With custom number of results
chiron-ingest search \
  "$CHIRON_DATABASE_URL" --schema math \
  "dummit-foote" "subgroup normal quotient" -k 5
```

## Backup Strategy

### Database Backups

```bash
# Backup entire database (recommended)
pg_dump -h server -U user chiron | gzip > backups/chiron-$(date +%Y%m%d).sql.gz

# Or backup specific schemas
pg_dump -h server -U user -n learning chiron | gzip > backups/learning-$(date +%Y%m%d).sql.gz
pg_dump -h server -U user -n math chiron | gzip > backups/math-$(date +%Y%m%d).sql.gz
```

### Org File Backups

Version control your learning:

```bash
cd ~/learning
git init
git add mathematics/ physics/ computer-science/
git commit -m "Learning progress $(date +%Y-%m-%d)"
```

### Restore

```bash
# Full database restore
dropdb chiron
createdb chiron
gunzip -c backups/chiron-20250121.sql.gz | psql chiron

# Or restore specific schema
psql chiron -c "DROP SCHEMA learning CASCADE;"
gunzip -c backups/learning-20250121.sql.gz | psql chiron
```

## Cost Management

### OpenAI Costs

- **Embeddings (one-time):** ~$0.0001 per 1000 tokens (~$1 per large textbook)
- **LLM calls:** Per session, depends on model

### Optimization

1. **Ingest once** - Embeddings stored permanently
2. **Share textbook DBs** - Multiple learning files use same textbooks
3. **Use cheaper models:**
   ```org
   # feynman-chiron-model: "gpt-3.5-turbo"
   # or
   # feynman-chiron-model: "claude-3-haiku-20240307"
   ```

## Troubleshooting

### Backend Not Starting

Check the backend buffer:

```
M-x switch-to-buffer RET *feynman-backend* RET
```

Common issues:
- `chiron-rs` binary not installed (`M-x feynman-chiron-install-backend`)
- Database connection failed (check URL, credentials)
- API key not set

### Database Connection Failed

Test connection manually:

```bash
psql "postgresql://user:pass@server/chiron"
```

Check:
- Server accessible
- Credentials correct
- Database exists
- Schemas exist
- Extensions enabled

### Extensions Not Found

```bash
# On PostgreSQL server
psql chiron -c "CREATE EXTENSION vector;"
psql chiron -c "CREATE EXTENSION age;"
```

Extensions are database-level, not schema-level. Create once per database.

If errors, check PostgreSQL version (need 14+) and extension installation.

### Backend Timeout

Increase timeout in Emacs config:

```elisp
;; In feynman-chiron.el, line ~326
(with-timeout (10 (error "Backend startup timeout"))  ; Change from 5 to 10
  ...)
```

## Environment Variables

Backend receives via environment:

- `CHIRON_DATABASE_URL` - Base PostgreSQL database URL
- `CHIRON_LEARNING_SCHEMA` - Schema name for learning state
- `CHIRON_TEXTBOOK_SOURCES` - JSON dict of textbook name → schema mappings
- `CHIRON_PROVIDER` - API provider (openai/anthropic)
- `CHIRON_MODEL` - Model name
- `OPENAI_API_KEY` or `ANTHROPIC_API_KEY` - API keys

Set in org file via file-local variables (or direnv for database URL). Backend automatically receives them.

## Summary

**Recommended workflow:**

1. **One PostgreSQL database** - `chiron` on server
2. **Schemas for organization:**
   - `learning` - knowledge graph, sessions, checkpoints
   - `math`, `physics`, `cs` - textbook embeddings
3. **Subject-based org files** - `~/learning/mathematics/algebra.org`
4. **Base database URL** - Set once via direnv or .dir-locals.el
5. **File-local variables** - Each file specifies only schema names
6. **Rust binaries** - `chiron-rs`/`chiron-ingest`, installed once via
   `M-x feynman-chiron-install-backend` (or `nix build .#chiron-rs`)

**Daily use:**

```bash
cd ~/learning
emacs mathematics/algebra.org
# M-x feynman-chiron-start
# Learn!
```

**Example org file configuration:**

```org
# Local Variables:
# feynman-chiron-learning-schema: "learning"
# feynman-chiron-textbook-sources: (("textbook-name" . "math"))
# feynman-chiron-provider: anthropic
# End:
```

The database URL is inherited from direnv or .dir-locals.el.

**Benefits:**

✅ Single database to manage
✅ Clean separation via schemas
✅ Shared knowledge graph across subjects
✅ Independent backends per buffer
✅ Standard PostgreSQL practice
✅ Easy backups