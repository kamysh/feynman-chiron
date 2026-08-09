use anyhow::{Context, Result};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};
use hf_hub::{split_id, HFClientSync};
use tokenizers::Tokenizer;

const MODEL_ID: &str = "sentence-transformers/all-MiniLM-L6-v2";
const EMBEDDING_DIM: usize = 384;

pub struct Embedder {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
}

impl Embedder {
    pub fn new() -> Result<Self> {
        let device = Device::Cpu;

        let client = HFClientSync::new().context("Failed to create HuggingFace Hub client")?;
        let (owner, name) = split_id(MODEL_ID);
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

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("Failed to load tokenizer: {}", e))?;

        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[&weights_path], DType::F32, &device)
                .context("Failed to load model weights")?
        };

        let model = BertModel::load(vb, &config)
            .context("Failed to load BERT model")?;

        Ok(Self { model, tokenizer, device })
    }

    /// Embed a text string, returning a 384-dimensional L2-normalised vector.
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
        let mask_f = attention_mask.to_dtype(DType::F32)?;           // [1, seq_len]
        let mask_3d = mask_f.unsqueeze(2)?.broadcast_as(output.shape())?; // [1, seq_len, 384]
        let sum   = (output * mask_3d)?.sum(1)?;                     // [1, 384]
        let count = mask_f.sum(1)?.unsqueeze(1)?;                    // [1, 1]
        let mean  = sum.broadcast_div(&count)?;                      // [1, 384]

        // L2 normalise
        let norm       = mean.sqr()?.sum(1)?.sqrt()?.unsqueeze(1)?;  // [1, 1]
        let normalised = mean.broadcast_div(&norm)?.squeeze(0)?;     // [384]

        let vec = normalised.to_vec1::<f32>()?;
        debug_assert_eq!(vec.len(), EMBEDDING_DIM);
        Ok(vec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires network + model download"]
    fn embed_returns_384_dims() {
        let embedder = Embedder::new().unwrap();
        let v = embedder.embed("Groups are a fundamental structure in algebra.").unwrap();
        assert_eq!(v.len(), 384);
    }

    #[test]
    #[ignore = "requires network + model download"]
    fn embed_is_unit_normalised() {
        let embedder = Embedder::new().unwrap();
        let v = embedder.embed("Hello world").unwrap();
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "norm was {}", norm);
    }
}
