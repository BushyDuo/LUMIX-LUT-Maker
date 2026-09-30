use super::{ConsistencyMode, ConsistencyReport, CubeGrid, sample_tetrahedral};

const MAX_DIAGNOSTIC_EDGE: usize = 256;

pub(super) fn diagnose(
    grid: &CubeGrid,
    original: &[[u16; 3]],
    graded: &[[u16; 3]],
    width: u32,
    height: u32,
    mode: ConsistencyMode,
) -> ConsistencyReport {
    let mut report = ConsistencyReport {
        mode,
        heldout_mae_8bit: None,
        heldout_delta_e76: None,
        edge_ssim: None,
        estimated_offset_pixels: None,
        feature_matches: None,
        ransac_inliers: None,
        estimated_rotation_degrees: None,
        estimated_scale_percent: None,
        warnings: Vec::new(),
    };
    if original.len() != graded.len() || original.len() != width as usize * height as usize {
        report
            .warnings
            .push("一致性诊断未运行：照片有效区域尺寸不一致。".to_owned());
        return report;
    }

    if matches!(
        mode,
        ConsistencyMode::Comprehensive | ConsistencyMode::ColorMapping
    ) {
        let stride = ((original.len() as f64 / 100_000.0).sqrt().ceil() as usize).max(1);
        let mut absolute_error = 0.0;
        let mut delta_e = 0.0;
        let mut count = 0usize;
        for y in (0..height as usize).step_by(stride) {
            for x in (0..width as usize).step_by(stride) {
                let index = y * width as usize + x;
                // Use held-out pixels only, matching the reference-fit validation cadence.
                if count.is_multiple_of(5) {
                    let input = original[index].map(|v| f64::from(v) / 65535.0);
                    let expected = graded[index].map(|v| f64::from(v) / 65535.0);
                    let prediction = sample_tetrahedral(grid, input[0], input[1], input[2]);
                    let error: [f64; 3] =
                        std::array::from_fn(|channel| prediction[channel] - expected[channel]);
                    absolute_error += error.iter().map(|value| value.abs()).sum::<f64>() / 3.0;
                    delta_e += delta_e_76(prediction, expected);
                }
                count += 1;
            }
        }
        let samples = count.div_ceil(5).max(1) as f64;
        report.heldout_mae_8bit = Some(absolute_error * 255.0 / samples);
        report.heldout_delta_e76 = Some(delta_e / samples);
        if report.heldout_mae_8bit.is_some_and(|value| value > 5.0) {
            report.warnings.push("颜色映射留出误差偏高：局部调整、空间效果或色域/色彩管理变化可能无法由单个 3D LUT 表示。".to_owned());
        }
    }

    if matches!(
        mode,
        ConsistencyMode::Comprehensive | ConsistencyMode::GeometryStructure
    ) {
        let (gray_a, gray_b, diagnostic_width, diagnostic_height) =
            gray_maps(original, graded, width as usize, height as usize);
        let edge_a = gradient_magnitude(&gray_a, diagnostic_width, diagnostic_height);
        let edge_b = gradient_magnitude(&gray_b, diagnostic_width, diagnostic_height);
        report.edge_ssim = Some(edge_ssim(
            &edge_a,
            &edge_b,
            diagnostic_width,
            diagnostic_height,
        ));
        let feature_result =
            estimate_orb_like_affine_ransac(&gray_a, &gray_b, diagnostic_width, diagnostic_height);
        let (offset_x, offset_y, score) = if let Some(result) = feature_result {
            report.feature_matches = Some(result.matches);
            report.ransac_inliers = Some(result.inliers);
            report.estimated_rotation_degrees = Some(result.rotation_degrees);
            report.estimated_scale_percent = Some(result.scale_percent);
            (
                result.offset_x,
                result.offset_y,
                result.inliers as f64 / result.matches.max(1) as f64,
            )
        } else {
            report.warnings.push(
                "ORB 风格特征不足，位置估计退回归一化梯度相关；平滑或低纹理照片的置信度有限。"
                    .to_owned(),
            );
            estimate_offset(&edge_a, &edge_b, diagnostic_width, diagnostic_height)
        };
        if score >= 0.15 {
            let scale_x = width as f64 / diagnostic_width.max(1) as f64;
            let scale_y = height as f64 / diagnostic_height.max(1) as f64;
            report.estimated_offset_pixels = Some((offset_x * scale_x, offset_y * scale_y));
            if offset_x.abs() > 0.75
                || offset_y.abs() > 0.75
                || report
                    .estimated_rotation_degrees
                    .is_some_and(|angle| angle.abs() > 0.25)
                || report
                    .estimated_scale_percent
                    .is_some_and(|scale| (scale - 100.0).abs() > 0.5)
            {
                report.warnings.push("检测到疑似位移；诊断不会自动配准或扭曲图像。请确认导出未改变裁切、画布位置或尺寸。".to_owned());
            }
        } else {
            report
                .warnings
                .push("结构特征不足以可靠估算偏移；请人工确认照片构图是否保持一致。".to_owned());
        }
        if report.edge_ssim.is_some_and(|value| value < 0.90) {
            report.warnings.push(
                "边缘结构相似度较低：可能存在裁切、局部几何变化、蒙版或空间效果。".to_owned(),
            );
        }
    }
    report
}

