use image::{DynamicImage, ImageDecoder, ImageReader};

use super::{CubeGrid, ReferenceMatch, ReferenceMatchReport, sample_tetrahedral};
use crate::error::{LutError, Result, io_error};
use crate::raster::is_srgb_profile;
use std::path::Path;

const MAX_SAMPLED_PIXELS: f64 = 400_000.0;
const MAX_NODE_OFFSET: f64 = 0.15;
const VALIDATION_PERIOD: usize = 5;

#[derive(Clone, Copy)]
struct Pair {
    original: [f64; 3],
    graded: [f64; 3],
}

#[derive(Clone, Copy)]
struct ErrorMetric {
    all: f64,
    dark: f64,
}

struct ReferenceImage {
    dimensions: (u32, u32),
    samples: Vec<[f64; 3]>,
    assumed_srgb: bool,
}

pub(super) fn fit_reference(
    grid: &mut CubeGrid,
    reference: &ReferenceMatch,
) -> Result<ReferenceMatchReport> {
    let original = read_reference(&reference.original_photo)?;
    let graded = read_reference(&reference.graded_photo)?;
    if original.dimensions != graded.dimensions {
        return Err(LutError::InvalidRaster(format!(
            "参考照片尺寸不一致：原图 {}×{}，直接调色图 {}×{}；请使用相同构图和尺寸的照片",
            original.dimensions.0, original.dimensions.1, graded.dimensions.0, graded.dimensions.1
        )));
    }
    let assumed_srgb = original.assumed_srgb || graded.assumed_srgb;
    let pairs: Vec<Pair> = original
        .samples
        .into_iter()
        .zip(graded.samples)
        .map(|(original, graded)| Pair { original, graded })
        .collect();
    fit_pairs(grid, &pairs, assumed_srgb)
}

pub(super) fn fit_reference_samples(
    grid: &mut CubeGrid,
    width: u32,
    height: u32,
    original: &[[u16; 3]],
    graded: &[[u16; 3]],
    assumed_srgb: bool,
) -> Result<ReferenceMatchReport> {
    let expected = width as usize * height as usize;
    if original.len() != expected || graded.len() != expected || width == 0 || height == 0 {
        return Err(LutError::InvalidRaster(
            "校准母版中的原图与调色图有效区域尺寸不一致；请重新生成并导回母版".to_owned(),
        ));
    }
    let stride = ((expected as f64 / MAX_SAMPLED_PIXELS).sqrt().ceil() as usize).max(1);
    let mut pairs = Vec::with_capacity(expected.div_ceil(stride * stride));
    for y in (0..height as usize).step_by(stride) {
        for x in (0..width as usize).step_by(stride) {
            let index = y * width as usize + x;
            pairs.push(Pair {
                original: original[index].map(|component| f64::from(component) / 65535.0),
                graded: graded[index].map(|component| f64::from(component) / 65535.0),
            });
        }
    }
    fit_pairs(grid, &pairs, assumed_srgb)
}

fn fit_pairs(
    grid: &mut CubeGrid,
    pairs: &[Pair],
    assumed_srgb: bool,
) -> Result<ReferenceMatchReport> {
    if pairs.len() < 500 {
        return Err(LutError::InvalidRaster(
            "参考照片有效采样少于 500 个像素，无法可靠匹配".to_owned(),
        ));
    }

    let before = evaluate(grid, pairs);
    let baseline = grid.values.clone();
    let mut best = baseline.clone();
    let mut best_error = before;
    let mut best_clipped = 0;

    // Fit the residual of the actual photo pair on top of the neutral-chart
    // LUT. The holdout pixels prevent a local Camera Raw adjustment or JPEG
    // noise from silently being treated as an exact, universal 3D transform.
    for _ in 0..6 {
        let mut sums = vec![[0.0_f64; 3]; grid.values.len()];
        let mut support = vec![0.0_f64; grid.values.len()];
        for (number, pair) in pairs.iter().enumerate() {
            if number % VALIDATION_PERIOD == 0 {
                continue;
            }
            let predicted =
                sample_tetrahedral(grid, pair.original[0], pair.original[1], pair.original[2]);
            let vertices = vertices(grid.size, pair.original);
            for (index, weight) in vertices {
                if weight <= 0.0 {
                    continue;
                }
                support[index] += weight;
                for channel in 0..3 {
                    sums[index][channel] += weight * (pair.graded[channel] - predicted[channel]);
                }
            }
        }

        let mut clipped = 0;
        for (index, node) in grid.values.iter_mut().enumerate() {
            let count = support[index];
            if count < 0.5 {
                continue;
            }
            let reliability = count / (count + 4.0);
            let mut clipped_here = false;
            for channel in 0..3 {
                let step = 0.7 * reliability * sums[index][channel] / count;
                let proposed = (node[channel] + step).clamp(
                    baseline[index][channel] - MAX_NODE_OFFSET,
                    baseline[index][channel] + MAX_NODE_OFFSET,
                );
                clipped_here |= !(0.0..=1.0).contains(&proposed);
                node[channel] = proposed.clamp(0.0, 1.0);
            }
            clipped += usize::from(clipped_here);
        }
        let current = evaluate(grid, pairs);
        if current.all + 0.005 < best_error.all {
            best_error = current;
            best.clone_from(&grid.values);
            best_clipped = clipped;
        } else {
            break;
        }
    }

    grid.values = best;
    let mut corrected_nodes = 0;
    let mut maximum_offset = 0.0_f64;
    for (index, node) in grid.values.iter_mut().enumerate() {
        let mut changed = false;
        for channel in 0..3 {
            node[channel] = (node[channel] * 1_000_000.0).round() / 1_000_000.0;
            let offset = (node[channel] - baseline[index][channel]).abs();
            changed |= offset > 0.000001;
            maximum_offset = maximum_offset.max(offset);
        }
        corrected_nodes += usize::from(changed);
    }
    let after = evaluate(grid, pairs);
    Ok(ReferenceMatchReport {
        sampled_pixels: pairs.len(),
        validation_pixels: pairs.len().div_ceil(VALIDATION_PERIOD),
        before_mae_8bit: before.all,
        after_mae_8bit: after.all,
        dark_before_mae_8bit: before.dark,
        dark_after_mae_8bit: after.dark,
        corrected_nodes,
        maximum_offset,
        clipped_nodes: best_clipped,
        assumed_srgb,
    })
}

