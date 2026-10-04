//! Qwen3, the transformer both of memory's models are built on: the one
//! that turns texts into vectors ([`crate::embed`]) and the one that reads a
//! query with its best matches and says how well each answers it
//! ([`crate::rerank`]).
//!
//! Adapted from candle-transformers' `qwen3` (MIT or Apache-2.0), for
//! reading a text whole rather than generating one: it keeps no cache, so
//! one loaded model reads any number of texts, from any thread, and it reads
//! a batch of them at once, each padded at its end. Attention is causal, so
//! a text's tokens never see the padding after them.

use candle_core::{Context, D, DType, Device, Module, Result, Tensor};
use candle_nn::{Embedding, Linear, RmsNorm, VarBuilder};
use serde::Deserialize;

/// The shape of a model, from its `config.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub vocab_size: usize,
    pub hidden_size: usize,
    pub intermediate_size: usize,
    pub num_hidden_layers: usize,
    pub num_attention_heads: usize,
    pub num_key_value_heads: usize,
    pub head_dim: usize,
    pub rope_theta: f64,
    pub rms_norm_eps: f64,
}

/// A loaded model: the token embeddings, the layers and the final norm.
pub struct Qwen3 {
    embed_tokens: Embedding,
    layers: Vec<Layer>,
    norm: RmsNorm,
    /// The frequencies rotary embeddings turn positions by, one for each
    /// pair of a head's dimensions.
    inv_freq: Tensor,
    device: Device,
    dtype: DType,
}

impl Qwen3 {
    /// The model whose weights `vb` holds, named as Qwen3's own are:
    /// `embed_tokens`, `layers.N…` and `norm`.
    pub fn load(vb: VarBuilder, config: &Config) -> Result<Qwen3> {
        let embed_tokens =
            candle_nn::embedding(config.vocab_size, config.hidden_size, vb.pp("embed_tokens"))?;
        let layers = (0..config.num_hidden_layers)
            .map(|n| Layer::load(config, vb.pp(format!("layers.{n}"))))
            .collect::<Result<_>>()?;
        let norm = candle_nn::rms_norm(config.hidden_size, config.rms_norm_eps, vb.pp("norm"))?;
        let dim = config.head_dim;
        let inv_freq: Vec<f32> = (0..dim)
            .step_by(2)
            .map(|i| 1.0 / config.rope_theta.powf(i as f64 / dim as f64) as f32)
            .collect();
        let inv_freq = Tensor::from_vec(inv_freq, (1, dim / 2), vb.device())?;
        Ok(Qwen3 {
            embed_tokens,
            layers,
            norm,
            inv_freq,
            device: vb.device().clone(),
            dtype: vb.dtype(),
        })
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    /// The last layer's state at every token of `ids`, `(batch, tokens)`,
    /// after the final norm: `(batch, tokens, hidden)`, in `f32`.
    pub fn forward(&self, ids: &Tensor) -> Result<Tensor> {
        let (_, length) = ids.dims2()?;
        let positions = Tensor::arange(0u32, length as u32, &self.device)?
            .to_dtype(DType::F32)?
            .reshape((length, 1))?;
        let freqs = positions.matmul(&self.inv_freq)?;
        let rotary = Rotary {
            cos: freqs.cos()?.to_dtype(self.dtype)?,
            sin: freqs.sin()?.to_dtype(self.dtype)?,
        };
        let mask = if fused(&self.device, length) {
            None
        } else {
            Some(causal_mask(length, &self.device)?.to_dtype(self.dtype)?)
        };
        let mut hidden = self.embed_tokens.forward(ids)?;
        for layer in &self.layers {
            hidden = layer.forward(&hidden, &rotary, mask.as_ref())?;
        }
        self.norm.forward(&hidden)?.to_dtype(DType::F32)
    }
}

/// Whether attention over `length` tokens runs Candle's fused kernel: on a
/// Mac's GPU it's causal, each key and value head serves several query
/// heads, and the whole matrix of scores is never in memory. Not for 8
/// tokens or fewer, which Candle hands to a kernel made for generating one
/// token at a time, that isn't causal.
fn fused(device: &Device, length: usize) -> bool {
    device.is_metal() && length > 8
}

/// Each position's turn, for rotary embeddings.
struct Rotary {
    cos: Tensor,
    sin: Tensor,
}

/// What keeps each token from seeing those after it: nothing on and below
/// the diagonal, minus infinity above it.
fn causal_mask(length: usize, device: &Device) -> Result<Tensor> {
    let mask: Vec<f32> = (0..length)
        .flat_map(|i| (0..length).map(move |j| if j > i { f32::NEG_INFINITY } else { 0.0 }))
        .collect();
    Tensor::from_vec(mask, (length, length), device)
}

struct Layer {
    attention: Attention,
    mlp: Mlp,
    input_norm: RmsNorm,
    post_attention_norm: RmsNorm,
}

impl Layer {
    fn load(config: &Config, vb: VarBuilder) -> Result<Layer> {
        let norm = |name| candle_nn::rms_norm(config.hidden_size, config.rms_norm_eps, vb.pp(name));
        Ok(Layer {
            attention: Attention::load(config, vb.pp("self_attn"))?,
            mlp: Mlp::load(config, vb.pp("mlp"))?,
            input_norm: norm("input_layernorm")?,
            post_attention_norm: norm("post_attention_layernorm")?,
        })
    }

