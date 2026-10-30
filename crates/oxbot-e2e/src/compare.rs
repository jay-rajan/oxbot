//! Screenshots against their approved baselines: the same, changed (with
//! a picture of what changed), or new.

use std::path::Path;

use anyhow::{Context, bail};
use serde::Serialize;

/// How a screenshot compares with its baseline.
#[derive(Debug, Clone, Serialize)]
pub struct Comparison {
    /// `same`, `changed` or `new` (no baseline yet).
    pub status: &'static str,
    /// Share of pixels that differ, in percent.
    pub percent: f64,
    /// The baseline, relative to the run folder (copied there).
    pub baseline: Option<String>,
    /// A picture with the differences in red, relative to the run folder.
    pub diff: Option<String>,
}

/// More than this share of pixels must differ to count as changed (a few
/// pixels move with text anti-aliasing).
const THRESHOLD: f64 = 0.15;

struct Image {
    w: u32,
    h: u32,
    rgba: Vec<u8>,
}

fn decode(bytes: &[u8]) -> anyhow::Result<Image> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size().context("image too big")?];
    let info = reader.next_frame(&mut buf)?;
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .chunks(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf
            .chunks(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Indexed => bail!("unexpected indexed image"),
    };
    Ok(Image {
        w: info.width,
        h: info.height,
        rgba,
    })
}

fn encode(img: &Image, path: &Path) -> anyhow::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), img.w, img.h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&img.rgba)?;
    Ok(())
}

/// Compares the screenshot `current` with `baseline` (if it exists).
/// Copies the baseline next to the run as `<name>.base.png` and, when they
/// differ, writes `<name>.diff.png`.
///
/// # Errors
/// Fails if an image can't be read or written.
pub fn compare(run_dir: &Path, current_rel: &str, baseline: &Path) -> anyhow::Result<Comparison> {
    if !baseline.exists() {
        return Ok(Comparison {
            status: "new",
            percent: 100.0,
            baseline: None,
            diff: None,
        });
    }
    let current_path = run_dir.join(current_rel);
    let now = decode(&std::fs::read(&current_path)?)?;
    let base = decode(&std::fs::read(baseline)?)?;
    let base_rel = current_rel.replace(".png", ".base.png");
    std::fs::copy(baseline, run_dir.join(&base_rel))?;
    if now.w != base.w || now.h != base.h {
        return Ok(Comparison {
            status: "changed",
            percent: 100.0,
            baseline: Some(base_rel),
            diff: None,
        });
    }
    let mut differ = 0u64;
    let mut out = Vec::with_capacity(now.rgba.len());
    for (a, b) in now.rgba.chunks(4).zip(base.rgba.chunks(4)) {
        let delta = a
            .iter()
            .zip(b)
            .take(3)
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(0);
        if delta > 24 {
            differ += 1;
            out.extend_from_slice(&[230, 30, 30, 255]);
        } else {
            // The unchanged page, faded, so the red stands out.
            out.extend(a.iter().take(3).map(|c| 255 - (255 - c) / 4));
            out.push(255);
        }
    }
    let total = u64::from(now.w) * u64::from(now.h);
    #[allow(
        clippy::cast_precision_loss,
        reason = "a percentage for people to read"
    )]
    let percent = (differ as f64) * 100.0 / (total.max(1) as f64);
    if percent <= THRESHOLD {
        return Ok(Comparison {
            status: "same",
            percent,
            baseline: Some(base_rel),
            diff: None,
        });
    }
    let diff_rel = current_rel.replace(".png", ".diff.png");
    encode(
        &Image {
            w: now.w,
            h: now.h,
            rgba: out,
        },
        &run_dir.join(&diff_rel),
    )?;
    Ok(Comparison {
        status: "changed",
        percent,
        baseline: Some(base_rel),
        diff: Some(diff_rel),
    })
}
