//! The reranker, jinaai/jina-reranker-v3: given a query and the entries a
//! search found best, it reads them all in one prompt and scores how well
//! each answers the query, from -1 to 1, so the best comes first and a query
//! nothing answers finds nothing ([`crate::embed`] says from what score an
//! entry counts).
//!
//! It's Qwen3 ([`crate::qwen3`]) with a small projector: the prompt marks
//! the end of each passage and of the query with a token of its own, the
//! projector turns the model's state at each mark into a vector, and a
//! passage's score is how alike its vector is to the query's. Adapted from
//! the model's own `modeling.py`.

use crate::qwen3::{self, Qwen3};
use anyhow::{Context, Result, anyhow, bail};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use std::fs;
use std::path::Path;
use tokenizers::Tokenizer;

/// The tokens that mark a passage's end, and the query's.
const PASSAGE_MARK: &str = "<|embed_token|>";
const QUERY_MARK: &str = "<|rerank_token|>";

/// The most tokens of a passage, and of the query, the model reads: the
/// rest is left off.
const PASSAGE_TOKENS: usize = 512;
const QUERY_TOKENS: usize = 512;

/// The most tokens of passages one prompt holds. More go into prompts of
/// their own, read one after another, which keeps attention's memory down.
const BLOCK_TOKENS: usize = 4096;

/// The reranker, loaded.
pub struct Reranker {
    model: Qwen3,
    /// The projector, on the CPU in float32: a layer to half the model's
    /// width, ReLU, then one to 512, without biases.
    project_in: Tensor,
    project_out: Tensor,
    tokenizer: Tokenizer,
    passage_mark: u32,
    query_mark: u32,
}

impl Reranker {
    /// The model whose files are in `dir`.
    pub fn load(dir: &Path, device: &Device, dtype: DType) -> Result<Reranker> {
        let config: qwen3::Config =
            serde_json::from_str(&fs::read_to_string(dir.join("config.json"))?)?;
        let weights = dir.join("model.safetensors");
        // SAFETY: the file is only read, and nothing else writes it once
        // it's in place: a download goes to a file beside it, then is moved.
        let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[weights], dtype, device)? };
        let model = Qwen3::load(vb.pp("model"), &config)?;
        let half = config.hidden_size / 2;
        let project_in = vb
            .get((half, config.hidden_size), "projector.0.weight")?
            .to_dtype(DType::F32)?
            .to_device(&Device::Cpu)?;
        let project_out = vb
            .get((512, half), "projector.2.weight")?
            .to_dtype(DType::F32)?
            .to_device(&Device::Cpu)?;
        let tokenizer =
            Tokenizer::from_file(dir.join("tokenizer.json")).map_err(|err| anyhow!(err))?;
        let mark = |token| {
            tokenizer
                .token_to_id(token)
                .with_context(|| format!("the tokenizer has no {token}"))
        };
        Ok(Reranker {
            passage_mark: mark(PASSAGE_MARK)?,
            query_mark: mark(QUERY_MARK)?,
            model,
            project_in,
            project_out,
            tokenizer,
        })
    }

    /// How well each of `passages` answers `query`, in their order, from -1
    /// to 1. Passages too many for one prompt go into several, each with the
    /// query; the query's vector is then their vectors averaged, weighted by
    /// how well each prompt's best passage answered it, as the model's own
    /// code does.
    pub fn scores(&self, query: &str, passages: &[&str]) -> Result<Vec<f32>> {
        if passages.is_empty() {
            return Ok(Vec::new());
        }
        let query = self.cut(&unmarked(query), QUERY_TOKENS)?;
        let mut cut = Vec::with_capacity(passages.len());
        for passage in passages {
            cut.push(self.cut(&unmarked(passage), PASSAGE_TOKENS)?);
        }
        let mut blocks: Vec<Vec<&(String, usize)>> = vec![Vec::new()];
        let mut filled = 0;
        for passage in &cut {
            let block = blocks.last_mut().unwrap();
            if !block.is_empty() && filled + passage.1 > BLOCK_TOKENS {
                blocks.push(Vec::new());
                filled = 0;
            }
            blocks.last_mut().unwrap().push(passage);
            filled += passage.1;
        }
        let mut passage_vectors = Vec::new();
        let mut query_vectors = Vec::new();
        let mut weights = Vec::new();
        for block in &blocks {
            let texts: Vec<&str> = block.iter().map(|(text, _)| text.as_str()).collect();
            let (passages, query) = self.read(&query.0, &texts)?;
            let best = cosines(&query, &passages)?.into_iter().fold(-1.0, f32::max);
            weights.push((1.0 + best) / 2.0);
            passage_vectors.push(passages);
            query_vectors.push(query);
        }
        let total: f32 = weights.iter().sum();
        let mut query = (query_vectors[0].zeros_like())?;
        for (vector, weight) in query_vectors.iter().zip(&weights) {
            query = (query + (vector * f64::from(weight / total))?)?;
        }
        cosines(&query, &Tensor::cat(&passage_vectors, 0)?)
    }

    /// `text`, cut to its first `tokens` tokens, and how many tokens it is.
    fn cut(&self, text: &str, tokens: usize) -> Result<(String, usize)> {
        let ids = self
            .tokenizer
            .encode(text, false)
            .map_err(|err| anyhow!(err))?
            .get_ids()
            .to_vec();
        if ids.len() <= tokens {
            return Ok((text.to_string(), ids.len()));
        }
        let text = self
            .tokenizer
            .decode(&ids[..tokens], false)
            .map_err(|err| anyhow!(err))?;
        Ok((text, tokens))
    }

    /// Reads `passages` with `query` in one prompt, and gives the projector's
    /// vector at each passage's mark, `(passages, 512)`, and at the query's,
    /// `(512)`.
    fn read(&self, query: &str, passages: &[&str]) -> Result<(Tensor, Tensor)> {
        let prompt = prompt(query, passages);
        let encoding = self
            .tokenizer
            .encode(prompt, false)
            .map_err(|err| anyhow!(err))?;
        let ids = encoding.get_ids();
        let at = |mark: u32| -> Vec<u32> {
            (0..ids.len() as u32)
                .filter(|&n| ids[n as usize] == mark)
                .collect()
        };
        let (passage_marks, query_marks) = (at(self.passage_mark), at(self.query_mark));
        if passage_marks.len() != passages.len() || query_marks.len() != 1 {
            bail!(
                "the prompt has {} passage marks for {} passages, and {} query marks",
                passage_marks.len(),
                passages.len(),
                query_marks.len()
            );
        }
        let device = self.model.device();
        let input = Tensor::new(ids, device)?.unsqueeze(0)?;
        let hidden = self.model.forward(&input)?.squeeze(0)?;
        let pick = |marks: Vec<u32>| -> Result<Tensor> {
            let rows = hidden.index_select(&Tensor::new(marks, device)?, 0)?;
            self.project(&rows.to_device(&Device::Cpu)?)
        };
        Ok((pick(passage_marks)?, pick(query_marks)?.squeeze(0)?))
    }

    /// The projector over `rows`, `(n, hidden)`, on the CPU in float32.
    fn project(&self, rows: &Tensor) -> Result<Tensor> {
        let inner = rows.matmul(&self.project_in.t()?)?.relu()?;
        Ok(inner.matmul(&self.project_out.t()?)?)
    }
}

