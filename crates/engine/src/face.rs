//! Face detection (YuNet), the MediaPipe face mesh (478 landmarks) and
//! MediaPipe blendshapes (eye blink, smile, ...) and SFace identity
//! embeddings, all run through tract.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Result, ensure};
use image::RgbImage;
use serde::{Deserialize, Serialize};
use tract_onnx::prelude::*;

static YUNET: &[u8] = include_bytes!("../../../models/face_detection_yunet.dyn.onnx");
static MESH: &[u8] = include_bytes!("../../../models/face_landmarks.onnx");
static BLEND: &[u8] = include_bytes!("../../../models/face_blendshapes.sim.onnx");
static SFACE: &[u8] = include_bytes!("../../../models/face_recognition_sface_2021dec.onnx");

type Plan = Arc<TypedRunnableModel>;

/// Long side the detector sees first. Portrait faces are big, and 640 keeps a
/// full-body face at ~40 px; frames with no face get a second look at 1280.
const DETECT_LONG: [u32; 2] = [640, 1280];
const MESH_SIZE: usize = 256;
/// SFace's aligned input side and embedding length.
pub const ALIGN_SIZE: usize = 112;
pub const EMBED_LEN: usize = 128;
/// Where the five YuNet landmarks land in the aligned 112² crop (the
/// ArcFace template OpenCV's `alignCrop` uses). Image-left eye first, as YuNet.
const TEMPLATE: [[f32; 2]; 5] =
    [[38.2946, 51.6963], [73.5318, 51.5014], [56.0252, 71.7366], [41.5493, 92.3655], [70.7299, 92.2041]];

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Detection {
    /// x, y, w, h in full-resolution pixels.
    pub bbox: [f32; 4],
    pub score: f32,
    /// Right eye, left eye, nose, right mouth corner, left mouth corner (subject's sides).
    pub kps: [[f32; 2]; 5],
}

#[derive(Debug, Clone)]
pub struct Mesh {
    /// 478 landmarks in full-resolution pixels (z in crop units, unused).
    pub points: Vec<[f32; 3]>,
    pub presence: f32,
    /// 52 MediaPipe blendshape scores, see [`BLENDSHAPES`].
    pub blend: Vec<f32>,
}

pub struct Models {
    yunet: Vec<u8>,
    yunet_plans: Mutex<HashMap<(usize, usize), Plan>>,
    mesh: Plan,
    blend: Plan,
    sface: Plan,
}

fn load(bytes: &[u8], shape: &[usize]) -> Result<Plan> {
    Ok(tract_onnx::onnx()
        .model_for_read(&mut &bytes[..])?
        .with_input_fact(0, f32::fact(shape).into())?
        .into_optimized()?
        .into_runnable()?)
}

impl Models {
    pub fn new() -> Result<Self> {
        Ok(Self {
            yunet: YUNET.to_vec(),
            yunet_plans: Mutex::default(),
            mesh: load(MESH, &[1, MESH_SIZE, MESH_SIZE, 3])?,
            blend: load(BLEND, &[1, 146, 2])?,
            sface: load(SFACE, &[1, 3, ALIGN_SIZE, ALIGN_SIZE])?,
        })
    }

    fn yunet_plan(&self, h: usize, w: usize) -> Result<Plan> {
        // Held while building: optimising a plan takes seconds, and every
        // worker would otherwise build its own copy of the same shape.
        let mut plans = self.yunet_plans.lock().unwrap();
        if let Some(p) = plans.get(&(h, w)) {
            return Ok(p.clone());
        }
        let p = load(&self.yunet, &[1, 3, h, w])?;
        plans.insert((h, w), p.clone());
        Ok(p)
    }

    /// All faces above `min_score`, largest first.
    pub fn detect(&self, img: &RgbImage, min_score: f32) -> Result<Vec<Detection>> {
        let faces = self.detect_at(img, min_score, DETECT_LONG[0])?;
        if !faces.is_empty() {
            return Ok(faces);
        }
        self.detect_at(img, min_score, DETECT_LONG[1])
    }

