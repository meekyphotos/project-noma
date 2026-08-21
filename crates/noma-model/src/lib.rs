//! Finds, downloads, and unpacks the ASR model.
//!
//! Parakeet is half a gigabyte, so it does not ship with the binary. On first
//! run Noma fetches it from the sherpa-onnx model release, unpacks it into the
//! user's local data directory, and never touches the network again.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

mod download;

use download::ProgressReader;

/// Where the archives come from. These are the official sherpa-onnx releases.
const RELEASE_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models";

/// One downloadable speech model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelSpec {
    /// Stable id used in settings, and the directory name once unpacked.
    pub id: &'static str,
    /// Name for the UI.
    pub label: &'static str,
    /// One line on what it is good for.
    pub summary: &'static str,
    /// Roughly how big the download is, for the progress readout.
    pub download_bytes: u64,
    /// `model_type` sherpa-onnx needs to pick its decoder.
    pub model_type: &'static str,
}

impl ModelSpec {
    pub fn archive_url(&self) -> String {
        format!("{RELEASE_URL}/{}.tar.bz2", self.id)
    }
}

/// Files sherpa-onnx needs to build a recognizer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelPaths {
    pub encoder: PathBuf,
    pub decoder: PathBuf,
    pub joiner: PathBuf,
    pub tokens: PathBuf,
    pub model_type: String,
}

/// What the downloader is doing right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    /// Bytes received out of the expected total.
    Downloading { received: u64, total: u64 },
    /// Unpacking the archive; no byte count, bzip2 does not give us one cheaply.
    Extracting,
}

impl Progress {
    /// Percent complete, counting extraction as the last tenth of the job.
    pub fn percent(&self) -> f32 {
        match self {
            Progress::Downloading { received, total } if *total > 0 => {
                90.0 * (*received as f32 / *total as f32).clamp(0.0, 1.0)
            }
            Progress::Downloading { .. } => 0.0,
            Progress::Extracting => 95.0,
        }
    }

    /// A short line for the HUD.
    pub fn message(&self) -> String {
        match self {
            Progress::Downloading { received, total } => {
                format!(
                    "Downloading model {:.0}% ({} of {})",
                    self.percent() / 0.9,
                    human_bytes(*received),
                    human_bytes(*total)
                )
            }
            Progress::Extracting => "Unpacking model".to_string(),
        }
    }
}

/// Multilingual Parakeet: 25 European languages, punctuation and casing built in.
pub const PARAKEET_V3: ModelSpec = ModelSpec {
    id: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8",
    label: "Parakeet TDT 0.6B v3 (int8)",
    summary: "25 European languages, punctuated",
    download_bytes: 487_170_055,
    model_type: "nemo_transducer",
};

/// English-only Parakeet: same size, a little sharper on English.
pub const PARAKEET_V2: ModelSpec = ModelSpec {
    id: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8",
    label: "Parakeet TDT 0.6B v2 (int8)",
    summary: "English only, punctuated",
    download_bytes: 482_468_385,
    model_type: "nemo_transducer",
};

/// Every model Noma knows how to fetch.
pub fn catalog() -> &'static [ModelSpec] {
    &[PARAKEET_V3, PARAKEET_V2]
}

/// Look up a model by the id stored in settings.
pub fn find(id: &str) -> Option<ModelSpec> {
    catalog().iter().copied().find(|spec| spec.id == id)
}

/// `%LOCALAPPDATA%\noma\models` on Windows, `~/.local/share/noma/models` elsewhere.
pub fn models_dir() -> Result<PathBuf> {
    let base = dirs::data_local_dir().ok_or_else(|| anyhow!("no local data directory"))?;
    Ok(base.join("noma").join("models"))
}

/// The model's files if it is already unpacked on disk.
pub fn installed(spec: &ModelSpec) -> Option<ModelPaths> {
    let paths = layout(&models_dir().ok()?.join(spec.id), spec);
    paths.exists().then_some(paths)
}

/// Return the model's files, downloading and unpacking it first if needed.
///
/// `on_progress` is called from the calling thread as bytes arrive.
pub fn ensure(spec: &ModelSpec, on_progress: &mut dyn FnMut(Progress)) -> Result<ModelPaths> {
    if let Some(paths) = installed(spec) {
        return Ok(paths);
    }

    let root = models_dir()?;
    fs::create_dir_all(&root)
        .with_context(|| format!("create model directory {}", root.display()))?;

    // Download beside the target so a half-finished file is never mistaken for
    // an installed model, and so a crash leaves something we can just delete.
    let archive = root.join(format!("{}.tar.bz2.part", spec.id));
    download_archive(spec, &archive, on_progress)
        .inspect_err(|_| drop(fs::remove_file(&archive)))?;

    on_progress(Progress::Extracting);
    let unpacked = root.join(spec.id);
    if unpacked.exists() {
        // A previous run unpacked part of it; start clean.
        fs::remove_dir_all(&unpacked).with_context(|| format!("clear {}", unpacked.display()))?;
    }
    extract(&archive, &root).with_context(|| format!("unpack {}", archive.display()))?;
    let _ = fs::remove_file(&archive);

    let paths = layout(&unpacked, spec);
    if !paths.exists() {
        bail!(
            "{} unpacked without the expected model files in {}",
            spec.id,
            unpacked.display()
        );
    }
    Ok(paths)
}

