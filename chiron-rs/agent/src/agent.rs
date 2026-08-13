use std::collections::HashMap;

use anyhow::Result;
use reqwest::Client;

use chiron_core::Storage;

use crate::{
    llm::{chat, SystemBlock},
    textbook_registry::TextbookRegistry,
    types::{ChironState, Gap, Provider, Stage},
};

pub struct ProcessResult {
    pub response: String,
    pub turns: Vec<String>,
    pub concept: String,
    pub explanations: Vec<String>,
    pub gaps: Vec<Gap>,
    pub mastered_concepts: HashMap<String, serde_json::Value>,
    pub stage: String,
}

/// Bound on individual `Chiron:` turns already in the transcript before we
/// force a `Synthesize` turn instead of asking yet another round of
/// questions. `probe` now splits its 2-3 questions into separate turns (one
/// `Chiron:`/`You:` pair each — see `parse_json_string_array`), so this
/// counts TURNS, not rounds: 6 comfortably covers ~2 rounds at up to 3
/// questions each. Derived straight from the transcript text on every call
/// (see `count_chiron_turns`) — the buffer itself is the single source of
/// truth, not a separate DB-backed count that can drift from it.
const MAX_CHIRON_TURNS: u32 = 6;

/// The explanation field is now the WHOLE conversation transcript, not a
/// single flattened "current explanation" — each turn is prefixed
/// `Chiron: ` or `You: ` (feynman-chiron.el's `--insert-response` writes
/// this format; the Emacs side no longer strips prior turns before
/// resubmitting). Counting `Chiron:` turns directly in that text is how we
/// know how many rounds have happened, with no separate persisted counter.
fn count_chiron_turns(explanation: &str) -> u32 {
    explanation.lines().filter(|l| l.trim_start().starts_with("Chiron:")).count() as u32
}

/// Extract just the student's most recent `You:` turn from the full
/// transcript, for storage paths (mastery records) where persisting the
/// whole growing conversation as "the explanation" would be redundant
/// bloat. Falls back to the whole string if no `You:` marker is present
/// yet (the very first submission, before any Chiron: turn exists).
fn last_student_turn(explanation: &str) -> String {
    match explanation.rfind("\nYou:").or_else(|| {
        explanation.starts_with("You:").then_some(0usize)
    }) {
        Some(pos) => {
            let after_marker = &explanation[pos..];
            let after_marker = after_marker.strip_prefix("\nYou:").or_else(|| after_marker.strip_prefix("You:")).unwrap_or(after_marker);
            after_marker.trim().to_string()
        }
        None => explanation.trim().to_string(),
    }
}