    fn detect_at(&self, img: &RgbImage, min_score: f32, long: u32) -> Result<Vec<Detection>> {
        let (w0, h0) = img.dimensions();
        let s = long as f32 / w0.max(h0) as f32;
        let (w1, h1) = ((w0 as f32 * s).round() as u32, (h0 as f32 * s).round() as u32);
        let mut small = crate::resize::rgb(img, None, w1, h1);
        lift(small.as_mut());
        let (pw, ph) = (w1.div_ceil(32) as usize * 32, h1.div_ceil(32) as usize * 32);

        // BGR, 0..255, NCHW, zero padding right/bottom.
        let mut input = tract_ndarray::Array4::<f32>::zeros((1, 3, ph, pw));
        for (x, y, p) in small.enumerate_pixels() {
            for c in 0..3 {
                input[[0, c, y as usize, x as usize]] = p[2 - c] as f32;
            }
        }
        let out = self.yunet_plan(ph, pw)?.run(tvec!(input.into_tensor().into()))?;
        ensure!(out.len() == 12, "unexpected YuNet outputs");

        let mut faces = vec![];
        for (i, stride) in [8usize, 16, 32].into_iter().enumerate() {
            let cls = f32s(&out[i])?;
            let obj = f32s(&out[i + 3])?;
            let bbox = f32s(&out[i + 6])?;
            let kps = f32s(&out[i + 9])?;
            let cols = pw / stride;
            let st = stride as f32;
            for idx in 0..cls.len() {
                let score = (cls[idx].clamp(0.0, 1.0) * obj[idx].clamp(0.0, 1.0)).sqrt();
                if score < min_score {
                    continue;
                }
                let (r, c) = ((idx / cols) as f32, (idx % cols) as f32);
                let b = &bbox[idx * 4..idx * 4 + 4];
                let (cx, cy) = ((c + b[0]) * st, (r + b[1]) * st);
                let (bw, bh) = (b[2].exp() * st, b[3].exp() * st);
                let k = &kps[idx * 10..idx * 10 + 10];
                let mut kp = [[0f32; 2]; 5];
                for (n, p) in kp.iter_mut().enumerate() {
                    *p = [(k[2 * n] + c) * st / s, (k[2 * n + 1] + r) * st / s];
                }
                faces.push(Detection {
                    bbox: [(cx - bw / 2.0) / s, (cy - bh / 2.0) / s, bw / s, bh / s],
                    score,
                    kps: kp,
                });
            }
        }
        let mut faces = nms(faces, 0.3);
        faces.sort_by(|a, b| (b.bbox[2] * b.bbox[3]).total_cmp(&(a.bbox[2] * a.bbox[3])));
        Ok(faces)
    }

    /// Face mesh + blendshapes for one detected face. Runs the mesh twice: the
    /// second pass uses a crop fitted to the first pass's landmarks, as MediaPipe does.
    pub fn mesh(&self, img: &RgbImage, det: &Detection) -> Result<Mesh> {
        let [x, y, w, h] = det.bbox;
        let (re, le) = (det.kps[0], det.kps[1]);
        let mut roi = Roi {
            cx: x + w / 2.0,
            cy: y + h / 2.0,
            size: w.max(h) * 1.5,
            angle: (le[1] - re[1]).atan2(le[0] - re[0]),
        };
        let mut mesh = self.mesh_once(img, &roi)?;
        roi = Roi::from_landmarks(&mesh.points);
        let second = self.mesh_once(img, &roi)?;
        if second.presence >= mesh.presence * 0.8 {
            mesh = second;
        }

        let pts: Vec<f32> = BLEND_SUBSET
            .iter()
            .flat_map(|&i| [mesh.points[i][0], mesh.points[i][1]])
            .collect();
        let t = tract_ndarray::Array3::from_shape_vec((1, 146, 2), pts)?;
        let out = self.blend.run(tvec!(t.into_tensor().into()))?;
        mesh.blend = f32s(&out[0])?.to_vec();
        Ok(mesh)
    }