fn gray_maps(
    original: &[[u16; 3]],
    graded: &[[u16; 3]],
    width: usize,
    height: usize,
) -> (Vec<f64>, Vec<f64>, usize, usize) {
    let scale = (MAX_DIAGNOSTIC_EDGE as f64 / width.max(height) as f64).min(1.0);
    let out_width = ((width as f64 * scale).round() as usize).max(3);
    let out_height = ((height as f64 * scale).round() as usize).max(3);
    let mut first = vec![0.0; out_width * out_height];
    let mut second = vec![0.0; out_width * out_height];
    for y in 0..out_height {
        let source_y = (y * height / out_height).min(height - 1);
        for x in 0..out_width {
            let source_x = (x * width / out_width).min(width - 1);
            let source_index = source_y * width + source_x;
            let index = y * out_width + x;
            first[index] = luminance(original[source_index]);
            second[index] = luminance(graded[source_index]);
        }
    }
    normalize(&mut first);
    normalize(&mut second);
    (first, second, out_width, out_height)
}

fn normalize(values: &mut [f64]) {
    let mean = values.iter().sum::<f64>() / values.len().max(1) as f64;
    let deviation = (values
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / values.len().max(1) as f64)
        .sqrt()
        .max(1.0e-6);
    for value in values {
        *value = (*value - mean) / deviation;
    }
}

fn gradient_magnitude(values: &[f64], width: usize, height: usize) -> Vec<f64> {
    let mut output = vec![0.0; values.len()];
    for y in 1..height - 1 {
        for x in 1..width - 1 {
            let dx = values[y * width + x + 1] - values[y * width + x - 1];
            let dy = values[(y + 1) * width + x] - values[(y - 1) * width + x];
            output[y * width + x] = (dx * dx + dy * dy).sqrt();
        }
    }
    normalize(&mut output);
    output
}

fn edge_ssim(a: &[f64], b: &[f64], width: usize, height: usize) -> f64 {
    let (mut total, mut count) = (0.0, 0usize);
    const C1: f64 = 0.0001;
    const C2: f64 = 0.0009;
    for y in 3..height.saturating_sub(3) {
        for x in 3..width.saturating_sub(3) {
            let mut sum_a = 0.0;
            let mut sum_b = 0.0;
            let mut sum_aa = 0.0;
            let mut sum_bb = 0.0;
            let mut sum_ab = 0.0;
            let mut n = 0.0;
            for dy in -2_isize..=2 {
                for dx in -2_isize..=2 {
                    let index = (y as isize + dy) as usize * width + (x as isize + dx) as usize;
                    sum_a += a[index];
                    sum_b += b[index];
                    sum_aa += a[index] * a[index];
                    sum_bb += b[index] * b[index];
                    sum_ab += a[index] * b[index];
                    n += 1.0;
                }
            }
            let mean_a = sum_a / n;
            let mean_b = sum_b / n;
            let variance_a = (sum_aa / n - mean_a * mean_a).max(0.0);
            let variance_b = (sum_bb / n - mean_b * mean_b).max(0.0);
            let covariance = sum_ab / n - mean_a * mean_b;
            total += ((2.0 * mean_a * mean_b + C1) * (2.0 * covariance + C2))
                / ((mean_a * mean_a + mean_b * mean_b + C1) * (variance_a + variance_b + C2));
            count += 1;
        }
    }
    total / count.max(1) as f64
}

