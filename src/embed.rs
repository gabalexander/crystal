//! Search by meaning, for memory: two models run on this machine through
//! Candle. One turns texts into vectors, jinaai/jina-embeddings-v5-text-small
//! with its retrieval adapter, so a search finds an entry by what it means
//! as well as by its words: "db" finds "the database", "flaky" finds "fails
//! now and then". The other, jinaai/jina-reranker-v3 ([`crate::rerank`]),
//! reads a query with the entries found best and says how well each answers
//! it, so the right one comes first and a search about something the memory
//! doesn't hold finds nothing. [`crate::memory`] keeps the vectors beside the
//! entries and merges the rankings.
//!
//! Both are Qwen3 ([`crate::qwen3`]), about 1.2 GB each, run on a Mac's GPU
//! (Metal) where there is one and otherwise on the CPU. They're on unless
//! `embeddings = false` is under `[memory]` in the config; `rerank = false`
//! leaves the second out. They aren't part of crystal: the daemon, or
//! `crystal memory embed`, downloads them once, at pinned revisions, checks
//! each file against its SHA-256, and keeps them in crystal's cache
//! directory. Until they're there, a search goes by words alone.
//!
//! Both models are licensed CC BY-NC 4.0: for use that isn't commercial.

use crate::config::{Config, MemorySettings};
use crate::output::errln;
use crate::qwen3::{self, Qwen3};
use crate::rerank::Reranker;
use anyhow::{Context, Result, anyhow, bail};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, PoisonError};
use tokenizers::{Tokenizer, TruncationParams};

/// The model that turns texts into vectors, as Hugging Face names it, which
/// each vector is kept under.
pub const MODEL: &str = "jinaai/jina-embeddings-v5-text-small";

/// The two models, as crystal downloads them: each at a commit, so its files
/// never change under their hashes.
pub const EMBEDDER: Spec = Spec {
    repo: MODEL,
    revision: "dd76d535f5447ca3897a9c893fb1e612ead98192",
    files: &[
        ModelFile {
            name: "config.json",
            size: 991,
            sha256: "1af1e1269488c83d8b2332e42099f0d2201d687fbe074d1ed096c6201f283546",
        },
        ModelFile {
            name: "tokenizer.json",
            size: 11_422_654,
            sha256: "aeb13307a71acd8fe81861d94ad54ab689df773318809eed3cbe794b4492dae4",
        },
        ModelFile {
            name: "model.safetensors",
            size: 1_192_133_208,
            sha256: "045fa75ff963a528cda2589fb1ca0a9ad848b53511780ed4f08f6fe10f6167c3",
        },
        ModelFile {
            name: "adapters/retrieval/adapter_config.json",
            size: 883,
            sha256: "f37c5d6dd368e2675e54e01b685252d4d44eed042c773a48f56ebe1565cd0320",
        },
        ModelFile {
            name: "adapters/retrieval/adapter_model.safetensors",
            size: 40_420_176,
            sha256: "2bc6ab71895eb04664e4d995ee29e1620603f3a3fc4dfc573bc2383dfc85bb94",
        },
    ],
};

pub const RERANKER: Spec = Spec {
    repo: "jinaai/jina-reranker-v3",
    revision: "d7d7e73b6ea138ced340b83865931b5dfb6c97aa",
    files: &[
        ModelFile {
            name: "config.json",
            size: 828,
            sha256: "625aea6b08e6062e334c4a5009af01ba50140d2d3994084f6f1b05c67dadf98a",
        },
        ModelFile {
            name: "tokenizer.json",
            size: 11_423_225,
            sha256: "4e95945ab0cef486709f760b81efcc7a6e75747f9165d13ead29159737455803",
        },
        ModelFile {
            name: "model.safetensors",
            size: 1_193_708_120,
            sha256: "200d852626fd18ce3f3a97c55b689f1f842031f1488055b4cdcfa274924b8f3d",
        },
    ],
};

const SPECS: [&Spec; 2] = [&EMBEDDER, &RERANKER];

/// The most tokens the model reads of a text: the rest is left off.
const MAX_TOKENS: usize = 512;

/// How many texts go through the model at once.
const BATCH: usize = 16;

/// What goes ahead of a query and of an entry, as the model was trained.
const QUERY: &str = "Query: ";
const PASSAGE: &str = "Document: ";