    /// Identity embedding of one face (unit length; cosine similarity is a dot
    /// product), its length before normalising (low for blurred, turned or
    /// non-faces), and the aligned crop it was computed from.
    pub fn embed(&self, img: &RgbImage, det: &Detection) -> Result<(Vec<f32>, f32, RgbImage)> {
        let crop = align(img, &det.kps);
        // RGB, 0..255, NCHW (OpenCV's FaceRecognizerSF swaps its BGR to RGB).
        let mut input = tract_ndarray::Array4::<f32>::zeros((1, 3, ALIGN_SIZE, ALIGN_SIZE));
        for (x, y, p) in crop.enumerate_pixels() {
            for c in 0..3 {
                input[[0, c, y as usize, x as usize]] = p[c] as f32;
            }
        }
        let out = self.sface.run(tvec!(input.into_tensor().into()))?;
        let mut v = f32s(&out[0])?.to_vec();
        ensure!(v.len() == EMBED_LEN, "unexpected SFace output length {}", v.len());
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
        v.iter_mut().for_each(|x| *x /= n);
        Ok((v, n, crop))
    }

    fn mesh_once(&self, img: &RgbImage, roi: &Roi) -> Result<Mesh> {
        let input = roi.warp(img);
        let t = tract_ndarray::Array4::from_shape_vec((1, MESH_SIZE, MESH_SIZE, 3), input)?;
        let out = self.mesh.run(tvec!(t.into_tensor().into()))?;
        let mut points = vec![];
        let mut presence = 0.0;
        for o in out.iter() {
            let v = f32s(o)?;
            if v.len() == 478 * 3 {
                points = v.chunks(3).map(|p| roi.unwarp(p[0], p[1], p[2])).collect();
            } else if v.len() == 1 && presence == 0.0 {
                presence = 1.0 / (1.0 + (-v[0]).exp());
            }
        }
        ensure!(points.len() == 478, "face mesh returned no landmarks");
        Ok(Mesh { points, presence, blend: vec![] })
    }
}

/// A rotated square crop in full-resolution pixels.
struct Roi {
    cx: f32,
    cy: f32,
    size: f32,
    /// Direction of the subject's right→left eye line, radians, image coordinates.
    angle: f32,
}

