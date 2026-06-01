#!/usr/bin/env python3
"""
LangGraph-based Feynman Chiron backend with PostgreSQL storage.

Uses:
- PostgreSQL with Apache AGE for knowledge graph
- pgvector for RAG semantic search
- Standard tables for checkpoints

Single database for everything.
"""

import sys
import json
import os
from typing import TypedDict, List, Annotated
from operator import add

# Check dependencies
try:
    from langgraph.graph import StateGraph, END
    from langchain_openai import ChatOpenAI
    from langchain_anthropic import ChatAnthropic
    from langchain_core.messages import BaseMessage, HumanMessage, AIMessage, SystemMessage
    from chiron_storage import ChironStorage
    HAVE_LANGGRAPH = True
except ImportError as e:
    print(f"ERROR: Missing dependencies: {e}", file=sys.stderr)
    print("Install: pip install langgraph langchain langchain-openai langchain-anthropic psycopg2-binary", file=sys.stderr)
    HAVE_LANGGRAPH = False
    sys.exit(1)


class ChironState(TypedDict):
    """State for Chiron learning agent."""
    # Current conversation
    messages: Annotated[List[BaseMessage], add]
    
    # What concept is being learned
    concept: str
    
    # Textbook context retrieved
    textbook_context: str
    
    # Student's explanations (history)
    explanations: List[str]
    
    # Identified gaps
    gaps: List[dict]
    
    # Current stage
    stage: str  # 'initial', 'analyze', 'probe', 'evaluate', 'complete'
    
    # Mastered concepts
    mastered_concepts: dict
    
    # Textbook sources dict (name → schema) as decoded from JSON
    textbook_sources: dict