/// How alike a query and an entry have to be, by this model, for the entry
/// to be worth ranking at all. On crystal's notes no score told a match
/// from the rest (a query nothing answered scored up to 0.38, a real match
/// as little as 0.28), so nothing is left out for its score alone: the
/// window below the best, and the reranker, do that.
const MIN_SIMILARITY: f32 = 0.0;

/// How far below the best match, by this model, another may score and
/// still count: 0.08 ranked crystal's notes best, out of 0.03 to 0.12.
const NEAR_BEST: f32 = 0.08;

/// The score the reranker gives the best entry for a query something
/// answers: on crystal's notes, the best entry for a query nothing answered
/// scored at most 0.014, and for one something did, at least 0.057. Below
/// it, a search finds nothing.
const ANSWERS_FROM: f32 = 0.03;

/// The score below which an entry the reranker read is left out, once
/// something answers the query: an entry that answers in part scores less
/// than the best, often below zero, and on crystal's notes this kept every
/// one while leaving out two in three of the rest.
const KEPT_FROM: f32 = -0.05;

/// How alike two entries' vectors have to be, by this model, for the second
/// to be the first said again in other words, whatever the reranker makes of
/// it: on crystal's notes, every pair this alike said the same thing.
const SAME_FROM: f32 = 0.92;

/// How alike two entries' vectors have to be for the reranker to be asked
/// whether the second says what the first does. Below it, on crystal's notes,
/// different lessons about the same thing scored as high with the reranker as
/// the same lesson said again.
const ALIKE_FROM: f32 = 0.87;

/// The score the reranker, reading an entry as the query, gives one said
/// before at least [`ALIKE_FROM`] alike for the two to say the same thing: on
/// crystal's notes, different lessons that alike scored 0.36 at most, the same
/// said again mostly 0.4 to 0.75.
const SAME_RERANKED_FROM: f32 = 0.40;

/// How alike two entries' vectors have to be, by this model, to be near
/// one another: for `crystal memory reconcile` to ask whether one shows the
/// other no longer holds, and for one being remembered to say which it's
/// near. On crystal's own notes, once those that say the same were merged,
/// every pair where one corrected the other (a prompt that pages now, a flag
/// that survives a restart now) was at least 0.80 alike, and only 5 pairs
/// were as alike as [`ALIKE_FROM`]. The reranker tells nothing here: an
/// entry and the one it corrects are about the same thing, which is what it
/// scores.
const NEAR_FROM: f32 = 0.80;

/// A model's files at a revision.
pub struct Spec {
    pub repo: &'static str,
    revision: &'static str,
    files: &'static [ModelFile],
}

struct ModelFile {
    name: &'static str,
    size: u64,
    sha256: &'static str,
}

impl Spec {
    /// The model's name without its owner.
    fn name(&self) -> &str {
        self.repo.rsplit('/').next().unwrap_or(self.repo)
    }

    /// Where it's kept in `root`: a directory named after the revision, so
    /// another never mixes with it.
    fn dir(&self, root: &Path) -> PathBuf {
        root.join(format!("{}-{}", self.name(), &self.revision[..8]))
    }

    fn size(&self) -> u64 {
        self.files.iter().map(|file| file.size).sum()
    }
}

/// What a search asks of the models: the real ones, or a stand-in in tests.
pub trait Embed {
    /// The name each vector is kept under: vectors from two models can't be
    /// compared.
    fn model(&self) -> &str;

    /// Each entry's vector, of length one, so two vectors' dot product is
    /// how alike they are.
    fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;

    /// A query's vector, to hold against the entries'.
    fn embed_query(&self, text: &str) -> Result<Vec<f32>>;

    /// How alike a query and a passage have to be to count as a match:
    /// below it, they're no more alike than any two sentences are.
    fn min_similarity(&self) -> f32;

    /// How far below the best match another may score and still count:
    /// the model's scores are close together, so a match well behind the
    /// best is one in name only.
    fn near_best(&self) -> f32;

    /// How well each of `passages` answers `query`, in their order, by a
    /// model that reads them together; `None` with no such model.
    fn rerank(&self, _query: &str, _passages: &[&str]) -> Result<Option<Vec<f32>>> {
        Ok(None)
    }