    fn forward(&self, x: &Tensor, rotary: &Rotary, mask: Option<&Tensor>) -> Result<Tensor> {
        let attended = self
            .attention
            .forward(&self.input_norm.forward(x)?, rotary, mask)?;
        let x = (x + attended)?;
        let fed = self.mlp.forward(&self.post_attention_norm.forward(&x)?)?;
        x + fed
    }
}

struct Attention {
    q_proj: Linear,
    k_proj: Linear,
    v_proj: Linear,
    o_proj: Linear,
    q_norm: RmsNorm,
    k_norm: RmsNorm,
    heads: usize,
    kv_heads: usize,
    head_dim: usize,
}

impl Attention {
    fn load(config: &Config, vb: VarBuilder) -> Result<Attention> {
        let (hidden, heads, kv_heads, head_dim) = (
            config.hidden_size,
            config.num_attention_heads,
            config.num_key_value_heads,
            config.head_dim,
        );
        let linear = |from, to, name| candle_nn::linear_no_bias(from, to, vb.pp(name));
        let norm = |name| candle_nn::rms_norm(head_dim, config.rms_norm_eps, vb.pp(name));
        Ok(Attention {
            q_proj: linear(hidden, heads * head_dim, "q_proj")?,
            k_proj: linear(hidden, kv_heads * head_dim, "k_proj")?,
            v_proj: linear(hidden, kv_heads * head_dim, "v_proj")?,
            o_proj: linear(heads * head_dim, hidden, "o_proj")?,
            q_norm: norm("q_norm")?,
            k_norm: norm("k_norm")?,
            heads,
            kv_heads,
            head_dim,
        })
    }

    fn forward(&self, x: &Tensor, rotary: &Rotary, mask: Option<&Tensor>) -> Result<Tensor> {
        let (batch, length, _) = x.dims3()?;
        let heads = |projected: Tensor, count: usize| {
            projected
                .reshape((batch, length, count, self.head_dim))?
                .transpose(1, 2)
        };
        let q = heads(self.q_proj.forward(x)?, self.heads)?;
        let k = heads(self.k_proj.forward(x)?, self.kv_heads)?;
        let v = heads(self.v_proj.forward(x)?, self.kv_heads)?;
        // Each head's queries and keys are normed on their own.
        let q = self.q_norm.forward(&q.flatten(0, 2)?)?.reshape((
            batch,
            self.heads,
            length,
            self.head_dim,
        ))?;
        let k = self.k_norm.forward(&k.flatten(0, 2)?)?.reshape((
            batch,
            self.kv_heads,
            length,
            self.head_dim,
        ))?;
        let q = candle_nn::rotary_emb::rope(&q.contiguous()?, &rotary.cos, &rotary.sin)?;
        let k = candle_nn::rotary_emb::rope(&k.contiguous()?, &rotary.cos, &rotary.sin)?;
        let scale = 1.0 / (self.head_dim as f64).sqrt();
        let attended = if fused(q.device(), length) {
            candle_nn::ops::sdpa(&q, &k, &v.contiguous()?, None, true, scale as f32, 1.0)?
        } else {
            // Grouped-query attention: each key and value head serves
            // several query heads.
            let groups = self.heads / self.kv_heads;
            let k = repeat_kv(k, groups)?.contiguous()?;
            let v = repeat_kv(v.contiguous()?, groups)?.contiguous()?;
            let mask = mask.context("attention without the fused kernel needs the causal mask")?;
            let scores = (q.matmul(&k.transpose(2, 3)?)? * scale)?.broadcast_add(mask)?;
            candle_nn::ops::softmax_last_dim(&scores)?.matmul(&v)?
        };
        attended
            .transpose(1, 2)?
            .reshape((batch, length, self.heads * self.head_dim))?
            .apply(&self.o_proj)
    }
}

/// `x`, `(batch, kv_heads, tokens, dim)`, with each head repeated `times`
/// over, one after another.
fn repeat_kv(x: Tensor, times: usize) -> Result<Tensor> {
    if times == 1 {
        return Ok(x);
    }
    let (batch, kv_heads, length, dim) = x.dims4()?;
    Tensor::cat(&vec![&x; times], 2)?.reshape((batch, kv_heads * times, length, dim))
}

struct Mlp {
    gate_proj: Linear,
    up_proj: Linear,
    down_proj: Linear,
}

impl Mlp {
    fn load(config: &Config, vb: VarBuilder) -> Result<Mlp> {
        let (hidden, inner) = (config.hidden_size, config.intermediate_size);
        Ok(Mlp {
            gate_proj: candle_nn::linear_no_bias(hidden, inner, vb.pp("gate_proj"))?,
            up_proj: candle_nn::linear_no_bias(hidden, inner, vb.pp("up_proj"))?,
            down_proj: candle_nn::linear_no_bias(inner, hidden, vb.pp("down_proj"))?,
        })
    }

    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let gate = candle_nn::ops::silu(&self.gate_proj.forward(x)?)?;
        (gate * self.up_proj.forward(x)?)?.apply(&self.down_proj)
    }
}

