use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub enum Provider {
    Anthropic { api_key: String, model: String, base_url: String },
    OpenAICompat { base_url: String, api_key: String, model: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stage {
    Initial,
    Analyze,
    Probe,
    Evaluate,
    Complete,
}

impl Stage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Stage::Initial  => "initial",
            Stage::Analyze  => "analyze",
            Stage::Probe    => "probe",
            Stage::Evaluate => "evaluate",
            Stage::Complete => "complete",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Gap {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub issue: String,
}

#[derive(Debug, Clone)]
pub struct ChironState {
    pub concept: String,
    pub textbook_context: String,
    pub explanations: Vec<String>,
    pub gaps: Vec<Gap>,
    pub stage: Stage,
    pub mastered_concepts: HashMap<String, serde_json::Value>,
    pub thread_id: String,
    pub textbook_sources: Vec<String>,
    pub response_message: Option<String>,
}

impl ChironState {
    pub fn new(
        concept: String,
        explanation: String,
        textbook_sources: Vec<String>,
        thread_id: String,
    ) -> Self {
        Self {
            concept,
            textbook_context: String::new(),
            explanations: vec![explanation],
            gaps: vec![],
            stage: Stage::Initial,
            mastered_concepts: HashMap::new(),
            thread_id,
            textbook_sources,
            response_message: None,
        }
    }
}

// ── stdin commands ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    Ready,
    Process {
        concept: String,
        explanation: String,
        #[serde(default)]
        textbook_sources: Vec<String>,
        #[serde(default = "default_thread")]
        thread_id: String,
    },
    GetMastered {
        #[serde(default = "default_thread")]
        thread_id: String,
    },
    Reset,
}

fn default_thread() -> String {
    "default".to_string()
}

// ── stdout responses ──────────────────────────────────────────────────────────

#[derive(Debug, Default, Serialize)]
pub struct Response {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<StateSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mastered_concepts: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub learning_schema: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct StateSnapshot {
    pub concept: String,
    pub explanations: Vec<String>,
    pub gaps: Vec<Gap>,
    pub mastered_concepts: HashMap<String, serde_json::Value>,
    pub stage: String,
}

impl Response {
    pub fn success_with(response: String, state: StateSnapshot) -> Self {
        Self {
            success: true,
            response: Some(response),
            state: Some(state),
            error: None,
            mastered_concepts: None,
            provider: None,
            model: None,
            database: None,
            learning_schema: None,
        }
    }

    pub fn mastered(concepts: Vec<serde_json::Value>) -> Self {
        Self {
            success: true,
            mastered_concepts: Some(concepts),
            response: None,
            state: None,
            error: None,
            provider: None,
            model: None,
            database: None,
            learning_schema: None,
        }
    }

    pub fn ready(provider: String, model: String, database: String, learning_schema: String) -> Self {
        Self {
            success: true,
            provider: Some(provider),
            model: Some(model),
            database: Some(database),
            learning_schema: Some(learning_schema),
            response: None,
            state: None,
            error: None,
            mastered_concepts: None,
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            success: false,
            error: Some(msg.into()),
            response: None,
            state: None,
            mastered_concepts: None,
            provider: None,
            model: None,
            database: None,
            learning_schema: None,
        }
    }
}

impl From<&ChironState> for StateSnapshot {
    fn from(s: &ChironState) -> Self {
        StateSnapshot {
            concept: s.concept.clone(),
            explanations: s.explanations.clone(),
            gaps: s.gaps.clone(),
            mastered_concepts: s.mastered_concepts.clone(),
            stage: s.stage.as_str().to_string(),
        }
    }
}