class ChironAgent:
    """Feynman Chiron learning agent using LangGraph and PostgreSQL."""

    def __init__(self, provider='anthropic', model=None, api_key=None,
                 database_url=None, learning_schema=None, textbook_sources=None):
        self.provider = provider
        self.database_url = database_url
        self.learning_schema = learning_schema

        # Initialize LLM
        if provider == 'openai':
            self.llm = ChatOpenAI(
                model=model or "gpt-4",
                temperature=0.3,
                api_key=api_key or os.getenv("OPENAI_API_KEY")
            )
        else:  # anthropic
            self.llm = ChatAnthropic(
                model=model or "claude-sonnet-4-6",
                temperature=0.3,
                api_key=api_key or os.getenv("ANTHROPIC_API_KEY")
            )

        # Initialize PostgreSQL storage for learning state
        if not database_url:
            raise ValueError("Database URL required (CHIRON_DATABASE_URL)")
        if not learning_schema:
            raise ValueError("Learning schema required (CHIRON_LEARNING_SCHEMA)")

        learning_db_url = self._build_db_url(database_url, learning_schema)
        self.learning_storage = ChironStorage(learning_db_url)
        print(f"Learning database: {database_url} / {learning_schema}", file=sys.stderr)

        # Initialize connections to textbook schemas
        # textbook_sources: {"textbook_name": {"schema": "name"}}
        #              or: {"textbook_name": {"database": "url", "schema": "name"}}
        self.textbook_sources = textbook_sources or {}
        self.textbook_connections = {}

        for name, spec in self.textbook_sources.items():
            try:
                # Handle both formats
                if isinstance(spec, str):
                    # Old format: just schema name
                    schema = spec
                    source_db_url = database_url
                elif isinstance(spec, dict):
                    # New format: dict with "schema" and optionally "database"
                    schema = spec.get("schema")
                    source_db_url = spec.get("database", database_url)
                else:
                    print(f"Invalid format for textbook '{name}', skipping", file=sys.stderr)
                    continue

                if not schema:
                    print(f"No schema specified for textbook '{name}', skipping", file=sys.stderr)
                    continue

                schema_url = self._build_db_url(source_db_url, schema)
                conn = ChironStorage(schema_url)
                self.textbook_connections[name] = conn

                if source_db_url == database_url:
                    print(f"Textbook source '{name}': {database_url} / {schema}", file=sys.stderr)
                else:
                    print(f"Textbook source '{name}': {source_db_url} / {schema}", file=sys.stderr)
            except Exception as e:
                print(f"Failed to connect to textbook '{name}': {e}", file=sys.stderr)

        # Build the graph
        self.graph = self._build_graph()

    def _build_db_url(self, base_url, schema):
        """Build database URL with schema search_path."""
        return f"{base_url}?options=-c%20search_path={schema}"
    
    def retrieve_from_textbooks(self, state: ChironState) -> ChironState:
        """Retrieve relevant content from multiple textbook databases."""
        concept = state["concept"]
        textbook_sources = state.get("textbook_sources", [])
        
        if not textbook_sources:
            return {**state, "textbook_context": "", "stage": "analyze"}
        
        all_results = []
        
        # Query each textbook source
        for source_name in textbook_sources:
            if source_name not in self.textbook_connections:
                print(f"No connection to textbook '{source_name}'", file=sys.stderr)
                continue
            
            try:
                conn = self.textbook_connections[source_name]
                # Search in this textbook's database
                results = conn.search_textbook(concept, [source_name], k=2)
                all_results.extend(results)
            except Exception as e:
                print(f"Error querying '{source_name}': {e}", file=sys.stderr)
        
        # Format combined results
        contexts = []
        for result in all_results:
            context = f"[{result['textbook_name']}, Page {result['page_number']}]\n{result['chunk_text']}"
            contexts.append(context)
        
        combined_context = "\n\n---\n\n".join(contexts) if contexts else ""
        
        return {
            **state,
            "textbook_context": combined_context,
            "stage": "analyze"
        }
    
    def analyze_explanation(self, state: ChironState) -> ChironState:
        """Analyze student's explanation and identify gaps."""
        concept = state["concept"]
        explanation = state["explanations"][-1] if state["explanations"] else ""
        textbook = state.get("textbook_context", "")
        
        if not explanation:
            return {**state, "stage": "complete"}
        
        prompt = f"""You are a Socratic tutor using the Feynman Technique.

Student is learning: {concept}

Textbook content:
{textbook if textbook else "[No textbook available]"}

Student's explanation:
{explanation}

Identify specific gaps in their understanding. Look for:
1. Jargon not explained
2. Missing key ideas
3. Vague language
4. Circular definitions
5. Logical gaps

Return JSON list of gaps: [{{"type": "...", "issue": "..."}}, ...]
If explanation is complete and clear, return empty list: []"""

        try:
            response = self.llm.invoke([SystemMessage(content=prompt)])
            content = response.content
            
            # Parse JSON
            gaps = json.loads(content) if content.strip().startswith('[') else []
            
            next_stage = "evaluate" if not gaps else "probe"
            
            return {
                **state,
                "gaps": gaps,
                "stage": next_stage
            }
        
        except Exception as e:
            print(f"Analysis error: {e}", file=sys.stderr)
            return {**state, "gaps": [], "stage": "evaluate"}
    
    def generate_probes(self, state: ChironState) -> ChironState:
        """Generate probing questions to expose gaps."""
        concept = state["concept"]
        explanation = state["explanations"][-1]
        gaps = state["gaps"]
        
        gaps_text = "\n".join([
            f"- {g.get('type', 'unknown')}: {g.get('issue', '')}"
            for g in gaps
        ])
        
        prompt = f"""You are a Socratic tutor.

Student explained '{concept}':
{explanation}

Gaps identified:
{gaps_text}

Generate 2-3 probing questions that expose these gaps without giving answers.
Make them think deeper. Be specific and reference their explanation."""

        try:
            response = self.llm.invoke([SystemMessage(content=prompt)])
            probes = response.content
            
            return {
                **state,
                "messages": [AIMessage(content=f"I notice some gaps:\n\n{probes}\n\nNow refine your explanation.")],
                "stage": "complete"
            }
        
        except Exception as e:
            print(f"Probe generation error: {e}", file=sys.stderr)
            return {
                **state,
                "messages": [AIMessage(content="Please refine your explanation.")],
                "stage": "complete"
            }
    
    def evaluate_mastery(self, state: ChironState) -> ChironState:
        """Evaluate if student has mastered the concept."""
        concept = state["concept"]
        explanation = state["explanations"][-1]
        textbook = state.get("textbook_context", "")
        thread_id = state.get("thread_id", "default")
        
        prompt = f"""Evaluate if the student truly understands '{concept}'.

Textbook (correct explanation):
{textbook if textbook else "[No textbook reference]"}

Student's explanation:
{explanation}

Criteria for mastery:
1. Uses simple language (12-year-old could understand)
2. Covers all essential aspects
3. No jargon without explanation
4. Shows understanding through examples or analogies
5. Explains WHY, not just WHAT

Return JSON: {{"score": X, "feedback": "...", "mastered": true/false}}
Score 1-10. Mastery if >= 8."""

        try:
            response = self.llm.invoke([SystemMessage(content=prompt)])
            content = response.content
            
            # Extract JSON
            if '{' in content:
                json_str = content[content.find('{'):content.rfind('}')+1]
                result = json.loads(json_str)
            else:
                result = {"score": 5, "feedback": content, "mastered": False}
            
            score = result.get("score", 5)
            mastered = result.get("mastered", False)
            feedback = result.get("feedback", "")
            
            # Update mastered concepts
            mastered_concepts = state.get("mastered_concepts", {})
            if mastered:
                mastered_concepts[concept] = {
                    "explanation": explanation,
                    "score": score,
                    "attempts": len(state["explanations"])
                }
                
                # Record in PostgreSQL graph
                try:
                    self.learning_storage.record_mastery(thread_id, concept, score, explanation)
                except Exception as e:
                    print(f"Error recording mastery: {e}", file=sys.stderr)
            
            response_msg = f"Score: {score}/10\n\n{feedback}\n\n"
            if mastered:
                response_msg += "🎉 Excellent! You've mastered this concept!"
            else:
                response_msg += "Keep refining - you're getting closer!"
            
            return {
                **state,
                "mastered_concepts": mastered_concepts,
                "messages": [AIMessage(content=response_msg)],
                "stage": "complete"
            }
        
        except Exception as e:
            print(f"Evaluation error: {e}", file=sys.stderr)
            return {
                **state,
                "messages": [AIMessage(content="Please continue refining your explanation.")],
                "stage": "complete"
            }
    
    def route_after_analysis(self, state: ChironState) -> str:
        """Route based on analysis results."""
        stage = state.get("stage", "complete")
        if stage == "probe":
            return "probe"
        elif stage == "evaluate":
            return "evaluate"
        else:
            return "end"
    
    def _build_graph(self):
        """Build the LangGraph workflow."""
        workflow = StateGraph(ChironState)
        
        # Add nodes
        workflow.add_node("retrieve", self.retrieve_from_textbooks)
        workflow.add_node("analyze", self.analyze_explanation)
        workflow.add_node("probe", self.generate_probes)
        workflow.add_node("evaluate", self.evaluate_mastery)
        
        # Define edges
        workflow.set_entry_point("retrieve")
        workflow.add_edge("retrieve", "analyze")
        
        # Conditional routing after analysis
        workflow.add_conditional_edges(
            "analyze",
            self.route_after_analysis,
            {
                "probe": "probe",
                "evaluate": "evaluate",
                "end": END
            }
        )
        
        workflow.add_edge("probe", END)
        workflow.add_edge("evaluate", END)
        
        # Compile (checkpointing handled by PostgreSQL)
        return workflow.compile()
    
    def process_explanation(self, concept, explanation, textbook_sources, thread_id="default"):
        """Process a student explanation through the Chiron workflow.
        
        Args:
            concept: The concept being learned
            explanation: Student's explanation
            textbook_sources: List of (name, url) tuples for textbook databases
            thread_id: Thread ID for tracking (e.g., filename)
        """
        
        # Build state
        state = {
            "messages": [],
            "concept": concept,
            "textbook_context": "",
            "explanations": [explanation],
            "gaps": [],
            "stage": "initial",
            "mastered_concepts": {},
            "textbook_sources": textbook_sources,
            "thread_id": thread_id
        }
        
        # Run through the graph
        result = self.graph.invoke(state)
        
        # Save checkpoint to PostgreSQL
        try:
            import uuid
            checkpoint_id = str(uuid.uuid4())
            self.learning_storage.save_checkpoint(thread_id, checkpoint_id, result)
        except Exception as e:
            print(f"Error saving checkpoint: {e}", file=sys.stderr)
        
        # Extract response
        response_msg = result["messages"][-1].content if result.get("messages") else ""
        
        return {
            "success": True,
            "response": response_msg,
            "state": {
                "concept": result.get("concept", ""),
                "explanations": result.get("explanations", []),
                "gaps": result.get("gaps", []),
                "mastered_concepts": result.get("mastered_concepts", {}),
                "stage": result.get("stage", "complete")
            }
        }