    /// The score from [`Embed::rerank`] the best passage has to reach for
    /// any to count as answering the query.
    fn answers_from(&self) -> f32 {
        ANSWERS_FROM
    }

    /// The score from [`Embed::rerank`] below which a passage is left out,
    /// once one answers the query.
    fn kept_from(&self) -> f32 {
        KEPT_FROM
    }

    /// How alike two entries' vectors have to be for the second to say what
    /// the first does, the reranker or not.
    fn same_from(&self) -> f32 {
        SAME_FROM
    }

    /// How alike two entries' vectors have to be for the reranker to be
    /// asked whether the second says what the first does.
    fn alike_from(&self) -> f32 {
        ALIKE_FROM
    }

    /// The score from [`Embed::rerank`], an entry read as the query, one at
    /// least [`Embed::alike_from`] alike has to reach to say the same thing.
    fn same_reranked_from(&self) -> f32 {
        SAME_RERANKED_FROM
    }

    /// How alike two entries' vectors have to be to be near one another,
    /// for one to be asked whether it shows the other no longer holds, and
    /// for one being remembered to say it's near the other.
    fn near_from(&self) -> f32 {
        NEAR_FROM
    }
}

/// Where the models run: on a Mac's GPU in bfloat16, the weights as they
/// come, unless it can't be had (or `CRYSTAL_MODELS_ON_CPU` is set);
/// otherwise on the CPU in float32, which Candle multiplies there.
fn device() -> (Device, DType) {
    #[cfg(target_os = "macos")]
    if std::env::var_os("CRYSTAL_MODELS_ON_CPU").is_none() {
        match Device::new_metal(0) {
            Ok(device) => return (device, DType::BF16),
            Err(err) => errln!("crystal: no GPU for memory's models, so the CPU: {err}"),
        }
    }
    (Device::Cpu, DType::F32)
}

/// The model that turns texts into vectors, loaded.
pub struct Embedder {
    model: Qwen3,
    tokenizer: Tokenizer,
}

impl Embedder {
    /// The model whose files are in `dir`, its retrieval adapter folded into
    /// its weights.
    fn load(dir: &Path, device: &Device, dtype: DType) -> Result<Embedder> {
        let config: qwen3::Config =
            serde_json::from_str(&fs::read_to_string(dir.join("config.json"))?)?;
        let adapter = dir.join("adapters/retrieval");
        let lora: LoraConfig =
            serde_json::from_str(&fs::read_to_string(adapter.join("adapter_config.json"))?)?;
        let weights = with_adapter(
            &dir.join("model.safetensors"),
            &adapter.join("adapter_model.safetensors"),
            lora.lora_alpha / lora.r,
        )?;
        let weights = weights
            .into_iter()
            .map(|(name, weight)| Ok((name, weight.to_dtype(dtype)?.to_device(device)?)))
            .collect::<Result<HashMap<_, _>>>()?;
        let model = Qwen3::load(VarBuilder::from_tensors(weights, dtype, device), &config)?;
        let mut tokenizer =
            Tokenizer::from_file(dir.join("tokenizer.json")).map_err(|err| anyhow!(err))?;
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: MAX_TOKENS,
                ..TruncationParams::default()
            }))
            .map_err(|err| anyhow!(err))?;
        Ok(Embedder { model, tokenizer })
    }

    /// Each text's vector: its last token's state (the end-of-text token the
    /// tokenizer adds), scaled to length one. Texts of about the same length
    /// go through together, so little of a batch is padding.
    fn vectors(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let encodings = self
            .tokenizer
            .encode_batch(texts, true)
            .map_err(|err| anyhow!(err))?;
        let mut order: Vec<usize> = (0..encodings.len()).collect();
        order.sort_by_key(|&n| encodings[n].get_ids().len());
        let mut vectors = vec![Vec::new(); encodings.len()];
        let device = self.model.device();
        for batch in order.chunks(BATCH) {
            let lengths: Vec<usize> = batch
                .iter()
                .map(|&n| encodings[n].get_ids().len())
                .collect();
            let padded = lengths.iter().copied().max().unwrap_or(1).max(1);
            let mut ids = vec![0u32; batch.len() * padded];
            for (row, &n) in batch.iter().enumerate() {
                let tokens = encodings[n].get_ids();
                ids[row * padded..row * padded + tokens.len()].copy_from_slice(tokens);
            }
            let ids = Tensor::from_vec(ids, (batch.len(), padded), device)?;
            let hidden = self.model.forward(&ids)?.flatten(0, 1)?;
            let last = qwen3::last_tokens(&lengths, padded, device)?;
            let unit = qwen3::unit_rows(&hidden.index_select(&last, 0)?)?;
            for (&n, vector) in batch.iter().zip(unit.to_vec2::<f32>()?) {
                vectors[n] = vector;
            }
        }
        Ok(vectors)
    }
}