impl Roi {
    fn from_landmarks(p: &[[f32; 3]]) -> Self {
        let angle = (p[263][1] - p[33][1]).atan2(p[263][0] - p[33][0]);
        let (c, s) = (angle.cos(), angle.sin());
        let (mut u0, mut u1, mut v0, mut v1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        for q in p {
            let (u, v) = (q[0] * c + q[1] * s, -q[0] * s + q[1] * c);
            (u0, u1, v0, v1) = (u0.min(u), u1.max(u), v0.min(v), v1.max(v));
        }
        let (uc, vc) = ((u0 + u1) / 2.0, (v0 + v1) / 2.0);
        Roi { cx: uc * c - vc * s, cy: uc * s + vc * c, size: (u1 - u0).max(v1 - v0) * 1.5, angle }
    }

    fn to_image(&self, nx: f32, ny: f32) -> (f32, f32) {
        let (px, py) = (nx * self.size, ny * self.size);
        let (c, s) = (self.angle.cos(), self.angle.sin());
        (self.cx + px * c - py * s, self.cy + px * s + py * c)
    }

    fn unwarp(&self, x: f32, y: f32, z: f32) -> [f32; 3] {
        let n = MESH_SIZE as f32;
        let (ix, iy) = self.to_image(x / n - 0.5, y / n - 0.5);
        [ix, iy, z]
    }

    /// RGB in [0,1], HWC, MESH_SIZE². Downscale the covering region first so the
    /// bilinear rotation doesn't alias; outside the frame is black.
    fn warp(&self, img: &RgbImage) -> Vec<f32> {
        let half = self.size * 0.72 + 2.0;
        let (iw, ih) = (img.width() as f32, img.height() as f32);
        let x0 = (self.cx - half).floor().clamp(0.0, iw - 1.0);
        let y0 = (self.cy - half).floor().clamp(0.0, ih - 1.0);
        let x1 = (self.cx + half).ceil().clamp(x0 + 1.0, iw);
        let y1 = (self.cy + half).ceil().clamp(y0 + 1.0, ih);
        let f = (MESH_SIZE as f32 / self.size).min(1.0);
        let sw = ((x1 - x0) * f).round().max(1.0) as u32;
        let sh = ((y1 - y0) * f).round().max(1.0) as u32;
        let region = [x0 as f64, y0 as f64, (x1 - x0) as f64, (y1 - y0) as f64];
        let sub = crate::resize::rgb(img, Some(region), sw, sh);
        // The resize maps the region onto whole pixels; use the actual scale.
        let f = sw as f32 / (x1 - x0);

        let n = MESH_SIZE;
        let mut out = vec![0f32; n * n * 3];
        for v in 0..n {
            for u in 0..n {
                let (ix, iy) = self.to_image((u as f32 + 0.5) / n as f32 - 0.5, (v as f32 + 0.5) / n as f32 - 0.5);
                let (sx, sy) = ((ix - x0) * f - 0.5, (iy - y0) * f - 0.5);
                if let Some(px) = bilinear(&sub, sx, sy) {
                    out[(v * n + u) * 3..(v * n + u) * 3 + 3].copy_from_slice(&px);
                }
            }
        }
        lift(&mut out);
        out.iter_mut().for_each(|c| *c /= 255.0);
        out
    }
}

/// Similarity transform (a, b, tx, ty) taking template points to image points
/// in the least-squares sense: image = [a -b; b a] · template + t.
fn similarity(from: &[[f32; 2]; 5], to: &[[f32; 2]; 5]) -> [f32; 4] {
    let mean = |p: &[[f32; 2]; 5]| {
        let (x, y) = p.iter().fold((0.0, 0.0), |(x, y), q| (x + q[0], y + q[1]));
        [x / 5.0, y / 5.0]
    };
    let (mf, mt) = (mean(from), mean(to));
    let (mut sa, mut sb, mut ss) = (0.0, 0.0, 0.0);
    for (f, t) in from.iter().zip(to) {
        let (x, y) = (f[0] - mf[0], f[1] - mf[1]);
        let (u, v) = (t[0] - mt[0], t[1] - mt[1]);
        sa += x * u + y * v;
        sb += x * v - y * u;
        ss += x * x + y * y;
    }
    let (a, b) = (sa / ss.max(1e-6), sb / ss.max(1e-6));
    [a, b, mt[0] - (a * mf[0] - b * mf[1]), mt[1] - (b * mf[0] + a * mf[1])]
}

/// The face warped onto the SFace template, ALIGN_SIZE². Downscales the
/// covering region first so a big face doesn't alias; outside the frame is black.
fn align(img: &RgbImage, kps: &[[f32; 2]; 5]) -> RgbImage {
    let [a, b, tx, ty] = similarity(&TEMPLATE, kps);
    let map = |u: f32, v: f32| (a * u - b * v + tx, b * u + a * v + ty);
    let n = ALIGN_SIZE as f32;
    let corners = [map(0.0, 0.0), map(n, 0.0), map(0.0, n), map(n, n)];
    let (iw, ih) = (img.width() as f32, img.height() as f32);
    let fold = |f: fn(f32, f32) -> f32, init: f32, k: usize| {
        corners.iter().map(|c| if k == 0 { c.0 } else { c.1 }).fold(init, f)
    };
    let x0 = (fold(f32::min, f32::MAX, 0) - 2.0).floor().clamp(0.0, iw - 1.0);
    let y0 = (fold(f32::min, f32::MAX, 1) - 2.0).floor().clamp(0.0, ih - 1.0);
    let x1 = (fold(f32::max, f32::MIN, 0) + 2.0).ceil().clamp(x0 + 1.0, iw);
    let y1 = (fold(f32::max, f32::MIN, 1) + 2.0).ceil().clamp(y0 + 1.0, ih);
    let scale = (a * a + b * b).sqrt().max(1e-6);
    let f = (1.0 / scale).min(1.0);
    let sw = ((x1 - x0) * f).round().max(1.0) as u32;
    let sh = ((y1 - y0) * f).round().max(1.0) as u32;
    let region = [x0 as f64, y0 as f64, (x1 - x0) as f64, (y1 - y0) as f64];
    let sub = crate::resize::rgb(img, Some(region), sw, sh);
    let f = sw as f32 / (x1 - x0);

    let mut out = RgbImage::new(ALIGN_SIZE as u32, ALIGN_SIZE as u32);
    for (u, v, px) in out.enumerate_pixels_mut() {
        let (ix, iy) = map(u as f32 + 0.5, v as f32 + 0.5);
        if let Some(c) = bilinear(&sub, (ix - x0) * f - 0.5, (iy - y0) * f - 0.5) {
            *px = image::Rgb(c.map(|c| c.round().clamp(0.0, 255.0) as u8));
        }
    }
    lift(out.as_mut());
    out
}

/// Brighten low-key frames for the networks (they're trained on ordinary
/// exposures): stretch so the 99.5th percentile sits at 235, then lift the
/// midtones with a gamma until the median is near 100. Works on RGB triples
/// in 0..255, u8 or f32.
fn lift<T: Copy + Into<f32> + FromF32>(px: &mut [T]) {
    let mut lum: Vec<f32> = px.chunks(3).map(|p| (p[0].into() + p[1].into() + p[2].into()) / 3.0).collect();
    if lum.is_empty() {
        return;
    }
    lum.sort_by(f32::total_cmp);
    let hi = lum[(lum.len() as f32 * 0.995) as usize].max(8.0);
    let gain = (235.0 / hi).clamp(1.0, 8.0);
    let med = (lum[lum.len() / 2] * gain / 255.0).clamp(0.01, 0.99);
    let gamma = if med < 0.4 { ((100.0f32 / 255.0).ln() / med.ln()).clamp(0.35, 1.0) } else { 1.0 };
    if gain == 1.0 && gamma == 1.0 {
        return;
    }
    let lut: Vec<f32> = (0..256).map(|v| 255.0 * ((v as f32 * gain / 255.0).min(1.0)).powf(gamma)).collect();
    for c in px.iter_mut() {
        let v: f32 = (*c).into();
        let i = v.clamp(0.0, 255.0);
        let (i0, f) = (i.floor() as usize, i.fract());
        let y = lut[i0] * (1.0 - f) + lut[(i0 + 1).min(255)] * f;
        *c = T::from_f32(y);
    }
}

trait FromF32 {
    fn from_f32(v: f32) -> Self;
}
impl FromF32 for f32 {
    fn from_f32(v: f32) -> Self {
        v
    }
}
impl FromF32 for u8 {
    fn from_f32(v: f32) -> Self {
        v.round().clamp(0.0, 255.0) as u8
    }
}

fn bilinear(img: &RgbImage, x: f32, y: f32) -> Option<[f32; 3]> {
    let (w, h) = (img.width() as f32, img.height() as f32);
    if x < -0.5 || y < -0.5 || x > w - 0.5 || y > h - 0.5 {
        return None;
    }
    let (x, y) = (x.clamp(0.0, w - 1.0), y.clamp(0.0, h - 1.0));
    let (xi, yi) = (x.floor() as u32, y.floor() as u32);
    let (xj, yj) = ((xi + 1).min(img.width() - 1), (yi + 1).min(img.height() - 1));
    let (fx, fy) = (x - xi as f32, y - yi as f32);
    let (a, b, c, d) = (img.get_pixel(xi, yi), img.get_pixel(xj, yi), img.get_pixel(xi, yj), img.get_pixel(xj, yj));
    Some(std::array::from_fn(|k| {
        let top = a[k] as f32 * (1.0 - fx) + b[k] as f32 * fx;
        let bot = c[k] as f32 * (1.0 - fx) + d[k] as f32 * fx;
        top * (1.0 - fy) + bot * fy
    }))
}

pub(crate) fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let (x0, y0) = (a[0].max(b[0]), a[1].max(b[1]));
    let (x1, y1) = ((a[0] + a[2]).min(b[0] + b[2]), (a[1] + a[3]).min(b[1] + b[3]));
    let inter = (x1 - x0).max(0.0) * (y1 - y0).max(0.0);
    inter / (a[2] * a[3] + b[2] * b[3] - inter)
}

