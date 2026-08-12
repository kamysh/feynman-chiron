mod agent;
mod llm;
mod textbook_registry;
mod types;

use std::env;
use std::io::{self, BufRead, Write};

use anyhow::{Context, Result};
use reqwest::Client;

use chiron_core::{url::with_schema, Storage};

use crate::{
    agent::process_explanation,
    llm::{model_name, provider_name},
    textbook_registry::TextbookRegistry,
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
    if matches!(env::args().nth(1).as_deref(), Some("--version") | Some("-V")) {
        println!("chiron-rs {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

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
    let mut textbook_registry = TextbookRegistry::build(database_url.clone(), &textbook_json).await;

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
            &line, &provider, &mut textbook_registry, &learning, &client
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
    textbook_registry: &mut TextbookRegistry,
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
                provider, textbook_registry, learning, client,
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
        "anthropic" => {
            let base_url = env::var("CHIRON_ENDPOINT_URL")
                .unwrap_or_else(|_| "https://api.anthropic.com".into());
            Ok(Provider::Anthropic { api_key, model, base_url })
        }
        _ => {
            // "openai", "openai-compat", "groq", or any other value
            let base_url = env::var("CHIRON_ENDPOINT_URL")
                .unwrap_or_else(|_| "https://api.openai.com".into());
            Ok(Provider::OpenAICompat { base_url, api_key, model })
        }
    }
}

