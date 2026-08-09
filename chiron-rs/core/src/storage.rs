use anyhow::{Context, Result};
use pgvector::Vector;
use serde_json::Value;
use tokio_postgres::{types::Json, Client, NoTls};
use uuid::Uuid;

const EMBEDDING_DIM: i32 = 384;

/// Create SCHEMA_NAME on the server at ADMIN_DB_URL if it doesn't already
/// exist. Uses a plain connection (no search_path scoping) since the schema
/// being created can't yet be the target of an `options=-c search_path=...`
/// connection param.
pub async fn create_schema(admin_db_url: &str, schema_name: &str) -> Result<()> {
    let (client, connection) = tokio_postgres::connect(admin_db_url, NoTls)
        .await
        .context("Failed to connect to PostgreSQL")?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("PostgreSQL connection error: {}", e);
        }
    });
    client
        .execute(&format!("CREATE SCHEMA IF NOT EXISTS \"{}\"", schema_name), &[])
        .await
        .context("CREATE SCHEMA")?;
    Ok(())
}

pub struct Storage {
    client: Client,
}

impl Storage {
    pub async fn connect(db_url: &str) -> Result<Self> {
        let (client, connection) = tokio_postgres::connect(db_url, NoTls)
            .await
            .context("Failed to connect to PostgreSQL")?;

        // Drive the connection in the background
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                eprintln!("PostgreSQL connection error: {}", e);
            }
        });

        let s = Self { client };
        s.init_schema().await?;
        Ok(s)
    }

    async fn init_schema(&self) -> Result<()> {
        // Extensions (vector + age) are pre-installed by postgres-ai image.
        // AGE knowledge graph — dynamic SQL is fine with tokio-postgres
        let _ = self
            .client
            .execute(
                "SELECT ag_catalog.create_graph('knowledge_graph')",
                &[],
            )
            .await;

        // Migrate embedding column if dimension changed
        let rows = self.client
            .query(
                "SELECT pg_catalog.format_type(a.atttypid, a.atttypmod)
                 FROM pg_attribute a
                 JOIN pg_class c ON a.attrelid = c.oid
                 WHERE c.relname = 'textbook_chunks'
                   AND a.attname = 'embedding'
                   AND a.attnum > 0
                   AND NOT a.attisdropped",
                &[],
            )
            .await?;

        if let Some(row) = rows.first() {
            let typ: String = row.get(0);
            if typ != format!("vector({})", EMBEDDING_DIM) {
                eprintln!(
                    "Migrating textbook_chunks.embedding {} → vector({}). Re-ingest textbooks.",
                    typ, EMBEDDING_DIM
                );
                self.client
                    .execute("DROP INDEX IF EXISTS textbook_chunks_embedding_idx", &[])
                    .await?;
                self.client
                    .execute("ALTER TABLE textbook_chunks DROP COLUMN embedding", &[])
                    .await?;
                self.client
                    .execute(
                        &format!(
                            "ALTER TABLE textbook_chunks ADD COLUMN embedding vector({})",
                            EMBEDDING_DIM
                        ),
                        &[],
                    )
                    .await?;
            }
        }

        self.client
            .execute(
                &format!(
                    "CREATE TABLE IF NOT EXISTS textbook_chunks (
                        id            SERIAL PRIMARY KEY,
                        textbook_name TEXT NOT NULL,
                        page_number   INTEGER,
                        chunk_text    TEXT NOT NULL,
                        embedding     vector({}),
                        metadata      JSONB,
                        created_at    TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                    )",
                    EMBEDDING_DIM
                ),
                &[],
            )
            .await
            .context("textbook_chunks table")?;

        self.client
            .execute(
                "CREATE INDEX IF NOT EXISTS textbook_chunks_embedding_idx
                 ON textbook_chunks USING ivfflat (embedding vector_cosine_ops)
                 WITH (lists = 100)",
                &[],
            )
            .await
            .context("embedding index")?;

        self.client
            .execute(
                "CREATE TABLE IF NOT EXISTS agent_checkpoints (
                    thread_id            TEXT NOT NULL,
                    checkpoint_id        TEXT NOT NULL,
                    parent_checkpoint_id TEXT,
                    state                JSONB NOT NULL,
                    metadata             JSONB,
                    created_at           TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                    PRIMARY KEY (thread_id, checkpoint_id)
                )",
                &[],
            )
            .await
            .context("agent_checkpoints table")?;

        self.client
            .execute(
                "CREATE TABLE IF NOT EXISTS learning_sessions (
                    session_id  SERIAL PRIMARY KEY,
                    thread_id   TEXT NOT NULL,
                    concept     TEXT NOT NULL,
                    explanation TEXT NOT NULL,
                    gaps        JSONB,
                    score       INTEGER,
                    mastered    BOOLEAN DEFAULT FALSE,
                    created_at  TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                )",
                &[],
            )
            .await
            .context("learning_sessions table")?;

        Ok(())
    }

    // ── RAG ──────────────────────────────────────────────────────────────────

    pub async fn insert_chunk(
        &self,
        textbook_name: &str,
        page_number: Option<i32>,
        chunk_text: &str,
        embedding: Vec<f32>,
        metadata: &Value,
    ) -> Result<()> {
        let embedding = Vector::from(embedding);
        self.client
            .execute(
                "INSERT INTO textbook_chunks
                    (textbook_name, page_number, chunk_text, embedding, metadata)
                 VALUES ($1, $2, $3, $4, $5)",
                &[&textbook_name, &page_number, &chunk_text, &embedding, &Json(metadata)],
            )
            .await
            .context("insert_chunk")?;
        Ok(())
    }

    pub async fn search_textbook(
        &self,
        query_embedding: Vec<f32>,
        names: &[String],
        k: i64,
    ) -> Result<Vec<TextbookChunk>> {
        let embedding = Vector::from(query_embedding);
        // tokio-postgres needs a slice of &dyn ToSql + Sync references.
        let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let rows = self.client
            .query(
                "SELECT textbook_name, page_number, chunk_text,
                        1.0 - (embedding <=> $1) AS similarity
                 FROM textbook_chunks
                 WHERE textbook_name = ANY($2)
                 ORDER BY embedding <=> $1
                 LIMIT $3",
                &[&embedding, &name_refs.as_slice(), &k],
            )
            .await
            .context("textbook search query")?;

        Ok(rows.iter().map(|r| TextbookChunk {
            textbook_name: r.get("textbook_name"),
            page_number:   r.get("page_number"),
            chunk_text:    r.get("chunk_text"),
            similarity:    r.get("similarity"),
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
        // AGE Cypher — dynamic SQL, tokio-postgres has no static-only restriction.
        let cypher = format!(
            "MERGE (s:Student {{thread_id: '{}'}})
             MERGE (c:Concept  {{name: '{}'}})
             MERGE (s)-[m:MASTERED]->(c)
             SET m.score = {}, m.date = timestamp()
             RETURN m",
            escape_cypher(thread_id),
            escape_cypher(concept),
            score,
        );
        let age_sql = format!(
            "SELECT * FROM ag_catalog.cypher('knowledge_graph', $${}$$) AS (m agtype)",
            cypher
        );
        if let Err(e) = self.client.execute(age_sql.as_str(), &[]).await {
            eprintln!("Warning: AGE record_mastery failed: {}", e);
        }

        self.client
            .execute(
                "INSERT INTO learning_sessions
                    (thread_id, concept, explanation, score, mastered)
                 VALUES ($1, $2, $3, $4, TRUE)",
                &[&thread_id, &concept, &explanation, &score],
            )
            .await
            .context("learning_sessions insert")?;

        Ok(())
    }

    pub async fn get_mastered_concepts(&self, thread_id: &str) -> Result<Vec<Value>> {
        let rows = self.client
            .query(
                "SELECT concept, score, explanation
                 FROM learning_sessions
                 WHERE thread_id = $1 AND mastered = TRUE
                 ORDER BY created_at DESC",
                &[&thread_id],
            )
            .await?;

        Ok(rows.iter().map(|r| {
            serde_json::json!({
                "concept":     r.get::<_, String>("concept"),
                "score":       r.get::<_, i32>("score"),
                "explanation": r.get::<_, String>("explanation"),
            })
        }).collect())
    }

    // ── Checkpoints ───────────────────────────────────────────────────────────

    pub async fn save_checkpoint(&self, thread_id: &str, state: &Value) -> Result<()> {
        let checkpoint_id = Uuid::new_v4().to_string();
        self.client
            .execute(
                "INSERT INTO agent_checkpoints (thread_id, checkpoint_id, state)
                 VALUES ($1, $2, $3)
                 ON CONFLICT (thread_id, checkpoint_id) DO UPDATE SET state = EXCLUDED.state",
                &[&thread_id, &checkpoint_id, &Json(state)],
            )
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

fn escape_cypher(s: &str) -> String {
    s.replace('\'', "\\'")
}