/// Each row of `passages`, `(n, dim)`, held against `query`, `(dim)`: the
/// cosine of the angle between them.
fn cosines(query: &Tensor, passages: &Tensor) -> Result<Vec<f32>> {
    let query = qwen3::unit_rows(&query.unsqueeze(0)?)?;
    let passages = qwen3::unit_rows(passages)?;
    Ok(passages.matmul(&query.t()?)?.squeeze(1)?.to_vec1()?)
}

/// `text` without the marks the prompt uses, so a passage can't pass for
/// two.
fn unmarked(text: &str) -> String {
    text.replace(PASSAGE_MARK, "").replace(QUERY_MARK, "")
}

/// The prompt the model was trained on: what it's to do, each passage
/// numbered and marked at its end, then the query, marked at its end.
fn prompt(query: &str, passages: &[&str]) -> String {
    let mut prompt = String::from(
        "<|im_start|>system\n\
         You are a search relevance expert who can determine a ranking of the passages based on \
         how relevant they are to the query. If the query is a question, how relevant a passage \
         is depends on how well it answers the question. If not, try to analyze the intent of the \
         query and assess how well each passage satisfies the intent. If an instruction is \
         provided, you should follow the instruction when determining the ranking.\
         <|im_end|>\n<|im_start|>user\n",
    );
    prompt.push_str(&format!(
        "I will provide you with {} passages, each indicated by a numerical identifier. \
         Rank the passages based on their relevance to query: {query}\n",
        passages.len()
    ));
    let numbered: Vec<String> = passages
        .iter()
        .enumerate()
        .map(|(n, passage)| format!("<passage id=\"{n}\">\n{passage}{PASSAGE_MARK}\n</passage>"))
        .collect();
    prompt.push_str(&numbered.join("\n"));
    prompt.push('\n');
    prompt.push_str(&format!("<query>\n{query}{QUERY_MARK}\n</query>"));
    prompt.push_str("<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n");
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_numbers_and_marks_each_passage_then_the_query() {
        let prompt = prompt("db?", &["Postgres runs first", "Deploys on Tuesdays"]);
        assert!(prompt.starts_with("<|im_start|>system\nYou are a search relevance expert"));
        assert!(prompt.contains(
            "I will provide you with 2 passages, each indicated by a numerical identifier. \
             Rank the passages based on their relevance to query: db?\n\
             <passage id=\"0\">\nPostgres runs first<|embed_token|>\n</passage>\n\
             <passage id=\"1\">\nDeploys on Tuesdays<|embed_token|>\n</passage>\n\
             <query>\ndb?<|rerank_token|>\n</query><|im_end|>\n<|im_start|>assistant\n\
             <think>\n\n</think>\n\n"
        ));
        assert!(prompt.ends_with("</think>\n\n"));
        assert_eq!(prompt.matches("<|embed_token|>").count(), 2);
    }

    #[test]
    fn a_passage_can_t_bring_marks_of_its_own() {
        assert_eq!(unmarked("a<|embed_token|>b<|rerank_token|>c"), "abc");
    }

    #[test]
    fn a_passage_s_score_is_its_cosine_with_the_query() {
        let cpu = Device::Cpu;
        let query = Tensor::new(&[1f32, 0.0], &cpu).unwrap();
        let passages = Tensor::new(&[[2f32, 0.0], [0.0, 3.0], [-1.0, 0.0]], &cpu).unwrap();
        assert_eq!(cosines(&query, &passages).unwrap(), vec![1.0, 0.0, -1.0]);
    }
}