pub async fn process_explanation(
    concept: &str,
    explanation: &str,
    textbook_sources: &[String],
    thread_id: &str,
    provider: &Provider,
    textbook_registry: &mut TextbookRegistry,
    learning: &Storage,
    client: &Client,
) -> Result<ProcessResult> {
    let mut state = ChironState::new(
        concept.to_string(),
        explanation.to_string(),
        textbook_sources.to_vec(),
        thread_id.to_string(),
    );
    state.probe_rounds = count_chiron_turns(explanation);

    let state = retrieve(state, textbook_registry).await;

    let latest_reply = last_student_turn(explanation);
    let synthesis_mode = state.probe_rounds >= MAX_CHIRON_TURNS
        || student_requests_synthesis(&latest_reply);

    let state = if synthesis_mode {
        if student_agrees(&latest_reply) {
            // They agreed — close out for real (mastery check + a short
            // confirmation) instead of restating the same conclusion
            // again. `evaluate`'s own prompt is written to recognize this
            // case (a short agreement rather than a full restated
            // explanation) and grade the synthesis they endorsed.
            evaluate(state, provider, client, learning).await
        } else {
            // Not agreement — either nothing typed yet, or genuine
            // pushback/a follow-up question. Either way `synthesize`
            // reacts to whatever they actually said (its own prior turns
            // are right there in `explanation`, so it can say "as above"
            // for parts that still stand instead of re-deriving from
            // scratch, while addressing what's new).
            synthesize(state, provider, client).await
        }
    } else {
        let state = analyze(state, provider, client).await;
        match state.stage {
            Stage::Probe    => probe(state, provider, client).await,
            Stage::Evaluate => evaluate(state, provider, client, learning).await,
            _               => state,
        }
    };

    // Persist checkpoint (mastery history only now — round-tracking no
    // longer needs a read-back path, see `count_chiron_turns`).
    let checkpoint_val = serde_json::json!({
        "concept":  state.concept,
        "stage":    state.stage.as_str(),
        "gaps":     state.gaps,
        "mastered": state.mastered_concepts,
    });
    if let Err(e) = learning.save_checkpoint(thread_id, &checkpoint_val).await {
        eprintln!("Warning: checkpoint save failed: {}", e);
    }

    let response = state.response_message
        .unwrap_or_else(|| "Please continue refining your explanation.".to_string());
    let turns = if state.turns.is_empty() { vec![response.clone()] } else { state.turns.clone() };

    Ok(ProcessResult {
        response,
        turns,
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
    textbook_registry: &mut TextbookRegistry,
) -> ChironState {
    if state.textbook_sources.is_empty() {
        state.stage = Stage::Analyze;
        return state;
    }

    let mut contexts = Vec::new();
    // Different textbook sources can be configured with different embedding
    // models (feynman-chiron-ingest-textbook's per-project --model), so the
    // query must be re-embedded in each source's own vector space — but
    // sources sharing a model share the same Arc<Embedder> (TextbookRegistry),
    // so cache by model_id to avoid repeating an identical forward pass for
    // each of them on every learner turn.
    let mut embeddings_by_model: HashMap<String, Vec<f32>> = HashMap::new();
    for source_name in &state.textbook_sources {
        // Resolves and caches the source now if it wasn't ready at agent
        // startup (e.g. ingested moments after this process started), so a
        // newly-ingested textbook becomes usable without restarting.
        let Some((storage, embedder)) = textbook_registry.get(source_name).await else {
            eprintln!("No connection to textbook '{}'", source_name);
            continue;
        };
        let query_embedding = if let Some(v) = embeddings_by_model.get(embedder.model_id()) {
            v.clone()
        } else {
            match embedder.embed(&state.concept) {
                Ok(v) => {
                    embeddings_by_model.insert(embedder.model_id().to_string(), v.clone());
                    v
                }
                Err(e) => { eprintln!("Embedding failed for '{}': {}", source_name, e); continue; }
            }
        };
        match storage.search_textbook(query_embedding, std::slice::from_ref(source_name), 2).await {
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

    // Split so the parts that repeat byte-for-byte across turns (framing +
    // concept + textbook; the transcript, which only grows by appending)
    // are their own cache_control blocks — see `SystemBlock`. The tail
    // (task instructions) always changes least usefully, so it stays
    // uncached.
    let system = vec![
        SystemBlock::cached(format!(
            "You are a Socratic tutor using the Feynman Technique.

Student is learning: {}

Textbook content:
{}",
            state.concept, textbook_section
        )),
        SystemBlock::cached(format!(
            "Below is the conversation so far. Lines starting `Chiron:` are your own
prior turns; lines starting `You:` are the student's. Treat the LAST `You:`
segment as the fresh material to analyze — earlier turns are context, not
new content to re-critique.

{}",
            explanation
        )),
        SystemBlock::plain(
            "Identify specific gaps in the student's LATEST reply. Look for:
1. Jargon not explained
2. Missing key ideas
3. Vague language
4. Circular definitions
5. Logical gaps

Stay at the level of abstraction the student is working at. If the concept
they're learning is itself a conceptual/comparative distinction (not an
implementation), do NOT raise gaps about implementation details they didn't
ask about — that's a different, deeper topic they can choose to pursue later,
not a hole in THIS explanation. A gap only counts if it weakens the specific
claim the student is making, at the level they're making it. Do not repeat
a gap already raised and already addressed earlier in the conversation.

Return JSON list of gaps: [{\"type\": \"...\", \"issue\": \"...\"}, ...]
If the latest reply is complete and clear, return empty list: []"
        ),
    ];

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

    let system = vec![
        SystemBlock::cached(format!(
            "You are a Socratic tutor.

Concept: '{}'",
            state.concept
        )),
        SystemBlock::cached(format!(
            "Conversation so far (`Chiron:` = you, `You:` = the student — respond to
their LAST `You:` turn; do not repeat ground already covered above):
{}",
            explanation
        )),
        SystemBlock::plain(format!(
            "Gaps identified in their latest reply:
{}

Generate 2-3 probing questions that expose these gaps without giving answers.
Make them think deeper. Be specific and reference their explanation. Each
question must stand alone — the student will answer each one separately,
right after it, before seeing the next — so do not write \"first... second...\"
framing that assumes they're read together, and do not number them yourself.

Return a JSON list of strings, one question per element:
[\"question one\", \"question two\", \"question three\"]",
            gaps_text
        )),
    ];

    let questions = match chat(client, provider, &system, "Generate probing questions:").await {
        Ok(content) => {
            let qs = parse_json_string_array(&content);
            if qs.is_empty() {
                // Model didn't return valid JSON — fall back to the raw
                // text as a single turn rather than silently dropping it.
                vec![content]
            } else {
                qs
            }
        }
        Err(e) => {
            eprintln!("Probe LLM error: {}", e);
            vec!["Please refine your explanation.".to_string()]
        }
    };

    state.response_message = Some(questions.join("\n\n"));
    state.turns = questions;
    state.stage = Stage::Complete;
    state
}

/// True if the student's own text is directly telling the agent to stop
/// asking questions and give its own answer instead — e.g. "answer your
/// own questions", "we should stop here", "this is cyclic". Checked as a
/// deterministic string match rather than left to the LLM to notice on its
/// own: a live session showed the model NOT reliably honoring such a
/// request inside a general-purpose gap-finding prompt (round 4 got more
/// questions after the student explicitly wrote "answer your own
/// questions" in round 3).
///
/// Checked against the LATEST reply only, not the whole transcript: the
/// transcript is cumulative and never shrinks, so matching the whole thing
/// meant one early "let's stop" permanently locked every future turn into
/// synthesis mode, even after the student later tried to genuinely
/// continue with new substance.
fn student_requests_synthesis(latest_reply: &str) -> bool {
    let lower = latest_reply.to_lowercase();
    const TRIGGERS: &[&str] = &[
        "answer your own question",
        "answer yourself",
        "you answer",
        "stop here",
        "let's stop",
        "we should stop",
        "this is cyclic",
        "becomes cyclic",
    ];
    TRIGGERS.iter().any(|t| lower.contains(t))
}

/// True if the student's LATEST reply is a simple agreement/confirmation
/// rather than a substantive pushback — used after `synthesize` has
/// already given its understanding once, to decide whether to close out
/// (agree → `evaluate`) or keep the dialogue going for real (anything else
/// → `synthesize` again, reacting to what they actually said). Checked as
/// a deterministic prefix match, same reasoning as
/// `student_requests_synthesis`: don't trust the LLM alone to notice
/// "the student agreed, stop repeating yourself" inside a free-form prompt.
///
/// A hedge word anywhere in the reply ("yes, but...") overrides an
/// agreement-looking start — a nominal yes with a caveat is exactly the
/// "I don't fully agree" case that should continue the dialogue, not close it.
fn student_agrees(latest_reply: &str) -> bool {
    let lower = latest_reply.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    const AGREE_STARTS: &[&str] = &[
        "yes", "agree", "agreed", "correct", "right", "exactly",
        "understood", "ok", "okay", "sounds right", "sounds good",
        "makes sense", "that matches", "that's it", "thats it",
    ];
    if !AGREE_STARTS.iter().any(|t| lower.starts_with(t)) {
        return false;
    }
    const HEDGES: &[&str] = &[
        "but", "however", "actually", "except", "though",
        "not quite", "not really",
    ];
    !HEDGES.iter().any(|h| lower.contains(h))
}

/// Instead of asking another round of questions, have the agent state its
/// own best understanding of the concept — using the textbook context and
/// whatever gaps are still open — and ask the student to confirm or
/// correct it. This is the exit from the Probe loop: reached either
/// because the student asked for it directly (`student_requests_synthesis`)
/// or because `MAX_PROBE_ROUNDS` was hit without the student's explanation
/// ever coming back gap-free.
async fn synthesize(mut state: ChironState, provider: &Provider, client: &Client) -> ChironState {
    state.stage = Stage::Synthesize;
    let explanation = state.explanations.last().cloned().unwrap_or_default();
    let textbook_section = if state.textbook_context.is_empty() {
        "[No textbook available]".to_string()
    } else {
        state.textbook_context.clone()
    };
    let gaps_text = state.gaps.iter()
        .map(|g| format!("- {}: {}", g.kind, g.issue))
        .collect::<Vec<_>>()
        .join("\n");

    let system = vec![
        SystemBlock::cached(format!(
            "You are a tutor using the Feynman Technique. Time to stop asking
questions and state your own understanding instead — either because the
student asked directly, or because several rounds of questions haven't
converged.

Concept: {}

Textbook content:
{}",
            state.concept, textbook_section
        )),
        SystemBlock::cached(format!(
            "Conversation so far (`Chiron:` = you, `You:` = the student). IMPORTANT: if
you already gave your own understanding in an earlier `Chiron:` turn below,
do NOT re-derive or restate it in full — just briefly confirm it still
stands, react to what the student said since, and stop there.
{}",
            explanation
        )),
        SystemBlock::plain(format!(
            "Gaps previously raised (may now be moot — use judgment):
{}

If this is your FIRST synthesis in this conversation: give a direct,
concrete answer — state your own best understanding of the concept in plain
language, at the SAME level of abstraction the student was working at (do
not introduce a new, deeper topic they didn't ask about) — then ask the
student to confirm whether this matches what they meant, or correct it.

If you already gave your understanding AND the student's last turn is NOT
a simple agreement (they pushed back, disagreed, or asked a follow-up):
address their SPECIFIC point directly. Say plainly whether they're right,
and either revise your understanding accordingly or explain why it still
holds — do not just restate your prior answer unchanged, and do not ask a
battery of new probing questions.",
            gaps_text
        )),
    ];

    let msg = match chat(client, provider, &system, "Give your own understanding:").await {
        Ok(answer) => answer,
        Err(e) => {
            eprintln!("Synthesize LLM error: {}", e);
            "I wasn't able to reach the model to synthesize an answer — please try again.".to_string()
        }
    };

    state.turns = vec![msg.clone()];
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

    let system = vec![
        SystemBlock::cached(format!(
            "Evaluate if the student truly understands '{}'.

Textbook (correct explanation):
{}",
            state.concept, textbook_section
        )),
        SystemBlock::cached(format!(
            "Conversation so far (`Chiron:` = you, `You:` = the student) — evaluate
their LAST `You:` turn, using earlier turns only as context for what's
already been clarified. If that last turn is a short agreement/confirmation
(\"yes\", \"agreed\", \"correct\", etc.) rather than a full restated
explanation, that means the student is endorsing YOUR most recent `Chiron:`
synthesis below — evaluate THAT synthesis against the mastery criteria, not
the brevity of their one-word reply:
{}",
            explanation
        )),
        SystemBlock::plain(
            "Criteria for mastery:
1. Uses simple language (12-year-old could understand)
2. Covers all essential aspects
3. No jargon without explanation
4. Shows understanding through examples or analogies
5. Explains WHY, not just WHAT

Return JSON: {\"score\": X, \"feedback\": \"...\", \"mastered\": true/false}
Score 1-10. Mastery if >= 8. Keep feedback SHORT when the student simply
agreed — a couple of sentences confirming what's now understood, not a
fresh restatement of the whole explanation."
        ),
    ];

    let (score, mastered, msg) =
        match chat(client, provider, &system, "Evaluate mastery:").await {
            Ok(content) => parse_evaluation(&content),
            Err(e) => {
                eprintln!("Evaluate LLM error: {}", e);
                (5, false, "Please continue refining your explanation.".to_string())
            }
        };

    if mastered {
        let latest = last_student_turn(&explanation);
        state.mastered_concepts.insert(state.concept.clone(), serde_json::json!({
            "explanation": latest,
            "score": score,
            "attempts": state.explanations.len(),
        }));
        if let Err(e) = learning.record_mastery(
            &state.thread_id, &state.concept, score, &latest
        ).await {
            eprintln!("Warning: record_mastery failed: {}", e);
        }
    }

    state.turns = vec![msg.clone()];
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

fn parse_json_string_array(text: &str) -> Vec<String> {
    if let Ok(v) = serde_json::from_str::<Vec<String>>(text.trim()) {
        return v;
    }
    if let (Some(start), Some(end)) = (text.find('['), text.rfind(']')) {
        if let Ok(v) = serde_json::from_str::<Vec<String>>(&text[start..=end]) {
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

    #[test]
    fn agreement_recognizes_plain_agreement() {
        assert!(student_agrees("Yes, I think we are in agreement here."));
        assert!(student_agrees("Agreed."));
        assert!(student_agrees("correct"));
        assert!(student_agrees("Exactly what I meant."));
    }

    #[test]
    fn agreement_rejects_hedged_agreement() {
        assert!(!student_agrees("Yes, but I still don't see why closure matters."));
        assert!(!student_agrees("Correct, however I'd phrase the last part differently."));
    }

    #[test]
    fn agreement_rejects_disagreement_and_empty() {
        assert!(!student_agrees("No, I don't think that's right."));
        assert!(!student_agrees("Actually I meant something different."));
        assert!(!student_agrees(""));
        assert!(!student_agrees("   "));
    }

    #[test]
    fn synthesis_request_scoped_to_latest_reply_only() {
        // The whole point of taking `latest_reply` instead of the full
        // transcript: an old trigger phrase from an earlier turn must NOT
        // permanently lock every future turn into synthesis mode.
        assert!(student_requests_synthesis("Let's stop here and you answer."));
        assert!(!student_requests_synthesis("Actually, here's a new angle to consider."));
    }
}
