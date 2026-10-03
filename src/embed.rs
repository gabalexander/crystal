//! Embeddings for memory's search: what an entry says as a vector, from a
//! small model run on this machine, BAAI/bge-small-en-v1.5 through Candle,
//! so a search finds an entry by what it means as well as by its words:
//! "db" finds "the database", "flaky" finds "fails now and then".
//! [`crate::memory`] keeps the vectors beside the entries and merges the two
//! rankings.
//!
//! It's off unless `embeddings = true` is under `[memory]` in the config.
//! The model isn't part of crystal: `crystal memory embed` downloads it
//! once, at a pinned revision, checks each file against its SHA-256, and
//! keeps it in crystal's cache directory. Until it's there, a search goes
//! by words alone.

use crate::config::{Config, MemorySettings};
use anyhow::{Context, Result, anyhow, bail};
use candle_core::{DType, Device, IndexOp, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config as BertConfig};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use tokenizers::{PaddingParams, Tokenizer, TruncationParams};

/// The model, as Hugging Face names it.
pub const MODEL: &str = "BAAI/bge-small-en-v1.5";

/// The revision of it crystal downloads: a commit, so the files never
/// change under the hashes below.
const REVISION: &str = "5c38ec7c405ec4b44b94cc5a9bb96e735b38267a";

/// The files the model is, with their sizes and SHA-256 hashes at
/// [`REVISION`].
const FILES: [ModelFile; 3] = [
    ModelFile {
        name: "config.json",
        size: 743,
        sha256: "094f8e891b932f2000c92cfc663bac4c62069f5d8af5b5278c4306aef3084750",
    },
    ModelFile {
        name: "tokenizer.json",
        size: 711_396,
        sha256: "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66",
    },
    ModelFile {
        name: "model.safetensors",
        size: 133_466_304,
        sha256: "3c9f31665447c8911517620762200d2245a2518d6e7208acc78cd9db317e21ad",
    },
];

/// The most tokens the model reads of a text: the rest is left off.
const MAX_TOKENS: usize = 512;

/// How many texts go through the model at once.
const BATCH: usize = 32;

/// How alike a query and an entry have to be, by this model, for the entry
/// to be worth ranking at all. Short notes all score between about 0.45 and
/// 0.75 against a short query, and an unrelated one can score above a real
/// match, so this only leaves out what's plainly about something else: the
/// ranking, merged with bm25's, does the rest.
const MIN_SIMILARITY: f32 = 0.5;

/// How far below the best match, by this model, another may score and
/// still count. On crystal's notes, the right entry led the next by 0.01 to
/// 0.13, and what came after was rarely within 0.05 of it.
const NEAR_BEST: f32 = 0.05;

struct ModelFile {
    name: &'static str,
    size: u64,
    sha256: &'static str,
}

/// What turns texts into vectors: the model, or a stand-in in tests.
pub trait Embed {
    /// Its name, which each vector is kept under: vectors from two models
    /// can't be compared.
    fn model(&self) -> &str;

    /// Each text's vector, of length one, so two vectors' dot product is
    /// how alike they are.
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;

    /// How alike a query and a passage have to be to count as a match:
    /// below it, they're no more alike than any two sentences are.
    fn min_similarity(&self) -> f32;

    /// How far below the best match another may score and still count:
    /// the model's scores are close together, so a match well behind the
    /// best is one in name only.
    fn near_best(&self) -> f32;
}

/// The model, loaded.
pub struct Embedder {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
}

impl Embedder {
    /// The model whose files are in `dir`.
    pub fn load(dir: &Path) -> Result<Embedder> {
        let device = Device::Cpu;
        let config = fs::read_to_string(dir.join("config.json"))?;
        let config: BertConfig = serde_json::from_str(&config)?;
        let mut tokenizer =
            Tokenizer::from_file(dir.join("tokenizer.json")).map_err(|err| anyhow!(err))?;
        tokenizer.with_padding(Some(PaddingParams::default()));
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: MAX_TOKENS,
                ..TruncationParams::default()
            }))
            .map_err(|err| anyhow!(err))?;
        let weights = dir.join("model.safetensors");
        // SAFETY: the file is only read, and nothing else writes it once
        // it's in place: a download goes to a file beside it, then is moved.
        let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[weights], DType::F32, &device)? };
        let model = BertModel::load(vb, &config)?;
        Ok(Embedder {
            model,
            tokenizer,
            device,
        })
    }
}

impl Embed for Embedder {
    fn model(&self) -> &str {
        MODEL
    }

    fn min_similarity(&self) -> f32 {
        MIN_SIMILARITY
    }

    fn near_best(&self) -> f32 {
        NEAR_BEST
    }

