use std::collections::HashMap;

use anyhow::Result;
use reqwest::Client;

use crate::{
    embeddings::Embedder,
    llm::chat,
    storage::Storage,
    types::{ChironState, Gap, Provider, Stage},
};

pub struct ProcessResult {
    pub response: String,
    pub concept: String,
    pub explanations: Vec<String>,
    pub gaps: Vec<Gap>,
    pub mastered_concepts: HashMap<String, serde_json::Value>,
    pub stage: String,
}

pub async fn process_explanation(
    concept: &str,
    explanation: &str,
    textbook_sources: &[String],
    thread_id: &str,
    provider: &Provider,
    textbook_pools: &HashMap<String, Storage>,
    embedder: &Embedder,
    learning: &Storage,
    client: &Client,
) -> Result<ProcessResult> {
    let state = ChironState::new(
        concept.to_string(),
        explanation.to_string(),
        textbook_sources.to_vec(),
        thread_id.to_string(),
    );

    let state = retrieve(state, textbook_pools, embedder).await;
    let state = analyze(state, provider, client).await;

    let state = match state.stage {
        Stage::Probe    => probe(state, provider, client).await,
        Stage::Evaluate => evaluate(state, provider, client, learning).await,
        _               => state,
    };

    // Persist checkpoint
    let checkpoint_val = serde_json::json!({
        "concept":    state.concept,
        "stage":      state.stage.as_str(),
        "gaps":       state.gaps,
        "mastered":   state.mastered_concepts,
    });
    if let Err(e) = learning.save_checkpoint(thread_id, &checkpoint_val).await {
        eprintln!("Warning: checkpoint save failed: {}", e);
    }

    let response = state.response_message
        .unwrap_or_else(|| "Please continue refining your explanation.".to_string());

    Ok(ProcessResult {
        response,
        concept: state.concept,
        explanations: state.explanations,
        gaps: state.gaps,
        mastered_concepts: state.mastered_concepts,
        stage: state.stage.as_str().to_string(),
    })
}

// ── Pipeline stages ───────────────────────────────────────────────────────────

async fn retrieve(
    mut state: ChironState,
    pools: &HashMap<String, Storage>,
    embedder: &Embedder,
) -> ChironState {
    if state.textbook_sources.is_empty() {
        state.stage = Stage::Analyze;
        return state;
    }

    let query_embedding = match embedder.embed(&state.concept) {
        Ok(v)  => v,
        Err(e) => {
            eprintln!("Embedding failed: {}", e);
            state.stage = Stage::Analyze;
            return state;
        }
    };

    let mut contexts = Vec::new();
    for source_name in &state.textbook_sources {
        let Some(storage) = pools.get(source_name) else {
            eprintln!("No connection to textbook '{}'", source_name);
            continue;
        };
        match storage.search_textbook(query_embedding.clone(), &[source_name.clone()], 2).await {
            Ok(chunks) => {
                for chunk in chunks {
                    contexts.push(format!(
                        "[{}, Page {}]\n{}",
                        chunk.textbook_name,
                        chunk.page_number.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
                        chunk.chunk_text
                    ));
                }
            }
            Err(e) => eprintln!("Search error for '{}': {}", source_name, e),
        }
    }

    state.textbook_context = contexts.join("\n\n---\n\n");
    state.stage = Stage::Analyze;
    state
}