fn estimate_offset(a: &[f64], b: &[f64], width: usize, height: usize) -> (f64, f64, f64) {
    let range_x = (width / 24).clamp(2, 10) as isize;
    let range_y = (height / 24).clamp(2, 10) as isize;
    let mut best = (0, 0, f64::NEG_INFINITY);
    for dy in -range_y..=range_y {
        for dx in -range_x..=range_x {
            let mut dot = 0.0;
            let mut aa = 0.0;
            let mut bb = 0.0;
            let x_start = dx.max(0) as usize;
            let y_start = dy.max(0) as usize;
            let x_end = (width as isize + dx).min(width as isize) as usize;
            let y_end = (height as isize + dy).min(height as isize) as usize;
            for y in y_start..y_end {
                let other_y = (y as isize - dy) as usize;
                for x in x_start..x_end {
                    let other_x = (x as isize - dx) as usize;
                    let va = a[y * width + x];
                    let vb = b[other_y * width + other_x];
                    dot += va * vb;
                    aa += va * va;
                    bb += vb * vb;
                }
            }
            let score = dot / (aa * bb).sqrt().max(1.0e-9);
            if score > best.2 {
                best = (dx as i32, dy as i32, score);
            }
        }
    }
    (best.0 as f64, best.1 as f64, best.2)
}

#[derive(Clone, Copy)]
struct OrientedFeature {
    x: f64,
    y: f64,
    descriptor: [u64; 4],
}

#[derive(Clone, Copy)]
struct FeaturePair {
    source: [f64; 2],
    destination: [f64; 2],
}

struct AffineEstimate {
    offset_x: f64,
    offset_y: f64,
    rotation_degrees: f64,
    scale_percent: f64,
    matches: usize,
    inliers: usize,
}

