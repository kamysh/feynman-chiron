use anyhow::{Context, Result};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};
use hf_hub::{split_id, HFClientSync};
use tokenizers::Tokenizer;

/// Default embedding model, used when a schema has no `embedding_config' yet
/// (fresh `chiron-ingest create-schema`/`ingest`) and no model was requested.
pub const DEFAULT_MODEL_ID: &str = "sentence-transformers/all-MiniLM-L6-v2";

pub struct Embedder {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
    model_id: String,
    dim: usize,
}

impl Embedder {
    /// Load MODEL_ID (any BERT-family sentence-embedding model on Hugging
    /// Face Hub compatible with `candle_transformers::models::bert`) —
    /// e.g. `DEFAULT_MODEL_ID` — from the local `hf-hub` cache, downloading
    /// it first if not yet cached.
    pub fn new(model_id: &str) -> Result<Self> {
        let device = Device::Cpu;

        let client = HFClientSync::new().context("Failed to create HuggingFace Hub client")?;
        let (owner, name) = split_id(model_id);
        let repo = client.model(owner, name);

        let config_path = repo.download_file().filename("config.json").send()
            .context("Failed to download config.json")?;
        let tokenizer_path = repo.download_file().filename("tokenizer.json").send()
            .context("Failed to download tokenizer.json")?;
        let weights_path = repo.download_file().filename("model.safetensors").send()
            .context("Failed to download model.safetensors")?;

        let config: Config = serde_json::from_reader(
            std::fs::File::open(&config_path)
                .context("Failed to open config.json")?
        ).context("Failed to parse config.json")?;
        let dim = config.hidden_size;

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("Failed to load tokenizer: {}", e))?;

        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[&weights_path], DType::F32, &device)
                .context("Failed to load model weights")?
        };

        let model = BertModel::load(vb, &config)
            .context("Failed to load BERT model")?;

        Ok(Self { model, tokenizer, device, model_id: model_id.to_string(), dim })
    }

    /// Hugging Face Hub id this embedder was loaded from, e.g.
    /// "sentence-transformers/all-MiniLM-L6-v2".
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// Output vector dimension, read from the model's own config
    /// (`hidden_size`) rather than assumed.
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Embed a text string, returning an L2-normalised vector of `self.dim()` dimensions.
    pub fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let encoding = self.tokenizer
            .encode(text, true)
            .map_err(|e| anyhow::anyhow!("Tokenization failed: {}", e))?;

        let ids   = encoding.get_ids();
        let mask  = encoding.get_attention_mask();
        let types = vec![0u32; ids.len()];

        let token_ids      = Tensor::new(ids, &self.device)?.unsqueeze(0)?;
        let attention_mask = Tensor::new(mask, &self.device)?.unsqueeze(0)?;
        let token_type_ids = Tensor::new(types.as_slice(), &self.device)?.unsqueeze(0)?;

        // Forward pass → [1, seq_len, hidden_size]
        let output = self.model.forward(&token_ids, &token_type_ids, Some(&attention_mask))?;

        // Mean pooling weighted by attention mask
        let mask_f = attention_mask.to_dtype(DType::F32)?;                // [1, seq_len]
        let mask_3d = mask_f.unsqueeze(2)?.broadcast_as(output.shape())?; // [1, seq_len, hidden_size]
        let sum   = (output * mask_3d)?.sum(1)?;                          // [1, hidden_size]
        let count = mask_f.sum(1)?.unsqueeze(1)?;                         // [1, 1]
        let mean  = sum.broadcast_div(&count)?;                           // [1, hidden_size]

        // L2 normalise
        let norm       = mean.sqr()?.sum(1)?.sqrt()?.unsqueeze(1)?;  // [1, 1]
        let normalised = mean.broadcast_div(&norm)?.squeeze(0)?;     // [hidden_size]

        let vec = normalised.to_vec1::<f32>()?;
        debug_assert_eq!(vec.len(), self.dim);
        Ok(vec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires network + model download"]
    fn embed_returns_384_dims() {
        let embedder = Embedder::new(DEFAULT_MODEL_ID).unwrap();
        let v = embedder.embed("Groups are a fundamental structure in algebra.").unwrap();
        assert_eq!(v.len(), 384);
        assert_eq!(embedder.dim(), 384);
    }

    #[test]
    #[ignore = "requires network + model download"]
    fn embed_is_unit_normalised() {
        let embedder = Embedder::new(DEFAULT_MODEL_ID).unwrap();
        let v = embedder.embed("Hello world").unwrap();
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "norm was {}", norm);
    }
}
