//! Getting pixels out of a shoot: the camera's embedded full-size JPEG for raws,
//! the file itself for JPEGs. No raw decoding.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, RgbImage};
use serde::Deserialize;

const RAW_EXTS: &[&str] = &["cr3", "cr2", "nef", "arw", "raf", "orf", "rw2", "dng"];
const JPEG_EXTS: &[&str] = &["jpg", "jpeg"];

#[derive(Debug, Clone)]
pub struct Shot {
    pub path: PathBuf,
    /// Capture time in seconds (local wall clock, sub-second when available).
    pub time: f64,
    pub orientation: u8,
}

pub fn is_photo(p: &Path) -> bool {
    let ext = p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    ext.is_some_and(|e| RAW_EXTS.contains(&e.as_str()) || JPEG_EXTS.contains(&e.as_str()))
}

fn is_raw(p: &Path) -> bool {
    let ext = p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    ext.is_some_and(|e| RAW_EXTS.contains(&e.as_str()))
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ExifRow {
    source_file: PathBuf,
    orientation: Option<u8>,
    date_time_original: Option<String>,
    sub_sec_time_original: Option<serde_json::Value>,
}

/// List the photos in `dir` with capture time and orientation, in capture order.
pub fn scan(dir: &Path) -> Result<Vec<Shot>> {
    let mut shots = read_exif(&list(dir)?)?;
    sort(&mut shots);
    Ok(shots)
}

/// The photo files in `dir`. When a raw and a JPEG share a stem (RAW+JPEG
/// shooting), only the raw is kept.
pub fn list(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && is_photo(p))
        .collect();
    let raw_stems: std::collections::HashSet<_> =
        files.iter().filter(|p| is_raw(p)).map(|p| p.with_extension("")).collect();
    files.retain(|p| is_raw(p) || !raw_stems.contains(&p.with_extension("")));
    Ok(files)
}

/// Capture order: time, then name for frames within the same instant.
pub fn sort(shots: &mut [Shot]) {
    shots.sort_by(|a, b| a.time.total_cmp(&b.time).then_with(|| a.path.cmp(&b.path)));
}

/// Capture time and orientation of `files`, in one exiftool run.
pub fn read_exif(files: &[PathBuf]) -> Result<Vec<Shot>> {
    if files.is_empty() {
        return Ok(vec![]);
    }

    let out = Command::new("exiftool")
        .args(["-j", "-n", "-q", "-fast2", "-Orientation", "-DateTimeOriginal", "-SubSecTimeOriginal"])
        .args(files)
        .output()
        .context("running exiftool")?;
    if !out.status.success() && out.stdout.is_empty() {
        bail!("exiftool failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let rows: Vec<ExifRow> = serde_json::from_slice(&out.stdout).context("parsing exiftool json")?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let subsec = match &r.sub_sec_time_original {
                Some(serde_json::Value::Number(n)) => format!("0.{n}").parse().unwrap_or(0.0),
                Some(serde_json::Value::String(s)) => format!("0.{s}").parse().unwrap_or(0.0),
                _ => 0.0,
            };
            let time = r.date_time_original.as_deref().and_then(parse_exif_time).unwrap_or(0.0) + subsec;
            Shot { path: r.source_file, time, orientation: r.orientation.unwrap_or(1) }
        })
        .collect())
}

/// "YYYY:MM:DD HH:MM:SS" → seconds on a monotonic-enough scale (days from a fixed epoch).
fn parse_exif_time(s: &str) -> Option<f64> {
    let n: Vec<i64> = s
        .split(|c: char| !c.is_ascii_digit())
        .filter(|t| !t.is_empty())
        .take(6)
        .map(|t| t.parse().ok())
        .collect::<Option<_>>()?;
    let [y, mo, d, h, mi, se] = n[..] else { return None };
    // Days from civil (Howard Hinnant), good for ordering and gaps.
    let (y, m) = if mo <= 2 { (y - 1, mo + 9) } else { (y, mo - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe;
    Some((days * 86400 + h * 3600 + mi * 60 + se) as f64)
}

/// The shot's pixels, upright.
pub fn load(shot: &Shot) -> Result<RgbImage> {
    let bytes = if is_raw(&shot.path) {
        let out = Command::new("exiftool")
            .args(["-b", "-JpgFromRaw"])
            .arg(&shot.path)
            .output()
            .context("running exiftool")?;
        if out.stdout.len() < 1000 {
            let out = Command::new("exiftool").args(["-b", "-PreviewImage"]).arg(&shot.path).output()?;
            if out.stdout.len() < 1000 {
                bail!("{}: no embedded preview", shot.path.display());
            }
            out.stdout
        } else {
            out.stdout
        }
    } else {
        std::fs::read(&shot.path)?
    };
    let mut dec = image::codecs::jpeg::JpegDecoder::new(std::io::Cursor::new(&bytes))?;
    // Raw previews carry no orientation of their own; the raw's EXIF says how to turn it.
    let orient = if is_raw(&shot.path) {
        Orientation::from_exif(shot.orientation).unwrap_or(Orientation::NoTransforms)
    } else {
        dec.orientation().unwrap_or(Orientation::NoTransforms)
    };
    let mut img = DynamicImage::from_decoder(dec)?;
    img.apply_orientation(orient);
    Ok(img.into_rgb8())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exif_time_orders_and_gaps() {
        let a = parse_exif_time("2026:09:27 14:41:51").unwrap();
        let b = parse_exif_time("2026:09:27 14:41:52").unwrap();
        let c = parse_exif_time("2026:09:28 00:00:00").unwrap();
        assert_eq!(b - a, 1.0);
        assert!(c > b);
        assert_eq!(parse_exif_time("2026:03:01 00:00:00").unwrap() - parse_exif_time("2026:02:28 00:00:00").unwrap(), 86400.0);
    }
}