/// Delete a downloaded model. Frees about half a gigabyte per model.
pub fn remove(spec: &ModelSpec) -> Result<()> {
    let dir = models_dir()?.join(spec.id);
    if dir.exists() {
        fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;
    }
    Ok(())
}

impl ModelPaths {
    fn exists(&self) -> bool {
        [&self.encoder, &self.decoder, &self.joiner, &self.tokens]
            .iter()
            .all(|path| path.is_file())
    }
}

/// The file names every sherpa-onnx NeMo transducer archive uses.
fn layout(dir: &Path, spec: &ModelSpec) -> ModelPaths {
    ModelPaths {
        encoder: dir.join("encoder.int8.onnx"),
        decoder: dir.join("decoder.int8.onnx"),
        joiner: dir.join("joiner.int8.onnx"),
        tokens: dir.join("tokens.txt"),
        model_type: spec.model_type.to_string(),
    }
}

fn download_archive(
    spec: &ModelSpec,
    destination: &Path,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<()> {
    let url = spec.archive_url();
    let response = ureq::get(&url)
        .call()
        .with_context(|| format!("fetch {url}"))?;

    let total = response
        .header("Content-Length")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(spec.download_bytes);

    let mut source = ProgressReader::new(response.into_reader(), total, on_progress);
    let mut file = BufWriter::new(
        File::create(destination)
            .with_context(|| format!("create {}", destination.display()))?,
    );
    std::io::copy(&mut source, &mut file).with_context(|| format!("download {url}"))?;
    file.flush().context("flush model archive")?;
    Ok(())
}

fn extract(archive: &Path, into: &Path) -> Result<()> {
    let file = File::open(archive).with_context(|| format!("open {}", archive.display()))?;
    let decompressed = bzip2::read::BzDecoder::new(std::io::BufReader::new(file));
    let mut tar = tar::Archive::new(decompressed);
    // tar-rs refuses entries that escape the destination, so a hostile archive
    // cannot write outside the models directory.
    tar.unpack(into).context("unpack archive")?;
    Ok(())
}

/// Sizes the way a download dialog would show them.
pub fn human_bytes(bytes: u64) -> String {
    const MB: f64 = 1_048_576.0;
    const GB: f64 = 1_073_741_824.0;
    let bytes = bytes as f64;
    if bytes >= GB {
        format!("{:.1} GB", bytes / GB)
    } else {
        format!("{:.0} MB", bytes / MB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_are_unique_and_findable() {
        for spec in catalog() {
            assert_eq!(find(spec.id).map(|found| found.id), Some(spec.id));
        }
        assert!(find("not-a-model").is_none());
    }

    #[test]
    fn archive_urls_point_at_the_release() {
        assert_eq!(
            PARAKEET_V3.archive_url(),
            "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/\
             sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2"
        );
    }

    #[test]
    fn layout_names_the_sherpa_files() {
        let paths = layout(Path::new("/models/x"), &PARAKEET_V3);
        assert!(paths.encoder.ends_with("encoder.int8.onnx"));
        assert!(paths.decoder.ends_with("decoder.int8.onnx"));
        assert!(paths.joiner.ends_with("joiner.int8.onnx"));
        assert!(paths.tokens.ends_with("tokens.txt"));
        assert_eq!(paths.model_type, "nemo_transducer");
    }

    #[test]
    fn missing_files_are_not_installed() {
        let paths = layout(Path::new("/definitely/not/here"), &PARAKEET_V3);
        assert!(!paths.exists());
    }

    #[test]
    fn progress_percent_never_reaches_a_hundred_before_extraction() {
        let half = Progress::Downloading {
            received: 50,
            total: 100,
        };
        assert!((half.percent() - 45.0).abs() < f32::EPSILON);
        let done = Progress::Downloading {
            received: 100,
            total: 100,
        };
        assert!((done.percent() - 90.0).abs() < f32::EPSILON);
        assert!(Progress::Extracting.percent() > done.percent());
    }

    #[test]
    fn progress_tolerates_an_unknown_total() {
        let unknown = Progress::Downloading {
            received: 10,
            total: 0,
        };
        assert_eq!(unknown.percent(), 0.0);
    }

    #[test]
    fn byte_sizes_read_like_a_download_dialog() {
        assert_eq!(human_bytes(487_170_055), "465 MB");
        assert_eq!(human_bytes(2_147_483_648), "2.0 GB");
    }
}
