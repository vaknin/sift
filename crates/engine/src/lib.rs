pub mod cache;
pub mod calibrate;
pub mod cull;
pub mod export;
pub mod face;
pub mod measure;
pub mod people;
pub mod preview;
pub mod reframe;
mod resize;
pub mod session;
pub mod xmp;

use std::path::Path;

use anyhow::Result;
use image::{RgbImage, imageops};
use serde::{Deserialize, Serialize};

pub use cache::Cache;
pub use face::Models;
pub use measure::FaceMetrics;
pub use preview::Shot;

/// Faces the detector is less sure of than this are ignored.
const MIN_FACE_SCORE: f32 = 0.6;
/// Faces smaller than this fraction of the largest face are background people.
const MIN_FACE_REL: f32 = 0.25;
/// Faces narrower than this (full-res px) are too small to identify reliably.
const MIN_EMBED_PX: f32 = 48.0;
/// Long side of the display copy the UI shows.
pub const DISPLAY_PX: u32 = 2048;
pub const THUMB_PX: u32 = 400;
/// Side of the grey signature used to tell similar frames apart.
pub const SIG_PX: u32 = 16;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShotAnalysis {
    pub file: String,
    pub size: u64,
    pub mtime: i64,
    pub time: f64,
    pub width: u32,
    pub height: u32,
    /// Largest first.
    pub faces: Vec<FaceMetrics>,
    /// Sharpest-region sharpness of the whole frame (for frames without a face).
    pub frame_sharp: f32,
    /// SIG_PX² grey thumbnail, levels-normalised.
    pub sig: Vec<u8>,
    pub error: Option<String>,
    /// The identity pass has run (faces carry `embed` where big enough).
    /// False for shots cached before people existed; they get it on next open.
    #[serde(default)]
    pub embedded: bool,
}

impl ShotAnalysis {
    pub fn stem(&self) -> &str {
        self.file.rsplit_once('.').map_or(&self.file, |(s, _)| s)
    }

    pub fn failed(shot: &Shot, e: &anyhow::Error) -> Self {
        let (size, mtime) = cache::stamp(&shot.path);
        Self {
            file: file_name(&shot.path),
            size,
            mtime,
            time: shot.time,
            width: 0,
            height: 0,
            faces: vec![],
            frame_sharp: 0.0,
            sig: vec![],
            error: Some(format!("{e:#}")),
            embedded: false,
        }
    }
}

pub fn file_name(p: &Path) -> String {
    p.file_name().unwrap_or_default().to_string_lossy().into_owned()
}

/// Analyse one shot and write its display images into the cache.
pub fn analyze(models: &Models, shot: &Shot, cache: &Cache) -> Result<ShotAnalysis> {
    let img = preview::load(shot)?;
    let dets = models.detect(&img, MIN_FACE_SCORE)?;
    let biggest = dets.first().map(|d| d.bbox[2]).unwrap_or(0.0);
    let mut faces = vec![];
    let mut kept = vec![];
    for d in dets.iter().filter(|d| d.bbox[2] >= biggest * MIN_FACE_REL) {
        let mesh = models.mesh(&img, d)?;
        faces.push(measure::measure(&img, d, &mesh));
        kept.push(*d);
    }
    let (size, mtime) = cache::stamp(&shot.path);
    let a = ShotAnalysis {
        file: file_name(&shot.path),
        size,
        mtime,
        time: shot.time,
        width: img.width(),
        height: img.height(),
        frame_sharp: measure::frame_sharpness(&img),
        sig: signature(&img),
        faces,
        error: None,
        embedded: false,
    };
    write_images(&img, &a, cache)?;
    let mut a = a;
    embed_faces(models, &img, &kept, &mut a, cache)?;
    Ok(a)
}

/// Identity embeddings for the faces of `a`, which were measured from `dets`
/// (same order); saves each identified face's aligned crop for the UI.
fn embed_faces(models: &Models, img: &RgbImage, dets: &[face::Detection], a: &mut ShotAnalysis, cache: &Cache) -> Result<()> {
    for (i, d) in dets.iter().enumerate() {
        if d.bbox[2] < MIN_EMBED_PX {
            continue;
        }
        let (v, norm, crop) = models.embed(img, d)?;
        cache.save_jpeg(&crop, &cache::face_name(a, i), 88)?;
        a.faces[i].embed = v;
        a.faces[i].embed_norm = norm;
    }
    a.embedded = true;
    Ok(())
}

