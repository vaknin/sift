//! Per-face measurements on the full-resolution preview.

use image::{GrayImage, RgbImage, imageops};
use serde::{Deserialize, Serialize};

use crate::face::{Detection, Mesh, blend};

/// Eye crops are measured at native resolution: a slight focus miss is a
/// pixel or two of blur at 100%, and any downscale hides it. Only very large
/// eyes are brought down to this width.
pub const EYE_PX: f32 = 400.0;
const FACE_PX: f32 = 360.0;

// MediaPipe mesh indices; "right"/"left" are the subject's.
const R_OUTER: usize = 33;
const R_INNER: usize = 133;
const R_UP: usize = 159;
const R_DOWN: usize = 145;
const L_OUTER: usize = 263;
const L_INNER: usize = 362;
const L_UP: usize = 386;
const L_DOWN: usize = 374;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaceMetrics {
    pub bbox: [f32; 4],
    pub det_score: f32,
    pub presence: f32,
    /// Eye width in full-res pixels (subject's right, left). Small = low-res face.
    pub eye_px: [f32; 2],
    /// Contrast-normalised detail at the eyes (right, left). Higher = sharper.
    pub eye_sharp: [f32; 2],
    pub face_sharp: f32,
    /// MediaPipe eyeBlink scores (right, left): 0 open … 1 closed.
    pub blink: [f32; 2],
    /// Lid gap / eye width (right, left).
    pub ear: [f32; 2],
    pub smile: f32,
    /// Mean luma of the inner face, 0–255, and fraction of it clipped.
    pub face_luma: f32,
    pub face_clip: f32,
    /// Part of the face is outside the frame.
    pub cut: bool,
    /// Eye crop boxes in full-res pixels (right, left).
    pub eye_boxes: [[f32; 4]; 2],
    /// Box spanning both eyes, full-res pixels, for the UI's eye strip.
    pub eyes_box: [f32; 4],
    /// SFace identity embedding, unit length; empty when the face was too
    /// small to identify (or the shot predates embeddings).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub embed: Vec<f32>,
    /// Length of the embedding before normalising: how face-like and sharp
    /// the face looked to the identity model.
    #[serde(default)]
    pub embed_norm: f32,
}