/// Where the last token of each of `lengths` is in a batch padded at the
/// end, as indices into its `(batch × tokens)` rows.
pub fn last_tokens(lengths: &[usize], padded: usize, device: &Device) -> Result<Tensor> {
    let rows: Vec<u32> = lengths
        .iter()
        .enumerate()
        .map(|(row, length)| (row * padded + length.max(&1) - 1) as u32)
        .collect();
    Tensor::from_vec(rows, lengths.len(), device)
}

/// `x`, `(rows, dim)`, each row scaled to length one.
pub fn unit_rows(x: &Tensor) -> Result<Tensor> {
    let length = x.sqr()?.sum_keepdim(D::Minus1)?.sqrt()?;
    x.broadcast_div(&length)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A tiny model with made-up weights, to check the shapes and that a
    /// token's state doesn't depend on the padding after it.
    fn tiny() -> Qwen3 {
        let config = Config {
            vocab_size: 11,
            hidden_size: 8,
            intermediate_size: 12,
            num_hidden_layers: 2,
            num_attention_heads: 4,
            num_key_value_heads: 2,
            head_dim: 4,
            rope_theta: 10_000.0,
            rms_norm_eps: 1e-6,
        };
        let device = Device::Cpu;
        let mut weights = HashMap::new();
        let mut seed = 1u32;
        let mut random = |shape: &[usize]| {
            let count: usize = shape.iter().product();
            let values: Vec<f32> = (0..count)
                .map(|_| {
                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                    (seed >> 16) as f32 / 65_536.0 - 0.5
                })
                .collect();
            Tensor::from_vec(values, shape, &device).unwrap()
        };
        weights.insert("embed_tokens.weight".to_string(), random(&[11, 8]));
        weights.insert("norm.weight".to_string(), random(&[8]));
        for n in 0..2 {
            let mut put = |name: &str, shape: &[usize]| {
                weights.insert(format!("layers.{n}.{name}.weight"), random(shape));
            };
            put("input_layernorm", &[8]);
            put("post_attention_layernorm", &[8]);
            put("self_attn.q_proj", &[16, 8]);
            put("self_attn.k_proj", &[8, 8]);
            put("self_attn.v_proj", &[8, 8]);
            put("self_attn.o_proj", &[8, 16]);
            put("self_attn.q_norm", &[4]);
            put("self_attn.k_norm", &[4]);
            put("mlp.gate_proj", &[12, 8]);
            put("mlp.up_proj", &[12, 8]);
            put("mlp.down_proj", &[8, 12]);
        }
        let vb = VarBuilder::from_tensors(weights, DType::F32, &device);
        Qwen3::load(vb, &config).unwrap()
    }

    #[test]
    fn padding_after_a_text_changes_nothing_of_it() {
        let model = tiny();
        let alone = Tensor::new(&[[3u32, 5, 7]], &Device::Cpu).unwrap();
        let padded = Tensor::new(&[[3u32, 5, 7, 0, 0], [1, 2, 3, 4, 5]], &Device::Cpu).unwrap();
        let alone = model.forward(&alone).unwrap();
        let padded = model.forward(&padded).unwrap();
        assert_eq!(padded.dims(), &[2, 5, 8]);
        let rows = padded.flatten(0, 1).unwrap();
        let last = last_tokens(&[3, 5], 5, &Device::Cpu).unwrap();
        let picked = rows.index_select(&last, 0).unwrap();
        let first: Vec<f32> = picked.get(0).unwrap().to_vec1().unwrap();
        let want: Vec<f32> = alone.get(0).unwrap().get(2).unwrap().to_vec1().unwrap();
        for (a, b) in first.iter().zip(&want) {
            assert!((a - b).abs() < 1e-5, "{first:?} {want:?}");
        }
    }

    #[test]
    fn rows_are_scaled_to_length_one() {
        let x = Tensor::new(&[[3f32, 4.0], [0.0, 2.0]], &Device::Cpu).unwrap();
        let unit: Vec<Vec<f32>> = unit_rows(&x).unwrap().to_vec2().unwrap();
        assert_eq!(unit, vec![vec![0.6, 0.8], vec![0.0, 1.0]]);
    }
}