/// What a LoRA adapter's `adapter_config.json` says of its scale.
#[derive(Deserialize)]
struct LoraConfig {
    r: f64,
    lora_alpha: f64,
}

/// The weights in the safetensors file `base`, on the CPU, with the LoRA
/// adapter in `adapter` folded in: each weight it adapts plus `scale` times
/// B·A, worked out in float32.
fn with_adapter(base: &Path, adapter: &Path, scale: f64) -> Result<HashMap<String, Tensor>> {
    let cpu = Device::Cpu;
    let mut weights = candle_core::safetensors::load(base, &cpu)?;
    let lora = candle_core::safetensors::load(adapter, &cpu)?;
    for (name, a) in &lora {
        let Some(adapted) = name
            .strip_prefix("base_model.model.")
            .and_then(|name| name.strip_suffix(".lora_A.weight"))
        else {
            continue;
        };
        let b = lora
            .get(&format!("base_model.model.{adapted}.lora_B.weight"))
            .with_context(|| format!("the adapter has no B for {adapted}"))?;
        let key = format!("{adapted}.weight");
        let weight = weights
            .get(&key)
            .with_context(|| format!("the adapter adapts {adapted}, which the model hasn't"))?;
        let delta = (b.to_dtype(DType::F32)?.matmul(&a.to_dtype(DType::F32)?)? * scale)?;
        let merged = (weight.to_dtype(DType::F32)? + delta)?;
        weights.insert(key, merged);
    }
    Ok(weights)
}

/// Both models, loaded: the one that turns texts into vectors, and the
/// reranker, unless the config leaves it out.
pub struct Models {
    embedder: Embedder,
    reranker: Option<Reranker>,
    /// Held while either model runs: Candle on a Mac's GPU gives wrong
    /// answers to threads that run models at once, and the GPU would run
    /// them one after another anyway.
    lane: Mutex<()>,
}

impl Models {
    /// The models whose files are under `root`.
    pub fn load(root: &Path, rerank: bool) -> Result<Models> {
        let (device, dtype) = device();
        let embedder = Embedder::load(&EMBEDDER.dir(root), &device, dtype)
            .with_context(|| format!("couldn't load {MODEL}"))?;
        let reranker = if rerank {
            Some(
                Reranker::load(&RERANKER.dir(root), &device, dtype)
                    .with_context(|| format!("couldn't load {}", RERANKER.repo))?,
            )
        } else {
            None
        };
        Ok(Models {
            embedder,
            reranker,
            lane: Mutex::new(()),
        })
    }

    fn has_reranker(&self) -> bool {
        self.reranker.is_some()
    }
}

impl Embed for Models {
    fn model(&self) -> &str {
        MODEL
    }

    fn min_similarity(&self) -> f32 {
        MIN_SIMILARITY
    }

    fn near_best(&self) -> f32 {
        NEAR_BEST
    }

    fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let _lane = self.lane.lock().unwrap_or_else(PoisonError::into_inner);
        let texts: Vec<String> = texts
            .iter()
            .map(|text| format!("{PASSAGE}{text}"))
            .collect();
        let vectors = self.embedder.vectors(texts.clone())?;
        again_unless_numbers(vectors, &texts, |text| {
            self.embedder.vectors(vec![text.to_string()])
        })
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let _lane = self.lane.lock().unwrap_or_else(PoisonError::into_inner);
        let texts = vec![format!("{QUERY}{text}")];
        let vectors = self.embedder.vectors(texts.clone())?;
        let vector = again_unless_numbers(vectors, &texts, |text| {
            self.embedder.vectors(vec![text.to_string()])
        })?
        .pop()
        .context("the model gave no vector")?;
        if !is_numbers(&vector) {
            bail!("the model gave a vector that isn't numbers");
        }
        Ok(vector)
    }

    fn rerank(&self, query: &str, passages: &[&str]) -> Result<Option<Vec<f32>>> {
        let _lane = self.lane.lock().unwrap_or_else(PoisonError::into_inner);
        match &self.reranker {
            Some(reranker) => Ok(Some(reranker.scores(query, passages)?)),
            None => Ok(None),
        }
    }
}

