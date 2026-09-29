//! Per-folder cache: analysis results plus the display images the UI shows.
//! Lives in `~/.cache/sift/<folder>-<hash>/`; nothing is written next to the photos.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use image::RgbImage;
use image::codecs::jpeg::JpegEncoder;

use crate::ShotAnalysis;

/// Bump when ShotAnalysis or the measurements change meaning.
const VERSION: u32 = 1;

pub struct Cache {
    pub dir: PathBuf,
    pub folder: PathBuf,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Stored {
    version: u32,
    shots: Vec<ShotAnalysis>,
}

/// (size, mtime seconds) identifying a file's contents well enough.
pub fn stamp(p: &Path) -> (u64, i64) {
    let Ok(m) = std::fs::metadata(p) else { return (0, 0) };
    let mtime = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    (m.len(), mtime)
}

pub(crate) fn fnv1a(s: &[u8]) -> u64 {
    s.iter().fold(0xcbf29ce484222325, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

pub fn display_name(a: &ShotAnalysis) -> String {
    format!("{}.jpg", a.stem())
}
pub fn thumb_name(a: &ShotAnalysis) -> String {
    format!("{}_t.jpg", a.stem())
}
pub fn eyes_name(a: &ShotAnalysis, face: usize) -> String {
    format!("{}_eyes{face}.jpg", a.stem())
}
/// The face aligned for identity (112² px), written for faces with an embedding.
pub fn face_name(a: &ShotAnalysis, face: usize) -> String {
    format!("{}_face{face}.jpg", a.stem())
}

impl Cache {
    pub fn for_folder(folder: &Path) -> Result<Self> {
        let folder = folder.canonicalize().with_context(|| format!("opening {}", folder.display()))?;
        let base = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
            .context("no cache directory")?;
        let name = folder.file_name().unwrap_or_default().to_string_lossy();
        let dir = base.join("sift").join(format!("{name}-{:08x}", fnv1a(folder.as_os_str().as_encoded_bytes()) as u32));
        std::fs::create_dir_all(dir.join("img"))?;
        Ok(Self { dir, folder })
    }

    pub fn img(&self, name: &str) -> PathBuf {
        self.dir.join("img").join(name)
    }

    pub fn save_jpeg(&self, img: &RgbImage, name: &str, quality: u8) -> Result<()> {
        let tmp = self.img(&format!("{name}.tmp"));
        let mut f = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        JpegEncoder::new_with_quality(&mut f, quality).encode_image(img)?;
        drop(f);
        std::fs::rename(&tmp, self.img(name))?;
        Ok(())
    }

    /// Cached analyses that still match their file (same size and mtime, images present).
    pub fn load(&self) -> HashMap<String, ShotAnalysis> {
        let Ok(bytes) = std::fs::read(self.dir.join("analysis.json")) else { return HashMap::new() };
        let Ok(stored) = serde_json::from_slice::<Stored>(&bytes) else { return HashMap::new() };
        if stored.version != VERSION {
            return HashMap::new();
        }
        stored
            .shots
            .into_iter()
            .filter(|a| a.error.is_none() && stamp(&self.folder.join(&a.file)) == (a.size, a.mtime))
            .filter(|a| self.img(&display_name(a)).exists() && self.img(&thumb_name(a)).exists())
            .map(|a| (a.file.clone(), a))
            .collect()
    }

    pub fn save(&self, shots: &[ShotAnalysis]) -> Result<()> {
        let tmp = self.dir.join("analysis.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(&Stored { version: VERSION, shots: shots.to_vec() })?)?;
        std::fs::rename(tmp, self.dir.join("analysis.json"))?;
        Ok(())
    }
}