fn read_reference(path: &Path) -> Result<ReferenceImage> {
    let reader = ImageReader::open(path)
        .map_err(|error| io_error(path, error))?
        .with_guessed_format()
        .map_err(|error| io_error(path, error))?;
    let mut decoder = reader.into_decoder()?;
    let color = decoder.original_color_type();
    if !matches!(
        color,
        image::ExtendedColorType::Rgb8 | image::ExtendedColorType::Rgb16
    ) {
        return Err(LutError::InvalidRaster(format!(
            "参考照片 {} 为 {color:?}；请选择 RGB8/RGB16 照片",
            path.display()
        )));
    }
    let icc = decoder.icc_profile()?;
    let is_jpeg = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "jpg" | "jpeg"));
    if icc.is_none() && !is_jpeg {
        return Err(LutError::InvalidRaster(format!(
            "参考照片 {} 缺少 ICC；请嵌入 sRGB 配置文件",
            path.display()
        )));
    }
    if icc
        .as_deref()
        .is_some_and(|profile| !is_srgb_profile(profile))
    {
        return Err(LutError::InvalidRaster(format!(
            "参考照片 {} 不是 sRGB；请先转换为 sRGB 再匹配",
            path.display()
        )));
    }
    let orientation = decoder.orientation()?;
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    let dimensions = (image.width(), image.height());
    let stride = ((f64::from(dimensions.0) * f64::from(dimensions.1) / MAX_SAMPLED_PIXELS)
        .sqrt()
        .ceil() as usize)
        .max(1);
    let mut samples = Vec::new();
    match image {
        DynamicImage::ImageRgb8(rgb) => {
            for y in (0..dimensions.1).step_by(stride) {
                for x in (0..dimensions.0).step_by(stride) {
                    samples.push(rgb.get_pixel(x, y).0.map(|value| f64::from(value) / 255.0));
                }
            }
        }
        DynamicImage::ImageRgb16(rgb) => {
            for y in (0..dimensions.1).step_by(stride) {
                for x in (0..dimensions.0).step_by(stride) {
                    samples.push(
                        rgb.get_pixel(x, y)
                            .0
                            .map(|value| f64::from(value) / 65535.0),
                    );
                }
            }
        }
        _ => unreachable!("original_color_type checked above"),
    }
    Ok(ReferenceImage {
        dimensions,
        samples,
        assumed_srgb: icc.is_none(),
    })
}

fn evaluate(grid: &CubeGrid, pairs: &[Pair]) -> ErrorMetric {
    let mut all_sum = 0.0;
    let mut dark_sum = 0.0;
    let mut all_count = 0;
    let mut dark_count = 0;
    for (index, pair) in pairs.iter().enumerate() {
        if index % VALIDATION_PERIOD != 0 {
            continue;
        }
        let prediction =
            sample_tetrahedral(grid, pair.original[0], pair.original[1], pair.original[2]);
        let luminance =
            0.2126 * pair.original[0] + 0.7152 * pair.original[1] + 0.0722 * pair.original[2];
        for (predicted, graded) in prediction.iter().zip(pair.graded) {
            let error = 255.0 * (predicted - graded).abs();
            all_sum += error;
            all_count += 1;
            if luminance < 0.25 {
                dark_sum += error;
                dark_count += 1;
            }
        }
    }
    ErrorMetric {
        all: all_sum / f64::from(all_count),
        dark: if dark_count > 0 {
            dark_sum / f64::from(dark_count)
        } else {
            0.0
        },
    }
}

fn vertices(size: usize, rgb: [f64; 3]) -> [(usize, f64); 4] {
    let point = rgb.map(|component| component.clamp(0.0, 1.0) * (size - 1) as f64);
    let mut corner = point.map(|value| (value.floor() as usize).min(size - 2));
    let fraction = std::array::from_fn::<_, 3, _>(|axis| point[axis] - corner[axis] as f64);
    let mut axes = [0, 1, 2];
    axes.sort_by(|left, right| fraction[*right].total_cmp(&fraction[*left]));
    let weights = [
        1.0 - fraction[axes[0]],
        fraction[axes[0]] - fraction[axes[1]],
        fraction[axes[1]] - fraction[axes[2]],
        fraction[axes[2]],
    ];
    let mut output = [(0, 0.0); 4];
    for step in 0..4 {
        if step > 0 {
            corner[axes[step - 1]] += 1;
        }
        output[step] = (
            corner[2] * size * size + corner[1] * size + corner[0],
            weights[step],
        );
    }
    output
}