/// Whether every number of `vector` is one: the model has given NaN, every
/// number of one vector, though the same text never did again, on the GPU
/// or the CPU, alone or in any batch.
pub fn is_numbers(vector: &[f32]) -> bool {
    vector.iter().all(|x| x.is_finite())
}

/// `vectors`, of `texts`, with each that isn't numbers ([`is_numbers`])
/// made again from its text alone by `again`, once: what went wrong once
/// hasn't been seen twice. One still not numbers is left as it is, for
/// whoever asked to leave out.
fn again_unless_numbers(
    mut vectors: Vec<Vec<f32>>,
    texts: &[String],
    again: impl Fn(&str) -> Result<Vec<Vec<f32>>>,
) -> Result<Vec<Vec<f32>>> {
    for (vector, text) in vectors.iter_mut().zip(texts) {
        if !is_numbers(vector) {
            errln!("crystal: the model gave a vector that isn't numbers; making it again");
            if let Some(remade) = again(text)?.pop() {
                *vector = remade;
            }
        }
    }
    Ok(vectors)
}

/// The models, once a process has loaded them.
static LOADED: Mutex<Option<Arc<Models>>> = Mutex::new(None);

/// The models, loaded once in each process and kept, when the config says
/// to search with them and they've been downloaded. With the config saying
/// not to, a process that had them loaded lets them go; with it leaving the
/// reranker out, or putting it back, they're loaded again to match.
pub fn shared(settings: &MemorySettings) -> Option<Arc<Models>> {
    let mut loaded = LOADED.lock().unwrap();
    if !settings.embeddings {
        *loaded = None;
        return None;
    }
    if let Some(models) = &*loaded {
        if models.has_reranker() == settings.rerank {
            return Some(models.clone());
        }
        *loaded = None;
    }
    let root = models_dir()?;
    if !is_downloaded(&root) {
        return None;
    }
    match Models::load(&root, settings.rerank) {
        Ok(models) => {
            let models = Arc::new(models);
            *loaded = Some(models.clone());
            Some(models)
        }
        Err(err) => {
            errln!("crystal: couldn't load memory's models: {err:#}");
            None
        }
    }
}

/// Whether this process has the models loaded.
pub fn is_loaded() -> bool {
    LOADED.lock().unwrap().is_some()
}

/// Lets the models go, when this process has them and the config now says
/// not to search with them.
pub fn let_go_unless(settings: &MemorySettings) {
    if !settings.embeddings {
        LOADED.lock().unwrap().take();
    }
}

/// How the models stand, as the settings view shows them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[cfg_attr(test, schemars(rename = "EmbeddingStatus"))]
pub struct Status {
    /// How much of the models is on disk, in bytes, downloaded or on its
    /// way, and how much they are in all.
    pub on_disk: u64,
    pub size: u64,
    /// Whether the daemon has them loaded.
    pub loaded: bool,
    /// What the daemon is doing to get them ready, while it does.
    pub preparing: Option<String>,
    /// Why getting them ready last failed.
    pub failed: Option<String>,
    /// How many entries every project has, and how many of them have their
    /// vector from the model.
    pub entries: usize,
    pub embedded: usize,
}

impl Status {
    pub fn is_downloaded(&self) -> bool {
        self.size > 0 && self.on_disk >= self.size
    }
}

/// How much of the models is under `root`, in bytes: the files there, and
/// those on their way, each counted up to its size.
pub fn on_disk(root: &Path) -> u64 {
    SPECS
        .iter()
        .flat_map(|spec| spec.files.iter().map(move |file| (spec.dir(root), file)))
        .map(|(dir, file)| {
            let size = |name: &str| fs::metadata(dir.join(name)).map_or(0, |meta| meta.len());
            let done = size(file.name);
            let coming = size(&format!("{}.part", file.name));
            done.max(coming).min(file.size)
        })
        .sum()
}

/// How big the models are, all their files together, in bytes.
pub fn size() -> u64 {
    SPECS.iter().map(|spec| spec.size()).sum()
}

