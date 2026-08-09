use std::env;
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use chiron_core::{storage, url::with_schema, Embedder, Storage};

const CHUNK_SIZE: usize = 1500;
const CHUNK_OVERLAP: usize = 300;

fn usage() -> &'static str {
    "Feynman Chiron textbook storage (Rust)

Usage:
  chiron-ingest create-schema <db-url> <schema>...
  chiron-ingest ingest --schema <schema> <db-url> <pdf-path> <textbook-name>
  chiron-ingest search --schema <schema> [-k <n>] <db-url> <textbook-name> <query>
"
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {:#}", e);
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("create-schema") => cmd_create_schema(&args[1..]).await,
        Some("ingest")        => cmd_ingest(&args[1..]).await,
        Some("search")        => cmd_search(&args[1..]).await,
        _ => {
            eprint!("{}", usage());
            bail!("unknown or missing command");
        }
    }
}

// ── create-schema ────────────────────────────────────────────────────────────

async fn cmd_create_schema(args: &[String]) -> Result<()> {
    if args.len() < 2 {
        bail!("usage: chiron-ingest create-schema <db-url> <schema>...");
    }
    let db_url = &args[0];
    for schema in &args[1..] {
        storage::create_schema(db_url, schema).await?;
        println!("✓ Schema '{}' created (or already exists)", schema);
    }
    Ok(())
}

// ── ingest ───────────────────────────────────────────────────────────────────

struct IngestArgs<'a> {
    schema: &'a str,
    db_url: &'a str,
    pdf_path: &'a str,
    textbook_name: &'a str,
}

fn parse_ingest_args(args: &[String]) -> Result<IngestArgs<'_>> {
    let mut schema = None;
    let mut positional = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--schema" | "-s" => {
                i += 1;
                schema = args.get(i).map(String::as_str);
            }
            other => positional.push(other),
        }
        i += 1;
    }
    let schema = schema.context("--schema/-s is required")?;
    if positional.len() != 3 {
        bail!("usage: chiron-ingest ingest --schema <schema> <db-url> <pdf-path> <textbook-name>");
    }
    Ok(IngestArgs {
        schema,
        db_url: positional[0],
        pdf_path: positional[1],
        textbook_name: positional[2],
    })
}

async fn cmd_ingest(args: &[String]) -> Result<()> {
    let a = parse_ingest_args(args)?;

    println!("Loading PDF: {}", a.pdf_path);
    let pages = pdf_extract::extract_text_by_pages(a.pdf_path)
        .with_context(|| format!("Failed to extract text from {}", a.pdf_path))?;
    println!("✓ Loaded {} pages", pages.len());

    println!("Splitting into chunks...");
    let mut chunks: Vec<(usize, String)> = Vec::new();
    for (page_idx, page_text) in pages.iter().enumerate() {
        for chunk in split_text(page_text, CHUNK_SIZE, CHUNK_OVERLAP) {
            chunks.push((page_idx + 1, chunk));
        }
    }
    println!("✓ Created {} chunks", chunks.len());

    println!("Loading embedding model…");
    let embedder = Embedder::new().context("Failed to load embedding model")?;

    let db_url = with_schema(a.db_url, a.schema);
    println!("Connecting to database (schema '{}')...", a.schema);
    let storage = Storage::connect(&db_url).await
        .context("Failed to connect to database")?;

    println!("Generating embeddings and storing...");
    for (page_number, chunk_text) in &chunks {
        // \x00 / \x01 are PDF formatting markers around math notation, not content.
        let clean_text = chunk_text.replace(['\u{0}', '\u{1}'], "");
        if clean_text.trim().is_empty() {
            continue;
        }
        let embedding = embedder.embed(&clean_text)
            .with_context(|| format!("Failed to embed chunk on page {}", page_number))?;
        storage
            .insert_chunk(
                a.textbook_name,
                Some(*page_number as i32),
                &clean_text,
                embedding,
                &serde_json::json!({}),
            )
            .await
            .with_context(|| format!("Failed to store chunk on page {}", page_number))?;
    }

    println!("✓ Ingested '{}' into schema '{}'", a.textbook_name, a.schema);
    Ok(())
}

/// Paragraph-aware chunker: greedily merges paragraphs up to CHUNK_SIZE
/// chars, carrying CHUNK_OVERLAP chars of the previous chunk's tail into the
/// next one. Hard-splits any single paragraph that alone exceeds CHUNK_SIZE.
/// A rough Rust equivalent of langchain's RecursiveCharacterTextSplitter,
/// not a byte-for-byte match.
fn split_text(text: &str, chunk_size: usize, overlap: usize) -> Vec<String> {
    let paragraphs: Vec<&str> = text.split("\n\n").filter(|p| !p.trim().is_empty()).collect();
    let mut chunks = Vec::new();
    let mut current = String::new();

    for para in paragraphs {
        if !current.is_empty() && current.chars().count() + para.chars().count() + 2 > chunk_size {
            chunks.push(current.clone());
            current = tail_chars(&current, overlap);
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(para);

        while current.chars().count() > chunk_size {
            let head = take_chars(&current, chunk_size);
            chunks.push(head.clone());
            let consumed = head.chars().count();
            current = current.chars().skip(consumed.saturating_sub(overlap)).collect();
        }
    }
    if !current.trim().is_empty() {
        chunks.push(current);
    }
    chunks
}

fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn tail_chars(s: &str, n: usize) -> String {
    let total = s.chars().count();
    let skip = total.saturating_sub(n);
    s.chars().skip(skip).collect()
}

// ── search ───────────────────────────────────────────────────────────────────

struct SearchArgs<'a> {
    schema: &'a str,
    k: i64,
    db_url: &'a str,
    textbook_name: &'a str,
    query: &'a str,
}

async fn cmd_search(args: &[String]) -> Result<()> {
    let a = parse_search_args(args)?;

    let embedder = Embedder::new().context("Failed to load embedding model")?;
    let query_embedding = embedder.embed(a.query)?;

    let db_url = with_schema(a.db_url, a.schema);
    let storage = Storage::connect(&db_url).await
        .context("Failed to connect to database")?;

    let results = storage
        .search_textbook(query_embedding, &[a.textbook_name.to_string()], a.k)
        .await?;

    println!("\nSearch results for: {}", a.query);
    println!("Schema: {}, Textbook: {}\n", a.schema, a.textbook_name);
    for (i, r) in results.iter().enumerate() {
        println!(
            "{}. Page {} (similarity: {:.3})",
            i + 1,
            r.page_number.map(|p| p.to_string()).unwrap_or_else(|| "?".into()),
            r.similarity
        );
        let preview: String = r.chunk_text.chars().take(200).collect();
        println!("   {}...\n", preview);
    }
    Ok(())
}

fn parse_search_args(args: &[String]) -> Result<SearchArgs<'_>> {
    let mut schema = None;
    let mut k: i64 = 3;
    let mut positional = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--schema" | "-s" => {
                i += 1;
                schema = args.get(i).map(String::as_str);
            }
            "-k" => {
                i += 1;
                k = args.get(i).and_then(|s| s.parse().ok()).context("-k requires a number")?;
            }
            other => positional.push(other),
        }
        i += 1;
    }
    let schema = schema.context("--schema/-s is required")?;
    if positional.len() != 3 {
        bail!("usage: chiron-ingest search --schema <schema> [-k <n>] <db-url> <textbook-name> <query>");
    }
    Ok(SearchArgs { schema, k, db_url: positional[0], textbook_name: positional[1], query: positional[2] })
}