    /// Queries go as they are, without the instruction the model card
    /// offers for them: on crystal's short notes, it ranked worse.
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut vectors = Vec::with_capacity(texts.len());
        for batch in texts.chunks(BATCH) {
            let encodings = self
                .tokenizer
                .encode_batch(batch.to_vec(), true)
                .map_err(|err| anyhow!(err))?;
            let tensor = |part: fn(&tokenizers::Encoding) -> &[u32]| -> Result<Tensor> {
                let rows = encodings
                    .iter()
                    .map(|encoding| Tensor::new(part(encoding), &self.device))
                    .collect::<candle_core::Result<Vec<_>>>()?;
                Ok(Tensor::stack(&rows, 0)?)
            };
            let ids = tensor(tokenizers::Encoding::get_ids)?;
            let mask = tensor(tokenizers::Encoding::get_attention_mask)?;
            let types = ids.zeros_like()?;
            let hidden = self.model.forward(&ids, &types, Some(&mask))?;
            // The model's own pooling: the first token's state, [CLS].
            let first = hidden.i((.., 0))?;
            let length = first.sqr()?.sum_keepdim(1)?.sqrt()?;
            let unit = first.broadcast_div(&length)?;
            vectors.extend(unit.to_vec2::<f32>()?);
        }
        Ok(vectors)
    }
}

/// The model, once a process has loaded it.
static LOADED: Mutex<Option<Arc<Embedder>>> = Mutex::new(None);

/// The model, loaded once in each process and kept, when the config says to
/// search with it and it's been downloaded. With the config saying not to,
/// a process that had it loaded lets it go.
pub fn shared(settings: &MemorySettings) -> Option<Arc<Embedder>> {
    let mut loaded = LOADED.lock().unwrap();
    if !settings.embeddings {
        *loaded = None;
        return None;
    }
    if let Some(embedder) = &*loaded {
        return Some(embedder.clone());
    }
    let dir = model_dir()?;
    if !is_downloaded(&dir) {
        return None;
    }
    match Embedder::load(&dir) {
        Ok(embedder) => {
            let embedder = Arc::new(embedder);
            *loaded = Some(embedder.clone());
            Some(embedder)
        }
        Err(err) => {
            eprintln!("crystal: couldn't load {MODEL}: {err:#}");
            None
        }
    }
}

/// Whether this process has the model loaded.
pub fn is_loaded() -> bool {
    LOADED.lock().unwrap().is_some()
}

/// Lets the model go, when this process has it and the config now says
/// not to search with it.
pub fn let_go_unless(settings: &MemorySettings) {
    if !settings.embeddings {
        LOADED.lock().unwrap().take();
    }
}

/// How the model stands, as the settings view shows it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Status {
    /// How much of the model is on disk, in bytes, downloaded or on its
    /// way, and how much it is in all.
    pub on_disk: u64,
    pub size: u64,
    /// Whether the daemon has it loaded.
    pub loaded: bool,
    /// What the daemon is doing to get it ready, while it does.
    pub preparing: Option<String>,
    /// Why getting it ready last failed.
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

/// How much of the model is in `dir`, in bytes: the files there, and those
/// on their way, each counted up to its size.
pub fn on_disk(dir: &Path) -> u64 {
    FILES
        .iter()
        .map(|file| {
            let size = |name: &str| fs::metadata(dir.join(name)).map_or(0, |meta| meta.len());
            let done = size(file.name);
            let coming = size(&format!("{}.part", file.name));
            done.max(coming).min(file.size)
        })
        .sum()
}

/// How big the model is, all its files together, in bytes.
pub fn size() -> u64 {
    FILES.iter().map(|file| file.size).sum()
}

/// [`shared`], by the config file as it is now.
pub fn shared_now() -> Option<Arc<Embedder>> {
    shared(&Config::load().ok()?.memory)
}

/// What a search is given of [`shared`]'s answer.
pub fn as_embed(embedder: &Option<Arc<Embedder>>) -> Option<&dyn Embed> {
    embedder.as_deref().map(|embedder| embedder as &dyn Embed)
}

/// Where the model is kept: in crystal's cache directory, in a directory
/// named after the revision, so another never mixes with it.
pub fn model_dir() -> Option<PathBuf> {
    let cache = cache_dir(std::env::var_os("XDG_CACHE_HOME"), std::env::var_os("HOME"))?;
    Some(model_dir_in(&cache))
}