/// The identity pass for a shot analysed before embeddings existed: detect
/// again and pair each stored face with the detection that overlaps it most.
fn backfill_embeddings(models: &Models, shot: &Shot, a: &mut ShotAnalysis, cache: &Cache) -> Result<()> {
    let img = preview::load(shot)?;
    let dets = models.detect(&img, MIN_FACE_SCORE)?;
    let mut paired = vec![];
    for f in &a.faces {
        let best = dets.iter().max_by(|x, y| face::iou(&x.bbox, &f.bbox).total_cmp(&face::iou(&y.bbox, &f.bbox)));
        match best {
            Some(d) if face::iou(&d.bbox, &f.bbox) > 0.5 => paired.push(*d),
            // No match: a zero-size stand-in that embed_faces skips.
            _ => paired.push(face::Detection { bbox: [0.0; 4], score: 0.0, kps: [[0.0; 2]; 5] }),
        }
    }
    embed_faces(models, &img, &paired, a, cache)
}

fn fit(img: &RgbImage, long: u32) -> RgbImage {
    let s = long as f32 / img.width().max(img.height()) as f32;
    if s >= 1.0 {
        return img.clone();
    }
    let (w, h) = ((img.width() as f32 * s).round() as u32, (img.height() as f32 * s).round() as u32);
    resize::rgb(img, None, w, h)
}

fn signature(img: &RgbImage) -> Vec<u8> {
    let g = imageops::grayscale(&resize::rgb(img, None, SIG_PX, SIG_PX));
    let (lo, hi) = g.iter().fold((255u8, 0u8), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    let span = hi.saturating_sub(lo).max(1) as f32;
    g.iter().map(|&v| ((v - lo) as f32 * 255.0 / span) as u8).collect()
}

fn write_images(img: &RgbImage, a: &ShotAnalysis, cache: &Cache) -> Result<()> {
    let display = fit(img, DISPLAY_PX);
    cache.save_jpeg(&display, &cache::display_name(a), 88)?;
    cache.save_jpeg(&fit(&display, THUMB_PX), &cache::thumb_name(a), 82)?;
    for (i, f) in a.faces.iter().enumerate() {
        let [x, y, w, h] = f.eyes_box;
        let (iw, ih) = (img.width() as f32, img.height() as f32);
        let (x0, y0) = (x.clamp(0.0, iw - 1.0), y.clamp(0.0, ih - 1.0));
        let (x1, y1) = ((x + w).clamp(x0 + 1.0, iw), (y + h).clamp(y0 + 1.0, ih));
        let crop = imageops::crop_imm(img, x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32).to_image();
        cache.save_jpeg(&crop, &cache::eyes_name(a, i), 92)?;
    }
    Ok(())
}

/// Analyse a folder, reusing cached results for unchanged files. `on_shot`
/// is called from worker threads as each shot finishes (done, total, shot).
/// Results are in capture order.
pub fn analyze_folder(
    folder: &Path,
    models: &Models,
    on_shot: &(dyn Fn(usize, usize, &ShotAnalysis) + Sync),
) -> Result<(Cache, Vec<ShotAnalysis>)> {
    use rayon::prelude::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let cache = Cache::for_folder(folder)?;
    let mut cached = cache.load();
    // Cached shots already know their capture time, so exiftool only has to
    // read the new or changed files; a fully cached re-open never runs it.
    let (hits, misses): (Vec<_>, Vec<_>) =
        preview::list(&cache.folder)?.into_iter().partition(|p| cached.contains_key(&file_name(p)));
    let todo = preview::read_exif(&misses)?;
    let total = hits.len() + todo.len();
    let mut results: Vec<ShotAnalysis> = hits.iter().filter_map(|p| cached.remove(&file_name(p))).collect();
    // Shots cached before identities existed get only the identity pass.
    let stale: Vec<_> = results
        .iter()
        .filter(|a| !a.embedded && !a.faces.is_empty())
        .map(|a| cache.folder.join(&a.file))
        .collect();
    let stale: std::collections::HashMap<String, Shot> =
        preview::read_exif(&stale)?.into_iter().map(|s| (file_name(&s.path), s)).collect();
    for (i, a) in results.iter().filter(|a| !stale.contains_key(&a.file)).enumerate() {
        on_shot(i + 1, total, a);
    }
    let done = AtomicUsize::new(results.len() - stale.len());
    results.par_iter_mut().filter(|a| stale.contains_key(&a.file)).for_each(|a| {
        // A failure leaves the shot unembedded; it is retried on the next open.
        let _ = backfill_embeddings(models, &stale[&a.file], a, &cache);
        on_shot(done.fetch_add(1, Ordering::Relaxed) + 1, total, a);
    });
    let done = AtomicUsize::new(results.len());
    results.par_extend(todo.par_iter().map(|shot| {
        let a = analyze(models, shot, &cache).unwrap_or_else(|e| ShotAnalysis::failed(shot, &e));
        on_shot(done.fetch_add(1, Ordering::Relaxed) + 1, total, &a);
        a
    }));
    results.sort_by(|a, b| a.time.total_cmp(&b.time).then_with(|| a.file.cmp(&b.file)));
    cache.save(&results)?;
    Ok((cache, results))
}
