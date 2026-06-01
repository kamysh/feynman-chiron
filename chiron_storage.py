#!/usr/bin/env python3
"""
PostgreSQL-based storage for Feynman Chiron.

Uses:
- Apache AGE for graph storage (knowledge graph, learning state)
- pgvector for RAG (semantic search)
- Standard tables for checkpoints

Single database for everything.
"""

import sys
import psycopg2
from psycopg2.extras import Json, RealDictCursor
import json
import os
from typing import List, Dict, Optional
import numpy as np

EMBEDDING_DIM = 384  # all-MiniLM-L6-v2 output dimension

try:
    from langchain_community.embeddings import HuggingFaceEmbeddings
    HAVE_EMBEDDINGS = True
except ImportError:
    HAVE_EMBEDDINGS = False
    print("WARNING: langchain_community not found, embeddings disabled", file=sys.stderr)


class ChironStorage:
    """PostgreSQL storage for Chiron agent."""
    
    def __init__(self, db_url: str):
        """
        Initialize storage.

        Args:
            db_url: PostgreSQL connection string
                   e.g., "postgresql://user:pass@localhost:5432/chiron_learning"
        """
        self.db_url = db_url
        self.conn = psycopg2.connect(db_url)
        self.conn.set_client_encoding('UTF8')
        self.embeddings = HuggingFaceEmbeddings(model_name="all-MiniLM-L6-v2") if HAVE_EMBEDDINGS else None

        # Initialize database schema
        self._init_schema()
    
    def _init_schema(self):
        """Initialize database schema if not exists."""
        # Extensions (vector + age) are pre-installed by postgres-ai image.
        with self.conn.cursor() as cur:

            # Create AGE graph for knowledge
            try:
                cur.execute("SELECT ag_catalog.create_graph('knowledge_graph');")
            except psycopg2.errors.DuplicateObject:
                self.conn.rollback()  # Rollback failed transaction
            except psycopg2.errors.InvalidSchemaName as e:
                # AGE throws InvalidSchemaName with message "graph already exists"
                # when trying to create a graph that exists (creates underlying schema)
                if 'already exists' in str(e):
                    self.conn.rollback()  # Rollback failed transaction
                else:
                    # This is a real invalid schema name error, re-raise it
                    raise
            
            # Migrate embedding column if dimension changed (e.g. 1536 → 384)
            cur.execute("""
                SELECT pg_catalog.format_type(a.atttypid, a.atttypmod)
                FROM pg_attribute a
                JOIN pg_class c ON a.attrelid = c.oid
                WHERE c.relname = 'textbook_chunks'
                  AND a.attname = 'embedding'
                  AND a.attnum > 0
                  AND NOT a.attisdropped
            """)
            existing = cur.fetchone()
            if existing and existing[0] != f'vector({EMBEDDING_DIM})':
                print(
                    f"Migrating textbook_chunks.embedding from {existing[0]} "
                    f"to vector({EMBEDDING_DIM}). Re-ingest your textbooks.",
                    file=sys.stderr,
                )
                cur.execute("DROP INDEX IF EXISTS textbook_chunks_embedding_idx;")
                cur.execute("ALTER TABLE textbook_chunks DROP COLUMN embedding;")
                cur.execute(f"ALTER TABLE textbook_chunks ADD COLUMN embedding vector({EMBEDDING_DIM});")
                self.conn.commit()

            # Vector storage for textbook chunks
            cur.execute(f"""
                CREATE TABLE IF NOT EXISTS textbook_chunks (
                    id SERIAL PRIMARY KEY,
                    textbook_name TEXT NOT NULL,
                    page_number INTEGER,
                    chunk_text TEXT NOT NULL,
                    embedding vector({EMBEDDING_DIM}),
                    metadata JSONB,
                    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                );
            """)

            # Index for vector similarity search
            cur.execute("""
                CREATE INDEX IF NOT EXISTS textbook_chunks_embedding_idx
                ON textbook_chunks
                USING ivfflat (embedding vector_cosine_ops)
                WITH (lists = 100);
            """)
            
            # Checkpoint storage for LangGraph state
            cur.execute("""
                CREATE TABLE IF NOT EXISTS agent_checkpoints (
                    thread_id TEXT NOT NULL,
                    checkpoint_id TEXT NOT NULL,
                    parent_checkpoint_id TEXT,
                    state JSONB NOT NULL,
                    metadata JSONB,
                    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                    PRIMARY KEY (thread_id, checkpoint_id)
                );
            """)
            
            # Learning session tracking
            cur.execute("""
                CREATE TABLE IF NOT EXISTS learning_sessions (
                    session_id SERIAL PRIMARY KEY,
                    thread_id TEXT NOT NULL,
                    concept TEXT NOT NULL,
                    explanation TEXT NOT NULL,
                    gaps JSONB,
                    score INTEGER,
                    mastered BOOLEAN DEFAULT FALSE,
                    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                );
            """)
            
            self.conn.commit()
    
    # ========================================================================
    # RAG: Textbook vector storage
    # ========================================================================
    
    def ingest_textbook(self, textbook_name: str, chunks: List[Dict]):
        """
        Ingest textbook chunks into vector storage.
        
        Args:
            textbook_name: Name of the textbook
            chunks: List of dicts with 'text', 'page', 'metadata'
        """
        if not self.embeddings:
            raise RuntimeError("Embeddings not available")
        
        with self.conn.cursor() as cur:
            for chunk in chunks:
                text = chunk['text']
                page = chunk.get('page')
                metadata = chunk.get('metadata', {})

                # Clean text: remove control characters used for mathematical notation formatting
                # \x00 and \x01 are PDF formatting markers around math symbols, not content
                text = text.replace('\x00', '').replace('\x01', '')

                # Generate embedding
                embedding = self.embeddings.embed_query(text)

                cur.execute("""
                    INSERT INTO textbook_chunks
                    (textbook_name, page_number, chunk_text, embedding, metadata)
                    VALUES (%s, %s, %s, %s, %s)
                """, (textbook_name, page, text, embedding, Json(metadata)))
            
            self.conn.commit()
    
    def search_textbook(self, query: str, textbook_names: List[str], k: int = 5) -> List[Dict]:
        """
        Semantic search in textbook chunks.
        
        Args:
            query: Search query
            textbook_names: List of textbook names to search
            k: Number of results
            
        Returns:
            List of chunks with text, page, similarity score
        """
        if not self.embeddings:
            return []
        
        query_embedding = self.embeddings.embed_query(query)
        
        with self.conn.cursor(cursor_factory=RealDictCursor) as cur:
            cur.execute("""
                SELECT 
                    textbook_name,
                    page_number,
                    chunk_text,
                    1 - (embedding <=> %s::vector) as similarity
                FROM textbook_chunks
                WHERE textbook_name = ANY(%s)
                ORDER BY embedding <=> %s::vector
                LIMIT %s
            """, (query_embedding, textbook_names, query_embedding, k))
            
            results = cur.fetchall()
            return [dict(r) for r in results]
    
    # ========================================================================
    # Graph: Knowledge graph and learning state
    # ========================================================================
    
    def create_concept_node(self, concept: str, properties: Dict = None):
        """Create a concept node in the knowledge graph."""
        props = properties or {}
        props['name'] = concept

        with self.conn.cursor() as cur:
            cur.execute(f"""
                SELECT * FROM ag_catalog.cypher('knowledge_graph', $$
                    MERGE (c:Concept {{name: '{concept}'}})
                    SET c += {json.dumps(props)}
                    RETURN c
                $$) as (concept agtype);
            """)
            self.conn.commit()

    def create_relationship(self, from_concept: str, to_concept: str, rel_type: str):
        """Create a relationship between concepts."""
        with self.conn.cursor() as cur:
            cur.execute(f"""
                SELECT * FROM ag_catalog.cypher('knowledge_graph', $$
                    MATCH (a:Concept {{name: '{from_concept}'}})
                    MATCH (b:Concept {{name: '{to_concept}'}})
                    MERGE (a)-[r:{rel_type}]->(b)
                    RETURN r
                $$) as (rel agtype);
            """)
            self.conn.commit()

    def get_prerequisites(self, concept: str) -> List[str]:
        """Get prerequisite concepts for a given concept."""
        with self.conn.cursor() as cur:
            cur.execute(f"""
                SELECT * FROM ag_catalog.cypher('knowledge_graph', $$
                    MATCH (c:Concept {{name: '{concept}'}})<-[:PREREQUISITE_FOR]-(prereq)
                    RETURN prereq.name
                $$) as (name agtype);
            """)
            return [row[0] for row in cur.fetchall()]

    def record_mastery(self, thread_id: str, concept: str, score: int, explanation: str):
        """Record that a concept has been mastered."""
        with self.conn.cursor() as cur:
            # Update graph
            cur.execute(f"""
                SELECT * FROM ag_catalog.cypher('knowledge_graph', $$
                    MERGE (s:Student {{thread_id: '{thread_id}'}})
                    MERGE (c:Concept {{name: '{concept}'}})
                    MERGE (s)-[m:MASTERED]->(c)
                    SET m.score = {score}, m.date = timestamp()
                    RETURN m
                $$) as (mastery agtype);
            """)
            
            # Also record in sessions table
            cur.execute("""
                INSERT INTO learning_sessions 
                (thread_id, concept, explanation, score, mastered)
                VALUES (%s, %s, %s, %s, %s)
            """, (thread_id, concept, explanation, score, True))
            
            self.conn.commit()
    
    def get_mastered_concepts(self, thread_id: str) -> List[Dict]:
        """Get all concepts mastered by this student."""
        with self.conn.cursor(cursor_factory=RealDictCursor) as cur:
            cur.execute("""
                SELECT concept, score, explanation, created_at
                FROM learning_sessions
                WHERE thread_id = %s AND mastered = TRUE
                ORDER BY created_at DESC
            """, (thread_id,))
            
            return [dict(r) for r in cur.fetchall()]
    
    # ========================================================================
    # Checkpoints: LangGraph state persistence
    # ========================================================================
    
    def save_checkpoint(self, thread_id: str, checkpoint_id: str, 
                       state: Dict, parent_id: Optional[str] = None):
        """Save a checkpoint for LangGraph."""
        with self.conn.cursor() as cur:
            cur.execute("""
                INSERT INTO agent_checkpoints 
                (thread_id, checkpoint_id, parent_checkpoint_id, state)
                VALUES (%s, %s, %s, %s)
                ON CONFLICT (thread_id, checkpoint_id) 
                DO UPDATE SET state = EXCLUDED.state
            """, (thread_id, checkpoint_id, parent_id, Json(state)))
            
            self.conn.commit()
    
    def load_checkpoint(self, thread_id: str, checkpoint_id: Optional[str] = None) -> Optional[Dict]:
        """Load a checkpoint for LangGraph."""
        with self.conn.cursor(cursor_factory=RealDictCursor) as cur:
            if checkpoint_id:
                cur.execute("""
                    SELECT state FROM agent_checkpoints
                    WHERE thread_id = %s AND checkpoint_id = %s
                """, (thread_id, checkpoint_id))
            else:
                # Get latest checkpoint
                cur.execute("""
                    SELECT state FROM agent_checkpoints
                    WHERE thread_id = %s
                    ORDER BY created_at DESC
                    LIMIT 1
                """, (thread_id,))
            
            row = cur.fetchone()
            return dict(row['state']) if row else None
    
    def close(self):
        """Close database connection."""
        self.conn.close()


