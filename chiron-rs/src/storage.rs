use anyhow::{Context, Result};
use pgvector::Vector;
use serde_json::Value;
use sqlx::{PgPool, Row, postgres::PgPoolOptions};
use uuid::Uuid;

// Keep constant for runtime checks only; SQL uses literal 384.
pub const EMBEDDING_DIM: i32 = 384;

pub struct Storage {
    pub pool: PgPool,
}

impl Storage {
    pub async fn connect(db_url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(db_url)
            .await
            .context("Failed to connect to PostgreSQL")?;
        let s = Self { pool };
        s.init_schema().await?;
        Ok(s)
    }

    async fn init_schema(&self) -> Result<()> {
        let pool = &self.pool;

        // Extensions
        sqlx::query("CREATE EXTENSION IF NOT EXISTS vector")
            .execute(pool).await.context("vector extension")?;
        sqlx::query("CREATE EXTENSION IF NOT EXISTS age")
            .execute(pool).await.context("age extension")?;

        if let Err(e) = sqlx::query("LOAD 'age'").execute(pool).await {
            eprintln!("Warning: could not LOAD age: {}", e);
        }

        // Migrate embedding column if dimension changed
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT pg_catalog.format_type(a.atttypid, a.atttypmod)
             FROM pg_attribute a
             JOIN pg_class c ON a.attrelid = c.oid
             WHERE c.relname = 'textbook_chunks'
               AND a.attname = 'embedding'
               AND a.attnum > 0
               AND NOT a.attisdropped"
        )
        .fetch_optional(pool)
        .await?;

        if let Some(typ) = existing {
            if typ != format!("vector({})", EMBEDDING_DIM) {
                eprintln!(
                    "Migrating textbook_chunks.embedding {} → vector({}). Re-ingest textbooks.",
                    typ, EMBEDDING_DIM
                );
                sqlx::query("DROP INDEX IF EXISTS textbook_chunks_embedding_idx")
                    .execute(pool).await?;
                sqlx::query("ALTER TABLE textbook_chunks DROP COLUMN embedding")
                    .execute(pool).await?;
                // EMBEDDING_DIM = 384, literal used below
                sqlx::query("ALTER TABLE textbook_chunks ADD COLUMN embedding vector(384)")
                    .execute(pool).await?;
            }
        }

        // Tables — SQL dimension is a literal 384, not a runtime format
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS textbook_chunks (
                id            SERIAL PRIMARY KEY,
                textbook_name TEXT NOT NULL,
                page_number   INTEGER,
                chunk_text    TEXT NOT NULL,
                embedding     vector(384),
                metadata      JSONB,
                created_at    TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            )"
        )
        .execute(pool).await.context("textbook_chunks table")?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS textbook_chunks_embedding_idx
             ON textbook_chunks USING ivfflat (embedding vector_cosine_ops)
             WITH (lists = 100)"
        )
        .execute(pool).await.context("embedding index")?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS agent_checkpoints (
                thread_id            TEXT NOT NULL,
                checkpoint_id        TEXT NOT NULL,
                parent_checkpoint_id TEXT,
                state                JSONB NOT NULL,
                metadata             JSONB,
                created_at           TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (thread_id, checkpoint_id)
            )"
        )
        .execute(pool).await.context("agent_checkpoints table")?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS learning_sessions (
                session_id  SERIAL PRIMARY KEY,
                thread_id   TEXT NOT NULL,
                concept     TEXT NOT NULL,
                explanation TEXT NOT NULL,
                gaps        JSONB,
                score       INTEGER,
                mastered    BOOLEAN DEFAULT FALSE,
                created_at  TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            )"
        )
        .execute(pool).await.context("learning_sessions table")?;

        Ok(())
    }

    // ── RAG ──────────────────────────────────────────────────────────────────

    pub async fn search_textbook(
        &self,
        query_embedding: Vec<f32>,
        names: &[String],
        k: i64,
    ) -> Result<Vec<TextbookChunk>> {
        let embedding = Vector::from(query_embedding);
        let rows = sqlx::query(
            "SELECT textbook_name, page_number, chunk_text,
                    1.0 - (embedding <=> $1::vector) AS similarity
             FROM textbook_chunks
             WHERE textbook_name = ANY($2)
             ORDER BY embedding <=> $1::vector
             LIMIT $3"
        )
        .bind(embedding)
        .bind(names)
        .bind(k)
        .fetch_all(&self.pool)
        .await
        .context("textbook search query")?;

        Ok(rows.iter().map(|r| TextbookChunk {
            textbook_name: r.get("textbook_name"),
            page_number:   r.try_get("page_number").unwrap_or(None),
            chunk_text:    r.get("chunk_text"),
            similarity:    r.try_get("similarity").unwrap_or(0.0),
        }).collect())
    }

    // ── Knowledge graph ───────────────────────────────────────────────────────

    pub async fn record_mastery(
        &self,
        thread_id: &str,
        concept: &str,
        score: i32,
        explanation: &str,
    ) -> Result<()> {
        // AGE graph update omitted: sqlx 0.9 only accepts static SQL literals.
        // Mastery is fully captured in learning_sessions which the Emacs frontend reads.
        sqlx::query(
            "INSERT INTO learning_sessions (thread_id, concept, explanation, score, mastered)
             VALUES ($1, $2, $3, $4, TRUE)"
        )
        .bind(thread_id)
        .bind(concept)
        .bind(explanation)
        .bind(score)
        .execute(&self.pool)
        .await
        .context("learning_sessions insert")?;

        Ok(())
    }

    pub async fn get_mastered_concepts(&self, thread_id: &str) -> Result<Vec<Value>> {
        let rows = sqlx::query(
            "SELECT concept, score, explanation, created_at
             FROM learning_sessions
             WHERE thread_id = $1 AND mastered = TRUE
             ORDER BY created_at DESC"
        )
        .bind(thread_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.iter().map(|r| {
            serde_json::json!({
                "concept":     r.get::<String, _>("concept"),
                "score":       r.get::<i32, _>("score"),
                "explanation": r.get::<String, _>("explanation"),
            })
        }).collect())
    }

    // ── Checkpoints ───────────────────────────────────────────────────────────

    pub async fn save_checkpoint(&self, thread_id: &str, state: &Value) -> Result<()> {
        let checkpoint_id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO agent_checkpoints (thread_id, checkpoint_id, state)
             VALUES ($1, $2, $3)
             ON CONFLICT (thread_id, checkpoint_id) DO UPDATE SET state = EXCLUDED.state"
        )
        .bind(thread_id)
        .bind(&checkpoint_id)
        .bind(state)
        .execute(&self.pool)
        .await
        .context("save_checkpoint")?;
        Ok(())
    }
}

pub struct TextbookChunk {
    pub textbook_name: String,
    pub page_number:   Option<i32>,
    pub chunk_text:    String,
    pub similarity:    f64,
}