def main():
    """Main loop: process commands from Emacs."""

    if not HAVE_LANGGRAPH:
        sys.exit(1)

    # Get configuration from environment
    provider = os.getenv("CHIRON_PROVIDER", "anthropic")
    model = os.getenv("CHIRON_MODEL", None)
    database_url = os.getenv("CHIRON_DATABASE_URL")
    learning_schema = os.getenv("CHIRON_LEARNING_SCHEMA")
    textbook_sources_json = os.getenv("CHIRON_TEXTBOOK_SOURCES", "{}")

    if not database_url:
        print("ERROR: CHIRON_DATABASE_URL environment variable required", file=sys.stderr)
        print("Example: postgresql://user:pass@localhost:5432/chiron", file=sys.stderr)
        sys.exit(1)

    if not learning_schema:
        print("ERROR: CHIRON_LEARNING_SCHEMA environment variable required", file=sys.stderr)
        print("Example: learning", file=sys.stderr)
        sys.exit(1)

    # Parse textbook sources from JSON (now schema names, not full URLs)
    try:
        textbook_sources = json.loads(textbook_sources_json)
    except json.JSONDecodeError:
        textbook_sources = {}

    try:
        agent = ChironAgent(
            provider=provider,
            model=model,
            database_url=database_url,
            learning_schema=learning_schema,
            textbook_sources=textbook_sources
        )
        print(f"READY provider={provider} db={database_url} schema={learning_schema}", flush=True)
    except Exception as e:
        print(f"ERROR: Failed to initialize agent: {e}", file=sys.stderr)
        import traceback
        traceback.print_exc(file=sys.stderr)
        sys.exit(1)
    
    # Process commands
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        
        try:
            command = json.loads(line)
            cmd = command.get("command")
            
            if cmd == "ready":
                response = {
                    "success": True,
                    "provider": provider,
                    "model": agent.llm.model_name if hasattr(agent.llm, 'model_name') else "unknown",
                    "database": database_url,
                    "learning_schema": learning_schema,
                    "textbook_sources": list(textbook_sources.keys())
                }
            
            elif cmd == "process":
                concept = command.get("concept", "")
                explanation = command.get("explanation", "")
                textbook_sources = command.get("textbook_sources", [])
                thread_id = command.get("thread_id", "default")
                
                result = agent.process_explanation(
                    concept, 
                    explanation, 
                    textbook_sources,
                    thread_id
                )
                
                response = result
            
            elif cmd == "get_mastered":
                thread_id = command.get("thread_id", "default")
                mastered = agent.learning_storage.get_mastered_concepts(thread_id)
                response = {
                    "success": True,
                    "mastered_concepts": mastered
                }
            
            elif cmd == "reset":
                response = {"success": True, "message": "Session reset"}
            
            else:
                response = {"success": False, "error": f"Unknown command: {cmd}"}
            
            print(json.dumps(response), flush=True)
        
        except json.JSONDecodeError as e:
            error_response = {"success": False, "error": f"Invalid JSON: {e}"}
            print(json.dumps(error_response), flush=True)
        
        except Exception as e:
            error_response = {"success": False, "error": f"Error: {e}"}
            print(json.dumps(error_response), flush=True)
            import traceback
            traceback.print_exc(file=sys.stderr)


if __name__ == "__main__":
    main()
