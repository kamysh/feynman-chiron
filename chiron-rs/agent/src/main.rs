mod agent;
mod llm;
mod types;

use std::collections::HashMap;
use std::env;
use std::io::{self, BufRead, Write};
use std::sync::Arc;

use anyhow::{Context, Result};
use reqwest::Client;

use chiron_core::{url::with_schema, Embedder, Storage};

/// A connected textbook source: its schema-scoped Storage, and the
/// Embedder matching the model that schema was actually ingested with
/// (`Storage::get_embedding_config`) — never assumed. Arc'd because
/// multiple sources commonly share one model and shouldn't each load
/// their own copy of it.
pub(crate) type TextbookSource = (Storage, Arc<Embedder>);

use crate::{
    agent::process_explanation,
    llm::{model_name, provider_name},
    types::{Command, Provider, Response},
};

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        let msg = format!("Fatal: {:#}", e);
        eprintln!("{}", msg);
        println!("{}", serde_json::to_string(&Response::err(msg)).unwrap());
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let provider     = build_provider()?;
    let database_url = env::var("CHIRON_DATABASE_URL")
        .context("CHIRON_DATABASE_URL is required")?;
    let learning_schema = env::var("CHIRON_LEARNING_SCHEMA")
        .context("CHIRON_LEARNING_SCHEMA is required")?;

    // Build connection URLs with search_path
    let learning_url  = with_schema(&database_url, &learning_schema);
    let textbook_json = env::var("CHIRON_TEXTBOOK_SOURCES").unwrap_or_else(|_| "{}".into());

    eprintln!("Connecting to learning database…");
    let learning = Storage::connect(&learning_url).await
        .context("Failed to connect to learning database")?;
    eprintln!("Learning database ready.");

    // Connect to textbook schemas
    let textbook_pools = build_textbook_pools(&database_url, &textbook_json).await;

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .connect_timeout(std::time::Duration::from_secs(30))
        .build()
        .context("Failed to build HTTP client")?;

    // Signal readiness on stdout so Emacs sees it
    println!(
        "READY provider={} model={} db={} schema={}",
        provider_name(&provider),
        model_name(&provider),
        database_url,
        learning_schema,
    );
    io::stdout().flush()?;

    // Command loop
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) if l.trim().is_empty() => continue,
            Ok(l)  => l,
            Err(e) => { eprintln!("stdin error: {}", e); break; }
        };

        let resp = handle_command(
            &line, &provider, &textbook_pools, &learning, &client
        ).await;

        if let Err(e) = writeln!(io::stdout(), "{}", serde_json::to_string(&resp)?) {
            eprintln!("stdout write error: {}", e);
            break;
        }
        io::stdout().flush()?;
    }

    Ok(())
}

