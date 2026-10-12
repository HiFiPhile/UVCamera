// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

//! Video formats advertised to the host.
//!
//! A [`FormatSpec`] is what we ask configfs to create. A [`FormatTable`] is
//! the same thing after the kernel assigned `bFormatIndex`/`bFrameIndex`,
//! which is what UVC requests refer to.

use std::{fmt, str::FromStr};

use anyhow::bail;

/// Frame interval in 100 ns units, as used throughout UVC.
pub type Interval = u32;

pub const fn fps_to_interval(fps: u32) -> Interval {
    10_000_000 / fps
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatKind {
    Mjpeg,
    /// Uncompressed YUY2 (packed 4:2:2).
    Yuyv,
}

impl FormatKind {
    /// V4L2 fourcc passed to `VIDIOC_S_FMT`.
    pub fn fourcc(self) -> u32 {
        u32::from_le_bytes(match self {
            Self::Mjpeg => *b"MJPG",
            Self::Yuyv => *b"YUYV",
        })
    }

    /// Format directory under `streaming/` in configfs.
    pub fn configfs_dir(self) -> &'static str {
        match self {
            Self::Mjpeg => "mjpeg",
            Self::Yuyv => "uncompressed",
        }
    }

    /// Name of the single format instance we create under [`Self::configfs_dir`].
    pub fn configfs_name(self) -> &'static str {
        match self {
            Self::Mjpeg => "m",
            Self::Yuyv => "u",
        }
    }

    /// Upper bound on the size of one frame.
    pub fn max_frame_size(self, width: u16, height: u16) -> u32 {
        // YUYV is exactly 2 bytes/pixel. MJPEG is bounded by the same so a
        // worst-case camera frame always fits the V4L2 buffer.
        u32::from(width) * u32::from(height) * 2
    }
}

impl fmt::Display for FormatKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Mjpeg => "mjpeg",
            Self::Yuyv => "yuyv",
        })
    }
}

impl FromStr for FormatKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "mjpeg" => Ok(Self::Mjpeg),
            "yuyv" => Ok(Self::Yuyv),
            _ => bail!("Unknown format: {s:?} (expected mjpeg or yuyv)"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameSpec {
    pub width: u16,
    pub height: u16,
    /// Supported intervals, shortest (fastest) first. The first is the default.
    pub intervals: Vec<Interval>,
}

impl FrameSpec {
    pub fn new(width: u16, height: u16, fps: &[u32]) -> Self {
        let mut intervals = fps.iter().map(|&f| fps_to_interval(f)).collect::<Vec<_>>();
        intervals.sort_unstable();
        intervals.dedup();
        Self {
            width,
            height,
            intervals,
        }
    }

    pub fn default_interval(&self) -> Interval {
        self.intervals[0]
    }

    /// The supported interval closest to `requested`, or the default for 0.
    pub fn closest_interval(&self, requested: Interval) -> Interval {
        if requested == 0 {
            return self.default_interval();
        }
        *self
            .intervals
            .iter()
            .min_by_key(|&&i| i.abs_diff(requested))
            .expect("frame has no intervals")
    }

    /// Size of the configfs directory for this frame.
    pub fn configfs_name(&self) -> String {
        format!("{}x{}", self.width, self.height)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatSpec {
    pub kind: FormatKind,
    pub frames: Vec<FrameSpec>,
}

impl FormatSpec {
    /// Built-in frame list per format, sized for a USB 2.0 isochronous link
    /// (~24 MB/s): YUYV is limited to 640×480@30 and 1280×720@10.
    pub fn default_for(kind: FormatKind) -> Self {
        let frames = match kind {
            FormatKind::Mjpeg => vec![
                FrameSpec::new(1920, 1080, &[30, 15]),
                FrameSpec::new(1280, 720, &[30, 15]),
                FrameSpec::new(640, 480, &[30, 15]),
            ],
            FormatKind::Yuyv => vec![
                FrameSpec::new(640, 480, &[30, 15]),
                FrameSpec::new(1280, 720, &[10]),
            ],
        };
        Self { kind, frames }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub index: u8,
    pub spec: FrameSpec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Format {
    pub index: u8,
    pub kind: FormatKind,
    pub frames: Vec<Frame>,
}

impl Format {
    pub fn frame(&self, index: u8) -> Option<&Frame> {
        self.frames.iter().find(|f| f.index == index)
    }
}

/// Formats as enumerated to the host, sorted by index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatTable {
    formats: Vec<Format>,
}

impl FormatTable {
    pub fn new(mut formats: Vec<Format>) -> anyhow::Result<Self> {
        if formats.is_empty() || formats.iter().any(|f| f.frames.is_empty()) {
            bail!("Format table must have at least one format and one frame per format");
        }
        formats.sort_by_key(|f| f.index);
        for format in &mut formats {
            format.frames.sort_by_key(|f| f.index);
        }
        Ok(Self { formats })
    }

    /// Assign indexes in declaration order, which is how the kernel numbers
    /// them when formats and frames are created and linked in order.
    #[cfg(test)]
    pub fn from_specs(specs: &[FormatSpec]) -> anyhow::Result<Self> {
        Self::new(
            specs
                .iter()
                .zip(1..)
                .map(|(spec, index)| Format {
                    index,
                    kind: spec.kind,
                    frames: spec
                        .frames
                        .iter()
                        .zip(1..)
                        .map(|(spec, index)| Frame {
                            index,
                            spec: spec.clone(),
                        })
                        .collect(),
                })
                .collect(),
        )
    }

    pub fn format(&self, index: u8) -> Option<&Format> {
        self.formats.iter().find(|f| f.index == index)
    }

    pub fn first(&self) -> (&Format, &Frame) {
        let format = &self.formats[0];
        (format, &format.frames[0])
    }

    pub fn last(&self) -> (&Format, &Frame) {
        let format = self.formats.last().unwrap();
        (format, format.frames.last().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closest_interval() {
        let frame = FrameSpec::new(640, 480, &[15, 30]);
        assert_eq!(frame.intervals, vec![333_333, 666_666]);
        assert_eq!(frame.closest_interval(0), 333_333);
        assert_eq!(frame.closest_interval(400_000), 333_333);
        assert_eq!(frame.closest_interval(600_000), 666_666);
        assert_eq!(frame.closest_interval(10_000_000), 666_666);
    }

    #[test]
    fn table_from_specs_numbers_from_one() {
        let table = FormatTable::from_specs(&[
            FormatSpec::default_for(FormatKind::Mjpeg),
            FormatSpec::default_for(FormatKind::Yuyv),
        ])
        .unwrap();
        let yuyv = table.format(2).unwrap();
        assert_eq!(yuyv.kind, FormatKind::Yuyv);
        assert_eq!(yuyv.frame(2).unwrap().spec.width, 1280);
        assert_eq!(table.first().1.spec.width, 1920);
        assert_eq!(table.last().1.spec.height, 720);
    }
}