# ============================================================================
# Utility: Ingest textbook PDF into PostgreSQL
# ============================================================================

def ingest_textbook_from_pdf(storage: ChironStorage, pdf_path: str, textbook_name: str):
    """
    Load a PDF textbook and ingest into PostgreSQL.
    
    Args:
        storage: ChironStorage instance
        pdf_path: Path to PDF file
        textbook_name: Name to identify this textbook
    """
    try:
        from langchain_community.document_loaders import PyPDFLoader
        from langchain_text_splitters import RecursiveCharacterTextSplitter
    except ImportError:
        print("ERROR: Need langchain_community and pypdf")
        print("pip install langchain-community pypdf")
        return
    
    print(f"Loading PDF: {pdf_path}")
    loader = PyPDFLoader(pdf_path)
    pages = loader.load()
    print(f"✓ Loaded {len(pages)} pages")
    
    print("Splitting into chunks...")
    splitter = RecursiveCharacterTextSplitter(
        chunk_size=1500,
        chunk_overlap=300
    )
    chunks = splitter.split_documents(pages)
    print(f"✓ Created {len(chunks)} chunks")
    
    print("Generating embeddings and storing...")
    chunk_dicts = [
        {
            'text': doc.page_content,
            'page': doc.metadata.get('page'),
            'metadata': doc.metadata
        }
        for doc in chunks
    ]
    
    storage.ingest_textbook(textbook_name, chunk_dicts)
    print(f"✓ Ingested {textbook_name} into PostgreSQL")


