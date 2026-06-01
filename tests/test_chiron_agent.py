import io
import sys
import pytest
from unittest.mock import patch, MagicMock


def make_agent():
    """Build a ChironAgent with all external deps mocked."""
    from chiron_agent import ChironAgent
    return ChironAgent(
        provider='anthropic',
        database_url='postgresql://fake/chiron',
        learning_schema='learning',
    )


@patch('chiron_agent.ChironStorage')
@patch('chiron_agent.ChatAnthropic')
def test_default_anthropic_model_is_current(mock_anthropic, mock_storage):
    """ChironAgent should default to claude-sonnet-4-6, not an old model."""
    make_agent()
    assert mock_anthropic.call_args.kwargs['model'] == 'claude-sonnet-4-6', \
        f"Expected claude-sonnet-4-6, got {mock_anthropic.call_args.kwargs.get('model')!r}"


@patch('chiron_agent.ChironStorage')
@patch('chiron_agent.ChatAnthropic')
def test_retrieve_handles_dict_textbook_sources(mock_anthropic, mock_storage):
    """retrieve_from_textbooks should not raise ValueError when sources is a dict."""
    agent = make_agent()
    agent.textbook_connections = {}  # no pre-built connections

    state = {
        "messages": [],
        "concept": "groups",
        "textbook_context": "",
        "explanations": ["A group is a set with a binary operation"],
        "gaps": [],
        "stage": "initial",
        "mastered_concepts": {},
        "textbook_names": [],
        # JSON-decoded dict (what Emacs json-encode produces for an alist)
        "textbook_sources": {"dummit-foote": "math"},
    }

    # Current code does `for source_name, source_url in textbook_sources:`
    # which unpacks dict keys (strings) into two vars → ValueError
    result = agent.retrieve_from_textbooks(state)
    assert result["stage"] == "analyze"


@patch('chiron_agent.ChironAgent')
def test_ready_signal_on_stdout(mock_agent_cls, monkeypatch, capsys):
    """main() should write the READY line to stdout, not stderr."""
    monkeypatch.setenv("CHIRON_DATABASE_URL", "postgresql://fake/chiron")
    monkeypatch.setenv("CHIRON_LEARNING_SCHEMA", "learning")
    monkeypatch.setattr('sys.stdin', io.StringIO(''))  # EOF immediately

    from chiron_agent import main
    main()

    captured = capsys.readouterr()
    assert 'READY' in captured.out, \
        f"READY not found in stdout.\nstdout={captured.out!r}\nstderr={captured.err!r}"