/// FAST corners + intensity-centroid oriented BRIEF descriptors, followed by
/// deterministic affine RANSAC. It is intentionally a diagnostic only; no
/// estimated transform is ever applied to calibration pixels.
fn estimate_orb_like_affine_ransac(
    source: &[f64],
    destination: &[f64],
    width: usize,
    height: usize,
) -> Option<AffineEstimate> {
    let features_a = detect_oriented_features(source, width, height);
    let features_b = detect_oriented_features(destination, width, height);
    if features_a.len() < 6 || features_b.len() < 6 {
        return None;
    }
    let mut matches = Vec::new();
    for source_feature in &features_a {
        let mut best = (usize::MAX, 0usize);
        let mut second = (usize::MAX, 0usize);
        for (index, destination_feature) in features_b.iter().enumerate() {
            let distance =
                descriptor_distance(&source_feature.descriptor, &destination_feature.descriptor);
            if distance < best.0 {
                second = best;
                best = (distance, index);
            } else if distance < second.0 {
                second = (distance, index);
            }
        }
        if best.0 <= 80 && best.0.saturating_mul(100) < second.0.saturating_mul(88) {
            let target = features_b[best.1];
            matches.push(FeaturePair {
                source: [source_feature.x, source_feature.y],
                destination: [target.x, target.y],
            });
        }
    }
    if matches.len() < 6 {
        return None;
    }

    let mut state = 0x9e37_79b9_u32 ^ matches.len() as u32;
    let mut best_transform = None;
    let mut best_inliers = Vec::new();
    for _ in 0..160 {
        let indices = [
            next_random(&mut state) as usize % matches.len(),
            next_random(&mut state) as usize % matches.len(),
            next_random(&mut state) as usize % matches.len(),
        ];
        if indices[0] == indices[1] || indices[0] == indices[2] || indices[1] == indices[2] {
            continue;
        }
        let sample = [
            matches[indices[0]],
            matches[indices[1]],
            matches[indices[2]],
        ];
        let Some(transform) = fit_affine_three(&sample) else {
            continue;
        };
        let inliers: Vec<usize> = matches
            .iter()
            .enumerate()
            .filter_map(|(index, pair)| {
                (reprojection_error(transform, pair) <= 2.5).then_some(index)
            })
            .collect();
        if inliers.len() > best_inliers.len() {
            best_inliers = inliers;
            best_transform = Some(transform);
        }
    }
    if best_inliers.len() < 6 {
        return None;
    }
    let transform = fit_affine_least_squares(&matches, &best_inliers).or(best_transform)?;
    let center_x = width as f64 / 2.0;
    let center_y = height as f64 / 2.0;
    let transformed_x = transform[0] * center_x + transform[1] * center_y + transform[2];
    let transformed_y = transform[3] * center_x + transform[4] * center_y + transform[5];
    let rotation = (transform[3] - transform[1]).atan2(transform[0] + transform[4]);
    let determinant = transform[0] * transform[4] - transform[1] * transform[3];
    Some(AffineEstimate {
        offset_x: transformed_x - center_x,
        offset_y: transformed_y - center_y,
        rotation_degrees: rotation.to_degrees(),
        scale_percent: determinant.abs().sqrt() * 100.0,
        matches: matches.len(),
        inliers: best_inliers.len(),
    })
}

fn detect_oriented_features(image: &[f64], width: usize, height: usize) -> Vec<OrientedFeature> {
    const RING: [(isize, isize); 16] = [
        (0, -3),
        (1, -3),
        (2, -2),
        (3, -1),
        (3, 0),
        (3, 1),
        (2, 2),
        (1, 3),
        (0, 3),
        (-1, 3),
        (-2, 2),
        (-3, 1),
        (-3, 0),
        (-3, -1),
        (-2, -2),
        (-1, -3),
    ];
    if width < 24 || height < 24 {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    for y in 12..height - 12 {
        for x in 12..width - 12 {
            let center = image[y * width + x];
            let ring: [f64; 16] = std::array::from_fn(|index| {
                let (dx, dy) = RING[index];
                image[(y as isize + dy) as usize * width + (x as isize + dx) as usize]
            });
            let threshold = 0.08;
            let mut corner = false;
            for polarity in [-1.0, 1.0] {
                let mut run = 0;
                for index in 0..32 {
                    if polarity * (ring[index % 16] - center) > threshold {
                        run += 1;
                        if run >= 9 {
                            corner = true;
                            break;
                        }
                    } else {
                        run = 0;
                    }
                }
                if corner {
                    break;
                }
            }
            if corner {
                let score = ring.iter().map(|value| (value - center).abs()).sum::<f64>();
                candidates.push((score, x, y));
            }
        }
    }
    candidates.sort_by(|left, right| right.0.total_cmp(&left.0));
    let mut selected: Vec<(usize, usize)> = Vec::new();
    let mut output = Vec::new();
    for (_, x, y) in candidates {
        if selected
            .iter()
            .any(|(sx, sy)| sx.abs_diff(x) < 5 && sy.abs_diff(y) < 5)
        {
            continue;
        }
        selected.push((x, y));
        let mut moment_x = 0.0;
        let mut moment_y = 0.0;
        for dy in -6_isize..=6 {
            for dx in -6_isize..=6 {
                let value = image[(y as isize + dy) as usize * width + (x as isize + dx) as usize];
                moment_x += dx as f64 * value;
                moment_y += dy as f64 * value;
            }
        }
        let angle = moment_y.atan2(moment_x);
        output.push(OrientedFeature {
            x: x as f64,
            y: y as f64,
            descriptor: oriented_brief(image, width, x, y, angle),
        });
        if output.len() >= 400 {
            break;
        }
    }
    output
}

fn oriented_brief(image: &[f64], width: usize, x: usize, y: usize, angle: f64) -> [u64; 4] {
    let (sin, cos) = angle.sin_cos();
    let mut descriptor = [0_u64; 4];
    for bit in 0..256 {
        let (ax, ay, bx, by) = brief_pair(bit as u32);
        let rotate = |px: i32, py: i32| {
            (
                (f64::from(px) * cos - f64::from(py) * sin).round() as isize,
                (f64::from(px) * sin + f64::from(py) * cos).round() as isize,
            )
        };
        let (ax, ay) = rotate(ax, ay);
        let (bx, by) = rotate(bx, by);
        let first = image[(y as isize + ay) as usize * width + (x as isize + ax) as usize];
        let second = image[(y as isize + by) as usize * width + (x as isize + bx) as usize];
        if first < second {
            descriptor[bit / 64] |= 1_u64 << (bit % 64);
        }
    }
    descriptor
}

fn brief_pair(bit: u32) -> (i32, i32, i32, i32) {
    let mut state = bit.wrapping_mul(0x045d_9f3b).wrapping_add(0x2710_0001);
    let mut coordinate = || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        (state % 15) as i32 - 7
    };
    let ax = coordinate();
    let ay = coordinate();
    let bx = coordinate();
    let by = coordinate();
    (ax, ay, bx, by)
}