def build_db_url_with_schema(base_url, schema):
    """Build database URL with schema search_path."""
    from urllib.parse import quote
    options = f"-c search_path={schema},ag_catalog,public"
    return f"{base_url}?options={quote(options)}"


def create_schema(db_url, schema_name):
    """Create a PostgreSQL schema if it doesn't exist."""
    import psycopg2

    conn = psycopg2.connect(db_url)
    try:
        with conn.cursor() as cur:
            # Create schema
            cur.execute(f"CREATE SCHEMA IF NOT EXISTS {schema_name};")
            print(f"✓ Schema '{schema_name}' created (or already exists)")
            conn.commit()
    finally:
        conn.close()


if __name__ == "__main__":
    # Example usage
    import sys
    import argparse

    parser = argparse.ArgumentParser(description="Feynman Chiron textbook storage")
    subparsers = parser.add_subparsers(dest='command', help='Commands')

    # Create schema command
    schema_parser = subparsers.add_parser('create-schema', help='Create PostgreSQL schema(s)')
    schema_parser.add_argument('db_url', help='PostgreSQL database URL')
    schema_parser.add_argument('schemas', nargs='+', help='Schema name(s) to create (e.g., math physics)')

    # Ingest command
    ingest_parser = subparsers.add_parser('ingest', help='Ingest a PDF textbook')
    ingest_parser.add_argument('db_url', help='PostgreSQL database URL')
    ingest_parser.add_argument('--schema', '-s', required=True, help='Schema name (e.g., math, physics)')
    ingest_parser.add_argument('pdf_path', help='Path to PDF file')
    ingest_parser.add_argument('textbook_name', help='Name to identify this textbook')

    # Search command
    search_parser = subparsers.add_parser('search', help='Search textbook content')
    search_parser.add_argument('db_url', help='PostgreSQL database URL')
    search_parser.add_argument('--schema', '-s', required=True, help='Schema name')
    search_parser.add_argument('textbook_name', help='Textbook name to search')
    search_parser.add_argument('query', help='Search query')
    search_parser.add_argument('-k', type=int, default=3, help='Number of results (default: 3)')

    if len(sys.argv) == 1:
        parser.print_help()
        sys.exit(1)

    args = parser.parse_args()

    if args.command == "create-schema":
        print(f"Creating schema(s)...")
        for schema in args.schemas:
            create_schema(args.db_url, schema)
        print(f"\n✓ Done")

    elif args.command == "ingest":
        db_url = build_db_url_with_schema(args.db_url, args.schema)
        storage = ChironStorage(db_url)
        print(f"Ingesting into schema '{args.schema}'...")
        ingest_textbook_from_pdf(storage, args.pdf_path, args.textbook_name)
        storage.close()

    elif args.command == "search":
        db_url = build_db_url_with_schema(args.db_url, args.schema)
        storage = ChironStorage(db_url)
        results = storage.search_textbook(args.query, [args.textbook_name], k=args.k)

        print(f"\nSearch results for: {args.query}")
        print(f"Schema: {args.schema}, Textbook: {args.textbook_name}\n")
        for i, result in enumerate(results, 1):
            print(f"{i}. Page {result['page_number']} (similarity: {result['similarity']:.3f})")
            print(f"   {result['chunk_text'][:200]}...\n")

        storage.close()

    else:
        parser.print_help()
        sys.exit(1)