/// [`shared`], by the config file as it is now.
pub fn shared_now() -> Option<Arc<Models>> {
    shared(&Config::load().ok()?.memory)
}

/// What a search is given of [`shared`]'s answer.
pub fn as_embed(models: &Option<Arc<Models>>) -> Option<&dyn Embed> {
    models.as_deref().map(|models| models as &dyn Embed)
}

/// Where the models are kept: crystal's models directory in its cache.
pub fn models_dir() -> Option<PathBuf> {
    let cache = cache_dir(std::env::var_os("XDG_CACHE_HOME"), std::env::var_os("HOME"))?;
    Some(cache.join("crystal").join("models"))
}

/// `$XDG_CACHE_HOME`, or else `~/.cache`.
fn cache_dir(
    from_env: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    match from_env {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => home.map(|home| PathBuf::from(home).join(".cache")),
    }
}

/// Whether every file of both models is under `root`, each the size it
/// should be. Its hash was checked as it was downloaded.
pub fn is_downloaded(root: &Path) -> bool {
    SPECS.iter().all(|spec| {
        let dir = spec.dir(root);
        spec.files
            .iter()
            .all(|file| fs::metadata(dir.join(file.name)).is_ok_and(|meta| meta.len() == file.size))
    })
}

/// How big the models are, all their files together, in megabytes.
pub fn size_mb() -> u64 {
    size() / 1_000_000
}

/// The models, as `crystal memory embed` names them.
pub fn names() -> String {
    format!("{} and {}", EMBEDDER.repo, RERANKER.repo)
}

/// Downloads both models under [`models_dir`], each file not there yet,
/// with `curl`, showing how it goes when `progress` says to. Each file goes
/// beside its place first and is moved there only once its hash is right.
pub fn download(progress: bool) -> Result<PathBuf> {
    let root = models_dir().context("can't tell where to keep the models: HOME isn't set")?;
    for spec in SPECS {
        let dir = spec.dir(&root);
        for file in spec.files {
            let path = dir.join(file.name);
            if fs::metadata(&path).is_ok_and(|meta| meta.len() == file.size) {
                continue;
            }
            let parent = path.parent().unwrap_or(&dir);
            fs::create_dir_all(parent)
                .with_context(|| format!("couldn't make {}", parent.display()))?;
            let partial = dir.join(format!("{}.part", file.name));
            let url = format!(
                "https://huggingface.co/{}/resolve/{}/{}",
                spec.repo, spec.revision, file.name
            );
            let mut curl = Command::new("curl");
            curl.args(["--fail", "--location", "--retry", "3", "--show-error"]);
            curl.arg(if progress {
                "--progress-bar"
            } else {
                "--silent"
            });
            curl.arg("--output").arg(&partial).arg(&url);
            let status = curl
                .status()
                .context("couldn't run curl, which downloads the models")?;
            if !status.success() {
                let _ = fs::remove_file(&partial);
                bail!("couldn't download {url}");
            }
            let sha256 = sha256_of(&partial)?;
            if sha256 != file.sha256 {
                let _ = fs::remove_file(&partial);
                bail!(
                    "{}'s {} came with the wrong SHA-256: {sha256}, not {}",
                    spec.repo,
                    file.name,
                    file.sha256
                );
            }
            fs::rename(&partial, &path)?;
        }
    }
    Ok(root)
}

