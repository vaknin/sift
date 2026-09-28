//! SIMD resizing (fast_image_resize); image's own resize is the slowest step
//! on 24 MP previews.

use fast_image_resize::{FilterType, ResizeAlg, ResizeOptions, Resizer};
use image::RgbImage;

/// Resize `src` (optionally only the region x, y, w, h) to `w` × `h`.
pub fn rgb(src: &RgbImage, crop: Option<[f64; 4]>, w: u32, h: u32) -> RgbImage {
    let mut dst = RgbImage::new(w.max(1), h.max(1));
    let mut opts = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear));
    if let Some([x, y, cw, ch]) = crop {
        opts = opts.crop(x, y, cw, ch);
    }
    Resizer::new().resize(src, &mut dst, &opts).expect("RGB8 to RGB8 resize");
    dst
}