async fn handle_command(
    line: &str,
    provider: &Provider,
    textbook_pools: &HashMap<String, TextbookSource>,
    learning: &Storage,
    client: &Client,
) -> Response {
    let cmd: Command = match serde_json::from_str(line) {
        Ok(c)  => c,
        Err(e) => return Response::err(format!("Invalid JSON: {}", e)),
    };

    match cmd {
        Command::Ready => Response::ready(
            provider_name(provider).to_string(),
            model_name(provider).to_string(),
            env::var("CHIRON_DATABASE_URL").unwrap_or_default(),
            env::var("CHIRON_LEARNING_SCHEMA").unwrap_or_default(),
        ),

        Command::Process { concept, explanation, textbook_sources, thread_id } => {
            match process_explanation(
                &concept, &explanation, &textbook_sources, &thread_id,
                provider, textbook_pools, learning, client,
            ).await {
                Ok(result) => Response::success_with(
                    result.response,
                    types::StateSnapshot {
                        concept:           result.concept,
                        explanations:      result.explanations,
                        gaps:              result.gaps,
                        mastered_concepts: result.mastered_concepts,
                        stage:             result.stage,
                    },
                ),
                Err(e) => Response::err(format!("Process error: {}", e)),
            }
        }

        Command::GetMastered { thread_id } => {
            match learning.get_mastered_concepts(&thread_id).await {
                Ok(concepts) => Response::mastered(concepts),
                Err(e)       => Response::err(format!("get_mastered error: {}", e)),
            }
        }

        Command::Reset => Response {
            success: true,
            response: Some("Session reset".into()),
            ..Default::default()
        },
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn build_provider() -> Result<Provider> {
    let provider_str = env::var("CHIRON_PROVIDER").unwrap_or_else(|_| "anthropic".into());
    let model        = env::var("CHIRON_MODEL")
        .context("CHIRON_MODEL is required")?;
    let api_key      = env::var("CHIRON_API_KEY")
        .or_else(|_| env::var("ANTHROPIC_API_KEY"))
        .or_else(|_| env::var("OPENAI_API_KEY"))
        .context("No API key: set CHIRON_API_KEY, ANTHROPIC_API_KEY, or OPENAI_API_KEY")?;

    match provider_str.as_str() {
        "anthropic" => Ok(Provider::Anthropic { api_key, model }),
        _ => {
            // "openai", "openai-compat", "groq", or any other value
            let base_url = env::var("CHIRON_ENDPOINT_URL")
                .unwrap_or_else(|_| "https://api.openai.com".into());
            Ok(Provider::OpenAICompat { base_url, api_key, model })
        }
    }
}

async fn build_textbook_pools(
    default_db_url: &str,
    json: &str,
) -> HashMap<String, TextbookSource> {
    let Ok(sources) = serde_json::from_str::<HashMap<String, serde_json::Value>>(json) else {
        eprintln!("Warning: could not parse CHIRON_TEXTBOOK_SOURCES JSON");
        return HashMap::new();
    };

    let mut pools = HashMap::new();
    // Each project/schema was ingested with its own chosen embedding model
    // (feynman-chiron-ingest-textbook's --model, persisted in that schema's
    // embedding_config table) — load one Embedder per distinct model actually
    // in use, not a single global one, and reuse it across sources that
    // happen to share a model.
    let mut embedders: HashMap<String, Arc<Embedder>> = HashMap::new();

    for (name, spec) in &sources {
        let (db_url, schema) = match spec {
            serde_json::Value::String(s) => (default_db_url.to_string(), s.clone()),
            serde_json::Value::Object(m) => {
                let schema = m.get("schema")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let db = m.get("database")
                    .and_then(|v| v.as_str())
                    .unwrap_or(default_db_url)
                    .to_string();
                (db, schema)
            }
            _ => { eprintln!("Invalid spec for textbook '{}'", name); continue; }
        };
        if schema.is_empty() {
            eprintln!("No schema for textbook '{}'", name);
            continue;
        }
        let url = with_schema(&db_url, &schema);
        let storage = match Storage::connect(&url).await {
            Ok(s)  => s,
            Err(e) => { eprintln!("Failed to connect to textbook '{}': {}", name, e); continue; }
        };

        let (model, dim) = match storage.get_embedding_config().await {
            Ok(Some(cfg)) => cfg,
            Ok(None) => {
                eprintln!(
                    "Textbook '{}' (schema '{}') has no ingested content yet — skipping",
                    name, schema
                );
                continue;
            }
            Err(e) => {
                eprintln!("Failed to read embedding config for '{}': {}", name, e);
                continue;
            }
        };

        let embedder = if let Some(e) = embedders.get(&model) {
            e.clone()
        } else {
            eprintln!("Loading embedding model '{}' for '{}'…", model, name);
            match Embedder::new(&model) {
                Ok(e) => {
                    let e = Arc::new(e);
                    embedders.insert(model.clone(), e.clone());
                    e
                }
                Err(e) => {
                    eprintln!("Failed to load embedding model '{}' for '{}': {}", model, name, e);
                    continue;
                }
            }
        };

        if embedder.dim() as i32 != dim {
            eprintln!(
                "Textbook '{}' (schema '{}'): embedding_config says model '{}' is {}-dim, \
                 but it actually reports {}-dim now — skipping to avoid a dimension mismatch",
                name, schema, model, dim, embedder.dim()
            );
            continue;
        }

        pools.insert(name.clone(), (storage, embedder));
    }
    pools
}

// Allow Response to have default fields for the Reset arm
impl Default for Response {
    fn default() -> Self {
        Self {
            success: false,
            response: None,
            error: None,
            state: None,
            mastered_concepts: None,
            provider: None,
            model: None,
            database: None,
            learning_schema: None,
        }
    }
}