/// The SHA-256 of the file at `path`, in hex.
pub fn sha256_of(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("couldn't read {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 16];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lays out `root` with every file of both models the right size, but
    /// for the files named in `short`, which are there but too small.
    fn lay_out(root: &Path, short: &[&str]) {
        for spec in SPECS {
            let dir = spec.dir(root);
            for file in spec.files {
                let path = dir.join(file.name);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                let size = if short.contains(&file.name) {
                    5
                } else {
                    file.size
                };
                File::create(&path).unwrap().set_len(size).unwrap();
            }
        }
    }

    #[test]
    fn the_models_are_kept_in_crystal_s_cache_by_their_revisions() {
        let cache = cache_dir(None, Some("/home/ann".into())).unwrap();
        assert_eq!(cache, PathBuf::from("/home/ann/.cache"));
        let cache = cache_dir(Some("/xdg".into()), Some("/home/ann".into())).unwrap();
        let root = cache.join("crystal").join("models");
        assert_eq!(
            EMBEDDER.dir(&root),
            PathBuf::from("/xdg/crystal/models/jina-embeddings-v5-text-small-dd76d535")
        );
        assert_eq!(
            RERANKER.dir(&root),
            PathBuf::from("/xdg/crystal/models/jina-reranker-v3-d7d7e73b")
        );
        assert_eq!(cache_dir(None, None), None);
    }

    #[test]
    fn a_file_s_hash_is_its_sha256_in_hex() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abc");
        fs::write(&path, "abc").unwrap();
        assert_eq!(
            sha256_of(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn models_with_a_file_missing_or_short_aren_t_downloaded() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_downloaded(dir.path()));
        lay_out(
            dir.path(),
            &["adapters/retrieval/adapter_model.safetensors"],
        );
        assert!(!is_downloaded(dir.path()));
        lay_out(dir.path(), &[]);
        assert!(is_downloaded(dir.path()));
        assert_eq!(size_mb(), 2449);
    }

    #[test]
    fn what_s_on_disk_counts_the_files_on_their_way() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(on_disk(dir.path()), 0);
        let embedder = EMBEDDER.dir(dir.path());
        fs::create_dir_all(&embedder).unwrap();
        fs::write(embedder.join("config.json"), vec![0; 991]).unwrap();
        fs::write(embedder.join("model.safetensors.part"), vec![0; 1000]).unwrap();
        let reranker = RERANKER.dir(dir.path());
        fs::create_dir_all(&reranker).unwrap();
        fs::write(reranker.join("config.json"), vec![0; 828]).unwrap();
        assert_eq!(on_disk(dir.path()), 991 + 1000 + 828);
        let status = Status {
            on_disk: size(),
            size: size(),
            ..Status::default()
        };
        assert!(status.is_downloaded());
        assert!(!Status::default().is_downloaded());
    }

    #[test]
    fn an_adapter_is_folded_into_the_weights_it_adapts() {
        let dir = tempfile::tempdir().unwrap();
        let cpu = Device::Cpu;
        let base = HashMap::from([
            (
                "layers.0.mlp.up_proj.weight".to_string(),
                Tensor::new(&[[1f32, 0.0], [0.0, 1.0]], &cpu).unwrap(),
            ),
            (
                "norm.weight".to_string(),
                Tensor::new(&[1f32, 1.0], &cpu).unwrap(),
            ),
        ]);
        let adapter = HashMap::from([
            (
                "base_model.model.layers.0.mlp.up_proj.lora_A.weight".to_string(),
                Tensor::new(&[[1f32, 2.0]], &cpu).unwrap(),
            ),
            (
                "base_model.model.layers.0.mlp.up_proj.lora_B.weight".to_string(),
                Tensor::new(&[[1f32], [0.0]], &cpu).unwrap(),
            ),
        ]);
        candle_core::safetensors::save(&base, dir.path().join("base.safetensors")).unwrap();
        candle_core::safetensors::save(&adapter, dir.path().join("lora.safetensors")).unwrap();
        let merged = with_adapter(
            &dir.path().join("base.safetensors"),
            &dir.path().join("lora.safetensors"),
            0.5,
        )
        .unwrap();
        let up: Vec<Vec<f32>> = merged["layers.0.mlp.up_proj.weight"].to_vec2().unwrap();
        assert_eq!(up, vec![vec![1.5, 1.0], vec![0.0, 1.0]]);
        let norm: Vec<f32> = merged["norm.weight"].to_vec1().unwrap();
        assert_eq!(norm, vec![1.0, 1.0]);
    }

    #[test]
    fn a_vector_that_isn_t_numbers_is_made_again_once() {
        let texts = ["one".to_string(), "two".to_string()];
        let vectors = vec![vec![1.0, 0.0], vec![f32::NAN, f32::NAN]];
        let again = |text: &str| -> Result<Vec<Vec<f32>>> {
            assert_eq!(text, "two", "only what isn't numbers is made again");
            Ok(vec![vec![0.0, 1.0]])
        };
        let made = again_unless_numbers(vectors.clone(), &texts, again).unwrap();
        assert_eq!(made, [vec![1.0, 0.0], vec![0.0, 1.0]]);
        // Still not numbers, it's left for whoever asked to leave out.
        let still = |_: &str| -> Result<Vec<Vec<f32>>> { Ok(vec![vec![f32::INFINITY, 0.0]]) };
        let made = again_unless_numbers(vectors, &texts, still).unwrap();
        assert!(is_numbers(&made[0]) && !is_numbers(&made[1]));
    }

    /// Runs the real models, once `crystal memory embed` has downloaded
    /// them: `cargo test -- --ignored the_real_models`.
    #[test]
    #[ignore]
    fn the_real_models_find_what_means_the_same_and_nothing_else() {
        let root = models_dir().unwrap();
        assert!(is_downloaded(&root), "run `crystal memory embed` first");
        let models = Models::load(&root, true).unwrap();
        let passages = [
            "Postgres has to be running before the ledger tests",
            "Deploys go out on Tuesdays",
            "The refund test fails now and then under load",
            "Fees are kept in cents; never store a float",
            "make e2e runs the browser tests; they take about 4 minutes",
            "Releases are built by the release workflow for macOS and Linux with musl",
        ];
        let vectors = models.embed_passages(&passages).unwrap();
        assert_eq!(vectors[0].len(), 1024);
        let length: f32 = vectors[0].iter().map(|x| x * x).sum();
        assert!((length - 1.0).abs() < 1e-3);
        // Whether the reranker holds the passage for an answer too: it
        // doesn't take "release day" for when deploys go out.
        let queries = [
            ("start the db", 0, true),
            ("release day", 1, false),
            ("flaky tests", 2, true),
            ("money rounding", 3, true),
            ("how long do end to end tests take", 4, true),
            ("static linux binary build", 5, true),
        ];
        for (query, want, answers) in queries {
            let asked = models.embed_query(query).unwrap();
            let scores: Vec<f32> = vectors
                .iter()
                .map(|vector| vector.iter().zip(&asked).map(|(a, b)| a * b).sum())
                .collect();
            let best = |scores: &[f32]| {
                scores
                    .iter()
                    .enumerate()
                    .max_by(|a, b| a.1.total_cmp(b.1))
                    .unwrap()
                    .0
            };
            assert_eq!(best(&scores), want, "{query}: {scores:?}");
            let reranked = models.rerank(query, &passages).unwrap().unwrap();
            errln!("{query}: {scores:?} {reranked:?}");
            if answers {
                assert_eq!(best(&reranked), want, "{query}: {reranked:?}");
                assert!(reranked[want] >= ANSWERS_FROM, "{query}: {reranked:?}");
            }
        }
        let nothing = models
            .rerank("kubernetes ingress certificate renewal", &passages)
            .unwrap()
            .unwrap();
        assert!(
            nothing.iter().all(|score| *score < ANSWERS_FROM),
            "{nothing:?}"
        );
    }

    /// Runs the real models from several threads at once, as the daemon's
    /// connections do: each gets what one thread alone does.
    #[test]
    #[ignore]
    fn the_real_models_answer_threads_running_them_at_once_alike() {
        let root = models_dir().unwrap();
        assert!(is_downloaded(&root), "run `crystal memory embed` first");
        let models = Arc::new(Models::load(&root, true).unwrap());
        let texts: Vec<String> = (0..24)
            .map(|n| {
                format!(
                    "note {n}: the ledger test fails under load {}",
                    "now and then ".repeat(n)
                )
            })
            .collect();
        let alone = |models: &Models, texts: &[String]| {
            let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
            let mut all = models.embed_passages(&texts).unwrap().concat();
            all.extend(models.embed_query("flaky ledger tests").unwrap());
            all.extend(
                models
                    .rerank("flaky ledger tests", &texts[..10])
                    .unwrap()
                    .unwrap(),
            );
            all
        };
        let want = alone(&models, &texts);
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let (models, texts) = (models.clone(), texts.clone());
                std::thread::spawn(move || {
                    (0..3).map(|_| alone(&models, &texts)).collect::<Vec<_>>()
                })
            })
            .collect();
        for thread in threads {
            for got in thread.join().unwrap() {
                let worst = got
                    .iter()
                    .zip(&want)
                    .map(|(a, b)| (a - b).abs())
                    .fold(0.0, f32::max);
                assert!(worst < 1e-3, "off by {worst}");
            }
        }
    }
}