fn descriptor_distance(a: &[u64; 4], b: &[u64; 4]) -> usize {
    a.iter()
        .zip(b)
        .map(|(left, right)| (left ^ right).count_ones() as usize)
        .sum()
}

fn next_random(state: &mut u32) -> u32 {
    *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    *state
}

fn fit_affine_three(pairs: &[FeaturePair; 3]) -> Option<[f64; 6]> {
    let matrix = pairs.map(|pair| [pair.source[0], pair.source[1], 1.0]);
    let destination_x = pairs.map(|pair| pair.destination[0]);
    let destination_y = pairs.map(|pair| pair.destination[1]);
    let x = solve_3x3(matrix, destination_x)?;
    let y = solve_3x3(matrix, destination_y)?;
    Some([x[0], x[1], x[2], y[0], y[1], y[2]])
}

fn fit_affine_least_squares(pairs: &[FeaturePair], indices: &[usize]) -> Option<[f64; 6]> {
    let mut matrix = [[0.0; 3]; 3];
    let mut destination_x = [0.0; 3];
    let mut destination_y = [0.0; 3];
    for index in indices {
        let pair = pairs[*index];
        let vector = [pair.source[0], pair.source[1], 1.0];
        for row in 0..3 {
            for column in 0..3 {
                matrix[row][column] += vector[row] * vector[column];
            }
            destination_x[row] += vector[row] * pair.destination[0];
            destination_y[row] += vector[row] * pair.destination[1];
        }
    }
    let x = solve_3x3(matrix, destination_x)?;
    let y = solve_3x3(matrix, destination_y)?;
    Some([x[0], x[1], x[2], y[0], y[1], y[2]])
}

fn solve_3x3(matrix: [[f64; 3]; 3], rhs: [f64; 3]) -> Option<[f64; 3]> {
    let determinant = matrix[0][0] * (matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1])
        - matrix[0][1] * (matrix[1][0] * matrix[2][2] - matrix[1][2] * matrix[2][0])
        + matrix[0][2] * (matrix[1][0] * matrix[2][1] - matrix[1][1] * matrix[2][0]);
    if determinant.abs() < 1.0e-9 {
        return None;
    }
    let det_replace = |column: usize| {
        let mut copy = matrix;
        for row in 0..3 {
            copy[row][column] = rhs[row];
        }
        copy[0][0] * (copy[1][1] * copy[2][2] - copy[1][2] * copy[2][1])
            - copy[0][1] * (copy[1][0] * copy[2][2] - copy[1][2] * copy[2][0])
            + copy[0][2] * (copy[1][0] * copy[2][1] - copy[1][1] * copy[2][0])
    };
    Some(std::array::from_fn(|column| {
        det_replace(column) / determinant
    }))
}