fn nms(mut v: Vec<Detection>, thr: f32) -> Vec<Detection> {
    v.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut keep: Vec<Detection> = vec![];
    for d in v {
        if keep.iter().all(|k| iou(&k.bbox, &d.bbox) < thr) {
            keep.push(d);
        }
    }
    keep
}

/// Landmarks the blendshape model reads (MediaPipe `kLandmarksSubsetIdxs`).
const BLEND_SUBSET: [usize; 146] = [
    0, 1, 4, 5, 6, 7, 8, 10, 13, 14, 17, 21, 33, 37, 39, 40, 46, 52, 53, 54, 55, 58, 61, 63, 65, 66, 67, 70, 78, 80,
    81, 82, 84, 87, 88, 91, 93, 95, 103, 105, 107, 109, 127, 132, 133, 136, 144, 145, 146, 148, 149, 150, 152, 153,
    154, 155, 157, 158, 159, 160, 161, 162, 163, 168, 172, 173, 176, 178, 181, 185, 191, 195, 197, 234, 246, 249,
    251, 263, 267, 269, 270, 276, 282, 283, 284, 285, 288, 291, 293, 295, 296, 297, 300, 308, 310, 311, 312, 314,
    317, 318, 321, 323, 324, 332, 334, 336, 338, 356, 361, 362, 365, 373, 374, 375, 377, 378, 379, 380, 381, 382,
    384, 385, 386, 387, 388, 389, 390, 397, 398, 400, 402, 405, 409, 415, 454, 466, 468, 469, 470, 471, 472, 473,
    474, 475, 476, 477,
];

