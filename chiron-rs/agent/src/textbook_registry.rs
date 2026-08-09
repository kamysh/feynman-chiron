use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::task::JoinSet;

use chiron_core::{url::with_schema, Embedder, Storage};

/// A connected textbook source: its schema-scoped Storage, and the
/// Embedder matching the model that schema was actually ingested with
/// (`Storage::get_embedding_config`) — never assumed. Both Arc'd: multiple
/// sources commonly share one embedding model and shouldn't each load
/// their own copy of it, and cloning out of `TextbookRegistry` needs to be
/// cheap and ownership-free (see `TextbookRegistry::get`).
pub(crate) type TextbookSource = (Arc<Storage>, Arc<Embedder>);

fn parse_spec(default_db_url: &str, name: &str, spec: &serde_json::Value) -> Option<(String, String)> {
    match spec {
        serde_json::Value::String(s) => Some((default_db_url.to_string(), s.clone())),
        serde_json::Value::Object(m) => {
            let schema = m.get("schema").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if schema.is_empty() {
                eprintln!("No schema for textbook '{}'", name);
                return None;
            }
            let db = m.get("database")
                .and_then(|v| v.as_str())
                .unwrap_or(default_db_url)
                .to_string();
            Some((db, schema))
        }
        _ => {
            eprintln!("Invalid spec for textbook '{}'", name);
            None
        }
    }
}

/// Connect to NAME's schema and read its persisted embedding config.
/// Deliberately does NOT load an embedding model — that's a separate,
/// often-shared step (see `TextbookRegistry`), so a caller resolving
/// several sources at once can dedupe model loads across them.
async fn connect_and_read_config(
    default_db_url: &str,
    name: &str,
    spec: &serde_json::Value,
) -> Option<(Storage, String, i32)> {
    let (db_url, schema) = parse_spec(default_db_url, name, spec)?;
    let url = with_schema(&db_url, &schema);
    let storage = match Storage::connect(&url).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to connect to textbook '{}': {}", name, e);
            return None;
        }
    };
    match storage.get_embedding_config().await {
        Ok(Some((model, dim))) => Some((storage, model, dim)),
        Ok(None) => {
            eprintln!("Textbook '{}' (schema '{}') has no ingested content yet", name, schema);
            None
        }
        Err(e) => {
            eprintln!("Failed to read embedding config for '{}': {}", name, e);
            None
        }
    }
}

/// Load MODEL on a blocking thread — `Embedder::new` does blocking network
/// I/O (first use) and CPU-bound model loading, neither of which should
/// run on an async task directly.
async fn load_embedder(model: String) -> Option<Arc<Embedder>> {
    let load_result = tokio::task::spawn_blocking({
        let model = model.clone();
        move || Embedder::new(&model)
    }).await;
    match load_result {
        Ok(Ok(embedder)) => Some(Arc::new(embedder)),
        Ok(Err(e)) => {
            eprintln!("Failed to load embedding model '{}': {}", model, e);
            None
        }
        Err(e) => {
            eprintln!("Embedding model load task for '{}' panicked: {}", model, e);
            None
        }
    }
}

/// All of a session's textbook sources: raw config (for lazily resolving
/// sources that weren't ready yet at `build()` time), connected+validated
/// pools for sources that are, and a model_id-keyed embedder cache shared
/// across all of them.
pub(crate) struct TextbookRegistry {
    default_db_url: String,
    sources: HashMap<String, serde_json::Value>,
    pools: HashMap<String, TextbookSource>,
    embedders: HashMap<String, Arc<Embedder>>,
}

impl TextbookRegistry {
    /// Parse JSON (CHIRON_TEXTBOOK_SOURCES) and connect every named source
    /// concurrently: connect+config-read for all sources runs in parallel,
    /// then one Embedder per distinct model (deduped) loads in parallel —
    /// N sources no longer cost N sequential connects and model loads.
    /// A source with no ingested content yet is skipped, not an error; see
    /// `get` for how it becomes usable once it is ingested.
    pub(crate) async fn build(default_db_url: String, json: &str) -> Self {
        let sources: HashMap<String, serde_json::Value> =
            serde_json::from_str(json).unwrap_or_else(|_| {
                eprintln!("Warning: could not parse CHIRON_TEXTBOOK_SOURCES JSON");
                HashMap::new()
            });

        let mut connect_tasks = JoinSet::new();
        for (name, spec) in sources.clone() {
            let default_db_url = default_db_url.clone();
            connect_tasks.spawn(async move {
                let resolved = connect_and_read_config(&default_db_url, &name, &spec).await;
                (name, resolved)
            });
        }
        let mut resolved: Vec<(String, Storage, String, i32)> = Vec::new();
        while let Some(res) = connect_tasks.join_next().await {
            if let Ok((name, Some((storage, model, dim)))) = res {
                resolved.push((name, storage, model, dim));
            }
        }

        let distinct_models: HashSet<String> =
            resolved.iter().map(|(_, _, model, _)| model.clone()).collect();
        let mut model_tasks = JoinSet::new();
        for model in distinct_models {
            model_tasks.spawn(async move {
                let embedder = load_embedder(model.clone()).await;
                (model, embedder)
            });
        }
        let mut embedders: HashMap<String, Arc<Embedder>> = HashMap::new();
        while let Some(res) = model_tasks.join_next().await {
            if let Ok((model, Some(embedder))) = res {
                embedders.insert(model, embedder);
            }
        }

        let mut pools = HashMap::new();
        for (name, storage, model, dim) in resolved {
            let Some(embedder) = embedders.get(&model) else { continue };
            if embedder.dim() as i32 != dim {
                eprintln!(
                    "Textbook '{}': embedding_config says model '{}' is {}-dim, but it \
                     actually reports {}-dim now — skipping to avoid a dimension mismatch",
                    name, model, dim, embedder.dim()
                );
                continue;
            }
            pools.insert(name, (Arc::new(storage), embedder.clone()));
        }

        Self { default_db_url, sources, pools, embedders }
    }

    /// Get NAME's connected+validated source, resolving and caching it now
    /// if it wasn't ready at `build()` time (e.g. ingested moments after
    /// this agent process started) — so a newly-ingested textbook becomes
    /// usable on its very next reference instead of staying excluded until
    /// the agent is restarted. Returns owned Arc clones, cheap either way.
    pub(crate) async fn get(&mut self, name: &str) -> Option<TextbookSource> {
        if !self.pools.contains_key(name) {
            if let Some(source) = self.resolve_one(name).await {
                self.pools.insert(name.to_string(), source);
            }
        }
        self.pools.get(name).cloned()
    }

    async fn resolve_one(&mut self, name: &str) -> Option<TextbookSource> {
        let spec = self.sources.get(name)?.clone();
        let (storage, model, dim) = connect_and_read_config(&self.default_db_url, name, &spec).await?;
        let embedder = if let Some(e) = self.embedders.get(&model) {
            e.clone()
        } else {
            eprintln!("Loading embedding model '{}' for '{}'…", model, name);
            let e = load_embedder(model.clone()).await?;
            self.embedders.insert(model.clone(), e.clone());
            e
        };
        if embedder.dim() as i32 != dim {
            eprintln!(
                "Textbook '{}': embedding_config says model '{}' is {}-dim, but it actually \
                 reports {}-dim now — refusing",
                name, model, dim, embedder.dim()
            );
            return None;
        }
        Some((Arc::new(storage), embedder))
    }
}