fn reprojection_error(transform: [f64; 6], pair: &FeaturePair) -> f64 {
    let x = transform[0] * pair.source[0] + transform[1] * pair.source[1] + transform[2];
    let y = transform[3] * pair.source[0] + transform[4] * pair.source[1] + transform[5];
    ((x - pair.destination[0]).powi(2) + (y - pair.destination[1]).powi(2)).sqrt()
}

fn luminance(rgb: [u16; 3]) -> f64 {
    let rgb = rgb.map(|value| f64::from(value) / 65535.0);
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}

fn delta_e_76(a: [f64; 3], b: [f64; 3]) -> f64 {
    let a = rgb_to_lab(a);
    let b = rgb_to_lab(b);
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn rgb_to_lab(rgb: [f64; 3]) -> [f64; 3] {
    let linear = rgb.map(|value| {
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    });
    let x = (0.4124564 * linear[0] + 0.3575761 * linear[1] + 0.1804375 * linear[2]) / 0.95047;
    let y = 0.2126729 * linear[0] + 0.7151522 * linear[1] + 0.0721750 * linear[2];
    let z = (0.0193339 * linear[0] + 0.1191920 * linear[1] + 0.9503041 * linear[2]) / 1.08883;
    let f = |value: f64| {
        if value > 0.008856451679 {
            value.cbrt()
        } else {
            7.787037037 * value + 16.0 / 116.0
        }
    };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_color_change_does_not_look_like_geometric_movement() {
        let (width, height) = (96_u32, 80_u32);
        let mut original = Vec::with_capacity((width * height) as usize);
        let mut graded = Vec::with_capacity((width * height) as usize);
        for y in 0..height {
            for x in 0..width {
                let detail = ((x * 11 + y * 7) % 19) as i32 - 9;
                let base = if (20..72).contains(&x) && (14..63).contains(&y) {
                    160_i32
                } else {
                    55_i32
                };
                let value = (base + detail).clamp(0, 255) as u16 * 257;
                original.push([value; 3]);
                let encoded = f64::from(value) / 65535.0;
                let transformed = [
                    (encoded * 0.80 + 0.08).clamp(0.0, 1.0),
                    (encoded * 0.70 + 0.12).clamp(0.0, 1.0),
                    (encoded * 0.60 + 0.16).clamp(0.0, 1.0),
                ];
                graded.push(transformed.map(|component| (component * 65535.0).round() as u16));
            }
        }
        let grid = CubeGrid {
            size: 2,
            values: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
                [1.0, 0.0, 1.0],
                [0.0, 1.0, 1.0],
                [1.0, 1.0, 1.0],
            ],
        };
        let report = diagnose(
            &grid,
            &original,
            &graded,
            width,
            height,
            ConsistencyMode::Comprehensive,
        );
        assert!(report.edge_ssim.unwrap() > 0.90);
        let (offset_x, offset_y) = report.estimated_offset_pixels.unwrap();
        assert!(offset_x.abs() < 1.5, "x offset was {offset_x}");
        assert!(offset_y.abs() < 1.5, "y offset was {offset_y}");
        assert!(report.heldout_delta_e76.unwrap() > 0.0);

        let mut shifted = vec![[32_768_u16; 3]; original.len()];
        for y in 0..height as usize {
            for x in 4..width as usize {
                shifted[y * width as usize + x] = original[y * width as usize + x - 4];
            }
        }
        let shifted_report = diagnose(
            &grid,
            &original,
            &shifted,
            width,
            height,
            ConsistencyMode::GeometryStructure,
        );
        let (offset_x, _) = shifted_report.estimated_offset_pixels.unwrap();
        assert!(offset_x.abs() > 2.0, "x offset was {offset_x}");
    }
}