/// MediaPipe blendshape order (`kBlendshapeNames`).
pub const BLENDSHAPES: [&str; 52] = [
    "_neutral", "browDownLeft", "browDownRight", "browInnerUp", "browOuterUpLeft", "browOuterUpRight", "cheekPuff",
    "cheekSquintLeft", "cheekSquintRight", "eyeBlinkLeft", "eyeBlinkRight", "eyeLookDownLeft", "eyeLookDownRight",
    "eyeLookInLeft", "eyeLookInRight", "eyeLookOutLeft", "eyeLookOutRight", "eyeLookUpLeft", "eyeLookUpRight",
    "eyeSquintLeft", "eyeSquintRight", "eyeWideLeft", "eyeWideRight", "jawForward", "jawLeft", "jawOpen", "jawRight",
    "mouthClose", "mouthDimpleLeft", "mouthDimpleRight", "mouthFrownLeft", "mouthFrownRight", "mouthFunnel",
    "mouthLeft", "mouthLowerDownLeft", "mouthLowerDownRight", "mouthPressLeft", "mouthPressRight", "mouthPucker",
    "mouthRight", "mouthRollLower", "mouthRollUpper", "mouthShrugLower", "mouthShrugUpper", "mouthSmileLeft",
    "mouthSmileRight", "mouthStretchLeft", "mouthStretchRight", "mouthUpperUpLeft", "mouthUpperUpRight",
    "noseSneerLeft", "noseSneerRight",
];

pub fn blend(mesh: &Mesh, name: &str) -> f32 {
    BLENDSHAPES.iter().position(|n| *n == name).and_then(|i| mesh.blend.get(i).copied()).unwrap_or(0.0)
}

fn f32s(t: &TValue) -> Result<&[f32]> {
    Ok(t.try_as_plain_ram()?.as_slice::<f32>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similarity_recovers_scale_rotation_and_shift() {
        let (a, b, tx, ty) = (1.5f32 * 0.8, 1.5f32 * 0.6, 40.0, -7.0);
        let to = TEMPLATE.map(|[x, y]| [a * x - b * y + tx, b * x + a * y + ty]);
        let got = similarity(&TEMPLATE, &to);
        for (g, w) in got.iter().zip([a, b, tx, ty]) {
            assert!((g - w).abs() < 1e-3, "{got:?}");
        }
        let id = similarity(&TEMPLATE, &TEMPLATE);
        assert!((id[0] - 1.0).abs() < 1e-5 && id[1].abs() < 1e-5 && id[2].abs() < 1e-3 && id[3].abs() < 1e-3);
    }
}