async fn analyze(mut state: ChironState, provider: &Provider, client: &Client) -> ChironState {
    let explanation = match state.explanations.last() {
        Some(e) if !e.is_empty() => e.clone(),
        _ => {
            state.stage = Stage::Complete;
            return state;
        }
    };

    let textbook_section = if state.textbook_context.is_empty() {
        "[No textbook available]".to_string()
    } else {
        state.textbook_context.clone()
    };

    let system = format!(
        "You are a Socratic tutor using the Feynman Technique.

Student is learning: {}

Textbook content:
{}

Student's explanation:
{}

Identify specific gaps in their understanding. Look for:
1. Jargon not explained
2. Missing key ideas
3. Vague language
4. Circular definitions
5. Logical gaps

Return JSON list of gaps: [{{\"type\": \"...\", \"issue\": \"...\"}}, ...]
If explanation is complete and clear, return empty list: []",
        state.concept, textbook_section, explanation
    );

    match chat(client, provider, &system, "Identify the gaps:").await {
        Ok(content) => {
            let gaps = parse_json_array(&content);
            state.stage = if gaps.is_empty() { Stage::Evaluate } else { Stage::Probe };
            state.gaps = gaps;
        }
        Err(e) => {
            eprintln!("Analysis LLM error: {}", e);
            state.stage = Stage::Evaluate;
        }
    }
    state
}

async fn probe(mut state: ChironState, provider: &Provider, client: &Client) -> ChironState {
    let explanation = state.explanations.last().cloned().unwrap_or_default();
    let gaps_text = state.gaps.iter()
        .map(|g| format!("- {}: {}", g.kind, g.issue))
        .collect::<Vec<_>>()
        .join("\n");

    let system = format!(
        "You are a Socratic tutor.

Student explained '{}':
{}

Gaps identified:
{}

Generate 2-3 probing questions that expose these gaps without giving answers.
Make them think deeper. Be specific and reference their explanation.",
        state.concept, explanation, gaps_text
    );

    let msg = match chat(client, provider, &system, "Generate probing questions:").await {
        Ok(probes) => format!(
            "I notice some gaps:\n\n{}\n\nNow refine your explanation.",
            probes
        ),
        Err(e) => {
            eprintln!("Probe LLM error: {}", e);
            "Please refine your explanation.".to_string()
        }
    };

    state.response_message = Some(msg);
    state.stage = Stage::Complete;
    state
}

async fn evaluate(
    mut state: ChironState,
    provider: &Provider,
    client: &Client,
    learning: &Storage,
) -> ChironState {
    let explanation = state.explanations.last().cloned().unwrap_or_default();
    let textbook_section = if state.textbook_context.is_empty() {
        "[No textbook reference]".to_string()
    } else {
        state.textbook_context.clone()
    };

    let system = format!(
        "Evaluate if the student truly understands '{}'.

Textbook (correct explanation):
{}

Student's explanation:
{}

Criteria for mastery:
1. Uses simple language (12-year-old could understand)
2. Covers all essential aspects
3. No jargon without explanation
4. Shows understanding through examples or analogies
5. Explains WHY, not just WHAT

Return JSON: {{\"score\": X, \"feedback\": \"...\", \"mastered\": true/false}}
Score 1-10. Mastery if >= 8.",
        state.concept, textbook_section, explanation
    );

    let (score, mastered, msg) =
        match chat(client, provider, &system, "Evaluate mastery:").await {
            Ok(content) => parse_evaluation(&content),
            Err(e) => {
                eprintln!("Evaluate LLM error: {}", e);
                (5, false, "Please continue refining your explanation.".to_string())
            }
        };

    if mastered {
        state.mastered_concepts.insert(state.concept.clone(), serde_json::json!({
            "explanation": explanation,
            "score": score,
            "attempts": state.explanations.len(),
        }));
        if let Err(e) = learning.record_mastery(
            &state.thread_id, &state.concept, score, &explanation
        ).await {
            eprintln!("Warning: record_mastery failed: {}", e);
        }
    }

    state.response_message = Some(msg);
    state.stage = Stage::Complete;
    state
}

// ── JSON helpers ──────────────────────────────────────────────────────────────

fn parse_json_array(text: &str) -> Vec<Gap> {
    // Try direct parse first
    if let Ok(v) = serde_json::from_str::<Vec<Gap>>(text.trim()) {
        return v;
    }
    // Find first '[' … last ']'
    if let (Some(start), Some(end)) = (text.find('['), text.rfind(']')) {
        if let Ok(v) = serde_json::from_str::<Vec<Gap>>(&text[start..=end]) {
            return v;
        }
    }
    vec![]
}

fn parse_evaluation(text: &str) -> (i32, bool, String) {
    let json_str = match (text.find('{'), text.rfind('}')) {
        (Some(s), Some(e)) => &text[s..=e],
        _ => return (5, false, text.to_string()),
    };

    let Ok(v) = serde_json::from_str::<serde_json::Value>(json_str) else {
        return (5, false, text.to_string());
    };

    let score   = v["score"].as_i64().unwrap_or(5) as i32;
    let mastered = v["mastered"].as_bool().unwrap_or(false);
    let feedback = v["feedback"].as_str().unwrap_or("").to_string();

    let suffix = if mastered {
        "Excellent! You've mastered this concept!"
    } else {
        "Keep refining — you're getting closer!"
    };

    let msg = format!("Score: {}/10\n\n{}\n\n{}", score, feedback, suffix);
    (score, mastered, msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_array_from_clean_json() {
        let gaps = parse_json_array(r#"[{"type":"jargon","issue":"undefined term"}]"#);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].kind, "jargon");
    }

    #[test]
    fn parse_array_from_prose_wrapped_json() {
        let text = "Here are the gaps:\n[{\"type\":\"vague\",\"issue\":\"unclear\"}]\nDone.";
        let gaps = parse_json_array(text);
        assert_eq!(gaps.len(), 1);
    }

    #[test]
    fn parse_array_empty_list() {
        let gaps = parse_json_array("[]");
        assert!(gaps.is_empty());
    }

    #[test]
    fn parse_evaluation_mastered() {
        let text = r#"{"score": 9, "feedback": "Well done", "mastered": true}"#;
        let (score, mastered, msg) = parse_evaluation(text);
        assert_eq!(score, 9);
        assert!(mastered);
        assert!(msg.contains("mastered"));
    }

    #[test]
    fn parse_evaluation_not_mastered() {
        let text = r#"{"score": 4, "feedback": "Needs work", "mastered": false}"#;
        let (score, mastered, _msg) = parse_evaluation(text);
        assert_eq!(score, 4);
        assert!(!mastered);
    }
}