fn model_dir_in(cache: &Path) -> PathBuf {
    let name = MODEL.rsplit('/').next().unwrap_or(MODEL);
    cache
        .join("crystal")
        .join("models")
        .join(format!("{name}-{}", &REVISION[..8]))
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

/// Whether every file of the model is in `dir`, each the size it should be.
/// Its hash was checked as it was downloaded.
pub fn is_downloaded(dir: &Path) -> bool {
    FILES
        .iter()
        .all(|file| fs::metadata(dir.join(file.name)).is_ok_and(|meta| meta.len() == file.size))
}

/// How big the model is, all its files together, in megabytes.
pub fn size_mb() -> u64 {
    size() / 1_000_000
}

/// Downloads the model into [`model_dir`], each file it doesn't have yet,
/// with `curl`, showing how it goes when `progress` says to. Each file goes
/// beside its place first and is moved there only once its hash is right.
pub fn download(progress: bool) -> Result<PathBuf> {
    let dir = model_dir().context("can't tell where to keep the model: HOME isn't set")?;
    fs::create_dir_all(&dir).with_context(|| format!("couldn't make {}", dir.display()))?;
    for file in &FILES {
        let path = dir.join(file.name);
        if fs::metadata(&path).is_ok_and(|meta| meta.len() == file.size) {
            continue;
        }
        let partial = dir.join(format!("{}.part", file.name));
        let url = format!(
            "https://huggingface.co/{MODEL}/resolve/{REVISION}/{}",
            file.name
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
            .context("couldn't run curl, which downloads the model")?;
        if !status.success() {
            let _ = fs::remove_file(&partial);
            bail!("couldn't download {url}");
        }
        let sha256 = sha256_of(&partial)?;
        if sha256 != file.sha256 {
            let _ = fs::remove_file(&partial);
            bail!(
                "{} came with the wrong SHA-256: {sha256}, not {}",
                file.name,
                file.sha256
            );
        }
        fs::rename(&partial, &path)?;
    }
    Ok(dir)
}

/// The SHA-256 of the file at `path`, in hex.
fn sha256_of(path: &Path) -> Result<String> {
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

    #[test]
    fn the_model_is_kept_in_crystal_s_cache_by_its_revision() {
        let cache = cache_dir(None, Some("/home/ann".into())).unwrap();
        assert_eq!(cache, PathBuf::from("/home/ann/.cache"));
        let cache = cache_dir(Some("/xdg".into()), Some("/home/ann".into())).unwrap();
        assert_eq!(
            model_dir_in(&cache),
            PathBuf::from("/xdg/crystal/models/bge-small-en-v1.5-5c38ec7c")
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
    fn a_model_with_a_file_missing_or_short_isn_t_downloaded() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_downloaded(dir.path()));
        for file in &FILES[..2] {
            fs::write(dir.path().join(file.name), vec![0; file.size as usize]).unwrap();
        }
        fs::write(dir.path().join("model.safetensors"), "short").unwrap();
        assert!(!is_downloaded(dir.path()));
        assert_eq!(size_mb(), 134);
    }

    #[test]
    fn what_s_on_disk_counts_the_files_on_their_way() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(on_disk(dir.path()), 0);
        fs::write(dir.path().join("config.json"), vec![0; 743]).unwrap();
        fs::write(dir.path().join("model.safetensors.part"), vec![0; 1000]).unwrap();
        assert_eq!(on_disk(dir.path()), 1743);
        let status = Status {
            on_disk: size(),
            size: size(),
            ..Status::default()
        };
        assert!(status.is_downloaded());
        assert!(!Status::default().is_downloaded());
    }

    /// Runs the real model, when `crystal memory embed` has downloaded it:
    /// `cargo test -- --ignored the_real_model`.
    #[test]
    #[ignore]
    fn the_real_model_finds_what_means_the_same() {
        let dir = model_dir().unwrap();
        assert!(is_downloaded(&dir), "run `crystal memory embed` first");
        let model = Embedder::load(&dir).unwrap();
        let passages = [
            "Postgres has to be running before the ledger tests",
            "Deploys go out on Tuesdays",
            "The refund test fails now and then under load",
            "Fees are kept in cents; never store a float",
            "make e2e runs the browser tests; they take about 4 minutes",
            "Releases are built by the release workflow for macOS and Linux with musl",
        ];
        let vectors = model.embed(&passages).unwrap();
        assert_eq!(vectors[0].len(), 384);
        let length: f32 = vectors[0].iter().map(|x| x * x).sum();
        assert!((length - 1.0).abs() < 1e-4);
        let queries = [
            ("start the db", 0),
            ("release day", 1),
            ("flaky tests", 2),
            ("money rounding", 3),
            ("how long do end to end tests take", 4),
            ("static linux binary build", 5),
        ];
        for (query, want) in queries {
            let asked = &model.embed(&[query]).unwrap()[0];
            let scores: Vec<f32> = vectors
                .iter()
                .map(|vector| vector.iter().zip(asked).map(|(a, b)| a * b).sum())
                .collect();
            eprintln!("{query}: {scores:?}");
            let best = scores
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .unwrap()
                .0;
            assert_eq!(best, want, "{query}: {scores:?}");
        }
    }
}