fn dist(a: &[f32; 3], b: &[f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

pub fn measure(img: &RgbImage, det: &Detection, mesh: &Mesh) -> FaceMetrics {
    let p = &mesh.points;
    let eye = |outer: usize, inner: usize, up: usize, down: usize| {
        let w = dist(&p[outer], &p[inner]).max(1.0);
        let c = [(p[outer][0] + p[inner][0]) / 2.0, (p[outer][1] + p[inner][1]) / 2.0];
        let bw = w * 1.8;
        let bh = w * 1.1;
        let bx = [c[0] - bw / 2.0, c[1] - bh / 2.0, bw, bh];
        let crop = gray_crop(img, bx, EYE_PX / w);
        (w, sharpness(&crop), dist(&p[up], &p[down]) / w, bx)
    };
    let (rw, rs, rear, rbox) = eye(R_OUTER, R_INNER, R_UP, R_DOWN);
    let (lw, ls, lear, lbox) = eye(L_OUTER, L_INNER, L_UP, L_DOWN);

    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for q in p {
        (x0, y0, x1, y1) = (x0.min(q[0]), y0.min(q[1]), x1.max(q[0]), y1.max(q[1]));
    }
    let (iw, ih) = (img.width() as f32, img.height() as f32);
    let margin = 0.03 * (x1 - x0);
    let cut = x0 < -margin || y0 < -margin || x1 > iw + margin || y1 > ih + margin;

    // Inner face: middle 60% of the landmark box.
    let (fw, fh) = (x1 - x0, y1 - y0);
    let inner = [x0 + fw * 0.2, y0 + fh * 0.2, fw * 0.6, fh * 0.6];
    let face = gray_crop(img, inner, FACE_PX / fw.max(1.0));
    let n = face.len().max(1) as f32;
    let face_luma = face.iter().map(|&v| v as f32).sum::<f32>() / n;
    let face_clip = face.iter().filter(|&&v| v >= 250).count() as f32 / n;

    let m = FaceMetrics {
        bbox: det.bbox,
        det_score: det.score,
        presence: mesh.presence,
        eye_px: [rw, lw],
        eye_sharp: [rs, ls],
        face_sharp: sharpness(&face),
        blink: [blend(mesh, "eyeBlinkRight"), blend(mesh, "eyeBlinkLeft")],
        ear: [rear, lear],
        smile: (blend(mesh, "mouthSmileLeft") + blend(mesh, "mouthSmileRight")) / 2.0,
        face_luma,
        face_clip,
        cut,
        eye_boxes: [rbox, lbox],
        eyes_box: {
            let (x0, y0) = (rbox[0].min(lbox[0]), rbox[1].min(lbox[1]));
            let (x1, y1) = ((rbox[0] + rbox[2]).max(lbox[0] + lbox[2]), (rbox[1] + rbox[3]).max(lbox[1] + lbox[3]));
            let pad = 0.15 * (x1 - x0);
            [x0 - pad, y0 - pad, x1 - x0 + 2.0 * pad, y1 - y0 + 2.0 * pad]
        },
        embed: vec![],
        embed_norm: 0.0,
    };
    m
}

/// Grey crop of `bx` (full-res px), scaled by `scale` (never enlarged).
fn gray_crop(img: &RgbImage, bx: [f32; 4], scale: f32) -> GrayImage {
    let (iw, ih) = (img.width() as f32, img.height() as f32);
    let x0 = bx[0].clamp(0.0, iw - 1.0);
    let y0 = bx[1].clamp(0.0, ih - 1.0);
    let x1 = (bx[0] + bx[2]).clamp(x0 + 1.0, iw);
    let y1 = (bx[1] + bx[3]).clamp(y0 + 1.0, ih);
    let sub = imageops::crop_imm(img, x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32).to_image();
    let g = imageops::grayscale(&sub);
    if scale < 1.0 {
        let (w, h) = (((x1 - x0) * scale).round().max(1.0) as u32, ((y1 - y0) * scale).round().max(1.0) as u32);
        imageops::resize(&g, w, h, imageops::FilterType::Triangle)
    } else {
        g
    }
}

/// Sharpness of the sharpest part of a frame without a usable face (a figure
/// from behind, a hand): the 90th percentile of per-tile [`sharpness`] on a
/// half-resolution grey copy, so defocused backgrounds don't drag it down.
pub fn frame_sharpness(img: &RgbImage) -> f32 {
    let half = crate::resize::rgb(img, None, img.width() / 2, img.height() / 2);
    let g = imageops::grayscale(&half);
    let (w, h) = g.dimensions();
    let t = (w.max(h) / 12).max(16);
    let mut v = vec![];
    for y in (0..h.saturating_sub(t)).step_by(t as usize) {
        for x in (0..w.saturating_sub(t)).step_by(t as usize) {
            let tile = imageops::crop_imm(&g, x, y, t, t).to_image();
            // Flat tiles (sky, a wall, deep shadow) have no detail to judge.
            if std_dev(&tile) > 6.0 {
                v.push(sharpness(&tile));
            }
        }
    }
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f32::total_cmp);
    v[(v.len() as f32 * 0.9) as usize]
}

fn std_dev(g: &GrayImage) -> f32 {
    let n = g.len().max(1) as f32;
    let mean = g.iter().map(|&v| v as f32).sum::<f32>() / n;
    (g.iter().map(|&v| (v as f32 - mean).powi(2)).sum::<f32>() / n).sqrt()
}

/// Contrast-normalised high-frequency energy: RMS of the Laplacian over
/// (std + 4). Blur removes fine detail but barely changes overall contrast, so
/// the ratio falls with blur while staying fair to dark, low-key faces.
pub fn sharpness(g: &GrayImage) -> f32 {
    let (w, h) = (g.width() as usize, g.height() as usize);
    if w < 5 || h < 5 {
        return 0.0;
    }
    let px = |x: usize, y: usize| g.as_raw()[y * w + x] as f32;
    let n = (w * h) as f32;
    let mean = g.iter().map(|&v| v as f32).sum::<f32>() / n;
    let var = g.iter().map(|&v| (v as f32 - mean).powi(2)).sum::<f32>() / n;
    let mut e = 0.0;
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let l = px(x - 1, y) + px(x + 1, y) + px(x, y - 1) + px(x, y + 1) - 4.0 * px(x, y);
            e += l * l;
        }
    }
    let rms = (e / ((w - 2) * (h - 2)) as f32).sqrt();
    rms / (var.sqrt() + 4.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(blur: u32) -> GrayImage {
        let g = GrayImage::from_fn(120, 80, |x, y| image::Luma([if (x / 3 + y / 5) % 2 == 0 { 40 } else { 200 }]));
        if blur == 0 { g } else { imageops::blur(&g, blur as f32) }
    }

    #[test]
    fn blur_lowers_sharpness() {
        let (a, b, c) = (sharpness(&pattern(0)), sharpness(&pattern(1)), sharpness(&pattern(3)));
        assert!(a > b && b > c, "{a} {b} {c}");
    }

    #[test]
    fn darkening_keeps_sharpness() {
        let bright = pattern(1);
        let dark = GrayImage::from_fn(120, 80, |x, y| image::Luma([bright.get_pixel(x, y)[0] / 4]));
        let (a, b) = (sharpness(&bright), sharpness(&dark));
        assert!((a - b).abs() / a < 0.35, "{a} {b}");
    }
}
