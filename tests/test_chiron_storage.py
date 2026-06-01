import sys
import pytest
from unittest.mock import patch, MagicMock


def test_embeddings_available_without_openai_key(monkeypatch):
    """Embeddings must work without an OpenAI key — uses local model now."""
    monkeypatch.delenv("OPENAI_API_KEY", raising=False)
    sys.modules.pop('chiron_storage', None)
    try:
        import chiron_storage
        assert chiron_storage.HAVE_EMBEDDINGS is True, \
            "HAVE_EMBEDDINGS should be True — local model requires no API key"
    finally:
        sys.modules.pop('chiron_storage', None)


def test_embedding_dimension_is_384():
    """EMBEDDING_DIM constant must be 384 (all-MiniLM-L6-v2 output size)."""
    import chiron_storage
    assert hasattr(chiron_storage, 'EMBEDDING_DIM'), \
        "EMBEDDING_DIM constant not defined in chiron_storage"
    assert chiron_storage.EMBEDDING_DIM == 384


def test_storage_uses_local_embedding_model():
    """ChironStorage must use HuggingFaceEmbeddings with all-MiniLM-L6-v2, not OpenAI."""
    import chiron_storage
    assert hasattr(chiron_storage, 'HuggingFaceEmbeddings'), \
        "chiron_storage does not import HuggingFaceEmbeddings"

    from chiron_storage import ChironStorage
    with patch('chiron_storage.psycopg2.connect'), \
         patch.object(ChironStorage, '_init_schema'), \
         patch('chiron_storage.HuggingFaceEmbeddings') as mock_hf:
        mock_hf.return_value = MagicMock()
        storage = ChironStorage("postgresql://fake/db")

    assert mock_hf.called, "HuggingFaceEmbeddings() was never called"
    assert mock_hf.call_args.kwargs.get('model_name') == 'all-MiniLM-L6-v2', \
        f"Wrong model: {mock_hf.call_args}"
