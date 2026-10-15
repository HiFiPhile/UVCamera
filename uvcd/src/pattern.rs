// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

//! Moving test pattern used to bring up the gadget without a camera.
//!
//! Top: eight color bars. Bottom: a white box sweeping left to right and a
//! two-digit counter, both cycling every [`LOOP_FRAMES`] frames, so stutter
//! and dropped frames are visible on the host.

use anyhow::{Context, Result};
use jpeg_encoder::{ColorType, Encoder};

use crate::uvc::format::FormatKind;

/// Pattern period in frames; MJPEG frames are encoded once and cached.
pub const LOOP_FRAMES: usize = 60;
const JPEG_QUALITY: u8 = 80;

const BARS: [[u8; 3]; 8] = [
    [235, 235, 235],
    [235, 235, 16],
    [16, 235, 235],
    [16, 235, 16],
    [235, 16, 235],
    [235, 16, 16],
    [16, 16, 235],
    [16, 16, 16],
];

/// 3×5 bitmap digits, one row per u8 (bits 2..0 = left..right).
const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b010, 0b010, 0b010],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
];

pub struct TestPattern {
    /// Draw the color bars right to left, to tell an app-fed pattern
    /// (`uvcd client source`) from the daemon's own placeholder.
    reversed: bool,
    kind: FormatKind,
    width: usize,
    height: usize,
    rgb: Vec<u8>,
    yuyv: Vec<u8>,
    jpeg_cache: Vec<Option<Vec<u8>>>,
}

impl TestPattern {
    pub fn new(kind: FormatKind, width: u16, height: u16) -> Self {
        Self {
            reversed: false,
            kind,
            width: width.into(),
            height: height.into(),
            rgb: vec![0; usize::from(width) * usize::from(height) * 3],
            yuyv: vec![],
            jpeg_cache: vec![None; LOOP_FRAMES],
        }
    }

    /// Variant with the color bars in reverse order.
    pub fn reversed(mut self) -> Self {
        self.reversed = true;
        self
    }

    /// The encoded frame for sequence number `n`.
    pub fn frame(&mut self, n: u64) -> Result<&[u8]> {
        let phase = (n % LOOP_FRAMES as u64) as usize;

        match self.kind {
            FormatKind::Mjpeg => {
                if self.jpeg_cache[phase].is_none() {
                    self.render(phase);
                    let mut jpeg = Vec::with_capacity(self.width * self.height / 4);
                    Encoder::new(&mut jpeg, JPEG_QUALITY)
                        .encode(
                            &self.rgb,
                            self.width as u16,
                            self.height as u16,
                            ColorType::Rgb,
                        )
                        .context("Failed to encode test pattern")?;
                    self.jpeg_cache[phase] = Some(jpeg);
                }
                Ok(self.jpeg_cache[phase].as_deref().unwrap())
            }
            FormatKind::Yuyv => {
                self.render(phase);
                rgb_to_yuyv(&self.rgb, &mut self.yuyv);
                Ok(&self.yuyv)
            }
            FormatKind::H264 => anyhow::bail!("There is no H.264 test pattern"),
        }
    }

    fn fill_rect(&mut self, x0: usize, y0: usize, w: usize, h: usize, color: [u8; 3]) {
        let x1 = (x0 + w).min(self.width);
        let y1 = (y0 + h).min(self.height);
        for y in y0..y1 {
            let row = &mut self.rgb[(y * self.width + x0) * 3..(y * self.width + x1) * 3];
            for px in row.as_chunks_mut::<3>().0 {
                px.copy_from_slice(&color);
            }
        }
    }

    fn render(&mut self, phase: usize) {
        let (w, h) = (self.width, self.height);
        let split = h * 2 / 3;

        for (i, color) in BARS.iter().enumerate() {
            let i = if self.reversed { BARS.len() - 1 - i } else { i };
            let x0 = w * i / BARS.len();
            let x1 = w * (i + 1) / BARS.len();
            self.fill_rect(x0, 0, x1 - x0, split, *color);
        }
        self.fill_rect(0, split, w, h - split, [32, 32, 32]);

        // Sweeping box.
        let box_size = (h - split) / 2;
        let x = (w - box_size) * phase / (LOOP_FRAMES - 1);
        self.fill_rect(
            x,
            split + (h - split - box_size) / 2,
            box_size,
            box_size,
            [235, 235, 235],
        );

        // Counter, centered over the bars' lower edge.
        let scale = (h / 12).max(1);
        let digits = [phase / 10, phase % 10];
        let text_w = (digits.len() * 4 - 1) * scale;
        let ox = (w - text_w) / 2;
        let oy = split - 6 * scale;
        self.fill_rect(
            ox - scale,
            oy - scale,
            text_w + 2 * scale,
            7 * scale,
            [16, 16, 16],
        );
        for (i, &d) in digits.iter().enumerate() {
            for (row, bits) in DIGITS[d].iter().enumerate() {
                for col in 0..3 {
                    if bits & (0b100 >> col) != 0 {
                        self.fill_rect(
                            ox + (i * 4 + col) * scale,
                            oy + row * scale,
                            scale,
                            scale,
                            [235, 235, 235],
                        );
                    }
                }
            }
        }
    }
}

/// BT.601 limited-range RGB → packed YUYV. Chroma is the average of each
/// horizontal pixel pair. `width` must be even.
fn rgb_to_yuyv(rgb: &[u8], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(rgb.len() / 3 * 2);

    for pair in rgb.as_chunks::<6>().0 {
        let (r0, g0, b0) = (i32::from(pair[0]), i32::from(pair[1]), i32::from(pair[2]));
        let (r1, g1, b1) = (i32::from(pair[3]), i32::from(pair[4]), i32::from(pair[5]));

        let y = |r: i32, g: i32, b: i32| (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8;
        let (r, g, b) = ((r0 + r1) / 2, (g0 + g1) / 2, (b0 + b1) / 2);
        let u = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128) as u8;
        let v = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128) as u8;

        out.extend_from_slice(&[y(r0, g0, b0), u, y(r1, g1, b1), v]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yuyv_frame_size_and_white() {
        let mut p = TestPattern::new(FormatKind::Yuyv, 640, 480);
        let frame = p.frame(0).unwrap();
        assert_eq!(frame.len(), 640 * 480 * 2);
        // Top-left bar is (235, 235, 235): Y=218, U=V=128.
        assert_eq!(&frame[..4], &[218, 128, 218, 128]);
    }

    #[test]
    fn mjpeg_frames_are_cached_per_phase() {
        let mut p = TestPattern::new(FormatKind::Mjpeg, 320, 240);
        let first = p.frame(1).unwrap().to_vec();
        assert_eq!(&first[..2], &[0xff, 0xd8]);
        assert_eq!(p.frame(1 + LOOP_FRAMES as u64).unwrap(), first.as_slice());
        assert_ne!(p.frame(2).unwrap(), first.as_slice());
    }
}
