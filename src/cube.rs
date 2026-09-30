use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::error::{LutError, Result, io_error, write_error};
use crate::raster::{
    GrainStatistics, NodeConfidence, PhotoMasterRaster, PhotoMasterSession, RasterQuality,
    load_raster_lut,
};

mod consistency;
mod reference_fit;

const EPSILON: f64 = 1.0e-9;
pub const OUTPUT_GRID_PRESETS: [usize; 4] = [17, 25, 33, 64];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmoothingMode {
    Auto,
    Off,
    Light,
    Medium,
}

impl SmoothingMode {
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Auto => "自动",
            Self::Off => "关闭",
            Self::Light => "轻度拟合",
            Self::Medium => "中度拟合",
        }
    }

    const fn confidence_percentile(self) -> f64 {
        match self {
            Self::Auto => 1.0,
            Self::Off => 1.0,
            Self::Light => 0.75,
            Self::Medium => 0.50,
        }
    }

    const fn jump_sigma(self) -> f64 {
        match self {
            Self::Auto => f64::INFINITY,
            Self::Off => f64::INFINITY,
            Self::Light => 3.5,
            Self::Medium => 2.25,
        }
    }

    const fn fit_strength(self) -> f64 {
        match self {
            Self::Auto => 0.0,
            Self::Off => 0.0,
            Self::Light => 0.35,
            Self::Medium => 0.60,
        }
    }

    const fn maximum_offset(self) -> f64 {
        match self {
            Self::Auto => 0.0,
            Self::Off => 0.0,
            Self::Light => 0.003,
            Self::Medium => 0.008,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SmoothingReport {
    pub mode: SmoothingMode,
    pub corrected_node_count: usize,
    pub maximum_offset: f64,
}

#[derive(Debug, Clone)]
pub struct ConversionReport {
    pub output_path: PathBuf,
    pub source_grid_size: usize,
    pub output_grid_size: usize,
    pub entry_count: usize,
    pub clipped_points: usize,
    pub grain_statistics: Option<GrainStatistics>,
    pub smoothing: SmoothingReport,
    pub reference_match: Option<ReferenceMatchReport>,
    pub consistency: Option<ConsistencyReport>,
    pub warnings: Vec<String>,
    pub validation: ValidationReport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceMatch {
    pub original_photo: PathBuf,
    pub graded_photo: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReferenceMatchReport {
    pub sampled_pixels: usize,
    pub validation_pixels: usize,
    pub before_mae_8bit: f64,
    pub after_mae_8bit: f64,
    pub dark_before_mae_8bit: f64,
    pub dark_after_mae_8bit: f64,
    pub corrected_nodes: usize,
    pub maximum_offset: f64,
    pub clipped_nodes: usize,
    pub assumed_srgb: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConsistencyMode {
    #[default]
    Comprehensive,
    ColorMapping,
    GeometryStructure,
}

impl ConsistencyMode {
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Comprehensive => "综合诊断",
            Self::ColorMapping => "颜色映射",
            Self::GeometryStructure => "几何与结构",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConsistencyReport {
    pub mode: ConsistencyMode,
    pub heldout_mae_8bit: Option<f64>,
    pub heldout_delta_e76: Option<f64>,
    pub edge_ssim: Option<f64>,
    pub estimated_offset_pixels: Option<(f64, f64)>,
    pub feature_matches: Option<usize>,
    pub ransac_inliers: Option<usize>,
    pub estimated_rotation_degrees: Option<f64>,
    pub estimated_scale_percent: Option<f64>,
    pub warnings: Vec<String>,
}

impl ConsistencyReport {
    /// Returns whether the combined color and geometry diagnostics found a
    /// mismatch or lacked enough confidence to rule one out.
    pub fn needs_confirmation(&self) -> bool {
        !self.warnings.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct ValidationReport {
    pub path: PathBuf,
    pub title: String,
    pub grid_size: usize,
    pub entry_count: usize,
    pub min_value: f64,
    pub max_value: f64,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct CubeGrid {
    size: usize,
    values: Vec<[f64; 3]>,
}

#[derive(Debug, Clone)]
pub struct PreparedLut {
    source_grid_size: usize,
    grid: CubeGrid,
    clipped_points: usize,
    warnings: Vec<String>,
    grain_statistics: Option<GrainStatistics>,
    smoothing: SmoothingReport,
    reference_match: Option<ReferenceMatchReport>,
    consistency: Option<ConsistencyReport>,
}

impl PreparedLut {
    pub fn source_grid_size(&self) -> usize {
        self.source_grid_size
    }

    pub fn target_grid_size(&self) -> usize {
        self.grid.size
    }

    pub fn entry_count(&self) -> usize {
        self.grid.values.len()
    }

    pub fn clipped_points(&self) -> usize {
        self.clipped_points
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn grain_statistics(&self) -> Option<&GrainStatistics> {
        self.grain_statistics.as_ref()
    }

    pub fn smoothing_report(&self) -> SmoothingReport {
        self.smoothing
    }

    pub fn reference_match_report(&self) -> Option<ReferenceMatchReport> {
        self.reference_match
    }

    pub fn consistency_report(&self) -> Option<&ConsistencyReport> {
        self.consistency.as_ref()
    }

    pub fn sample_tetrahedral(&self, rgb: [f32; 3]) -> [f32; 3] {
        let sampled = sample_tetrahedral(
            &self.grid,
            f64::from(rgb[0]),
            f64::from(rgb[1]),
            f64::from(rgb[2]),
        );
        [sampled[0] as f32, sampled[1] as f32, sampled[2] as f32]
    }
}

pub fn prepare_lut(input: impl AsRef<Path>, target_grid: usize) -> Result<PreparedLut> {
    prepare_lut_with_smoothing(input, target_grid, SmoothingMode::Off)
}

pub fn prepare_lut_with_smoothing(
    input: impl AsRef<Path>,
    target_grid: usize,
    smoothing_mode: SmoothingMode,
) -> Result<PreparedLut> {
    prepare_lut_internal(
        input.as_ref(),
        target_grid,
        smoothing_mode,
        None,
        ConsistencyMode::Comprehensive,
    )
}

pub fn prepare_lut_with_photo_master(
    input: impl AsRef<Path>,
    target_grid: usize,
    smoothing_mode: SmoothingMode,
    session: Option<PhotoMasterSession<'_>>,
    consistency_mode: ConsistencyMode,
) -> Result<PreparedLut> {
    prepare_lut_internal(
        input.as_ref(),
        target_grid,
        smoothing_mode,
        session,
        consistency_mode,
    )
}

fn prepare_lut_internal(
    input: &Path,
    target_grid: usize,
    smoothing_mode: SmoothingMode,
    photo_session: Option<PhotoMasterSession<'_>>,
    consistency_mode: ConsistencyMode,
) -> Result<PreparedLut> {
    if !OUTPUT_GRID_PRESETS.contains(&target_grid) {
        return Err(LutError::InvalidCube(format!(
            "输出 Grid 仅支持 17、25、33、64，检测到 {target_grid}"
        )));
    }

    let extension = input
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let (mut source, grain_statistics, node_confidence, raster_quality, layout_note, photo_master) =
        if extension == "cube" {
            (parse_cube(input)?, None, None, None, None, None)
        } else {
            let raster = load_raster_lut(input)?;
            let photo_master = raster.photo_master.clone();
            (
                CubeGrid {
                    size: raster.size,
                    values: raster.values,
                },
                raster.grain_statistics,
                raster.node_confidence,
                Some(raster.quality),
                raster.layout_note,
                photo_master,
            )
        };

    let mut warnings = Vec::new();
    if let Some(note) = layout_note {
        warnings.push(note);
    }
    if let Some(quality) = raster_quality {
        if quality.bit_depth == 8 {
            warnings.push(
                "检测到 RGB8 输入：已在每个 8-bit 量化区间内执行受限亚码值拟合；欠缺的原始位深无法完全恢复。"
                    .to_owned(),
            );
        }
        if quality.lossy_jpeg {
            warnings.push(
                "检测到 JPEG 输入：已用有界二阶残差拟合减弱量化与压缩伪影；有损压缩丢失的信息无法完全恢复。"
                    .to_owned(),
            );
        }
        if quality.assumed_srgb {
            warnings.push("JPEG 未嵌入 ICC，已按 sRGB 编码值解释。".to_owned());
        }
        if quality.bit_depth == 8 || quality.lossy_jpeg {
            let repair = refine_low_precision_grid(&mut source, quality);
            warnings.push(format!(
                "低精度补偿：修正 {} 个节点，最大偏移 {:.6}。",
                repair.corrected_node_count, repair.maximum_offset
            ));
        }
    }
    if let Some(statistics) = &grain_statistics
        && statistics.samples_per_node > 1
    {
        warnings.push(format!(
            "已识别{}中性图：每节点 {} 个样本，并使用自适应稳健聚合。",
            statistics.sampling.display_name(),
            statistics.samples_per_node
        ));
        warnings.push(
            "随机、近似零均值的颗粒可通过聚合减弱；固定纹理、暗角、光晕、局部对比度、锐化及其他空间效果无法正确转换为 3D LUT。"
                .to_owned(),
        );
        warnings.push("黑白端可能因颗粒裁切产生平均值偏移。".to_owned());
        if statistics.huber_node_count > 0 {
            warnings.push(format!(
                "检测到异常颗粒：{} 个节点已使用 Huber 稳健平均。",
                statistics.huber_node_count
            ));
        }
        if statistics.removed_clipped_samples > 0 {
            warnings.push(format!(
                "{} 个节点共剔除 {} 个阴影/高光裁切样本；未尝试恢复已裁切数据。",
                statistics.clipped_node_count, statistics.removed_clipped_samples
            ));
        }
    }
    let effective_smoothing_mode = resolve_smoothing_mode(
        smoothing_mode,
        grain_statistics
            .as_ref()
            .map(|statistics| statistics.sampling),
    );
    let smoothing = match (effective_smoothing_mode, node_confidence.as_deref()) {
        (SmoothingMode::Off, _) => SmoothingReport {
            mode: SmoothingMode::Off,
            corrected_node_count: 0,
            maximum_offset: 0.0,
        },
        (mode, Some(confidence)) => smooth_low_confidence_nodes(&mut source, confidence, mode),
        (mode, None) => {
            warnings.push(
                "平滑降噪仅适用于重复采样中性图；当前输入没有逐节点可信度，已跳过。".to_owned(),
            );
            SmoothingReport {
                mode,
                corrected_node_count: 0,
                maximum_offset: 0.0,
            }
        }
    };
    if smoothing.corrected_node_count > 0 {
        warnings.push(format!(
            "已应用{}：修正 {} 个低可信度节点，最大偏移 {:.6}。",
            smoothing.mode.display_name(),
            smoothing.corrected_node_count,
            smoothing.maximum_offset
        ));
    }

    if source.size < target_grid {
        warnings.push(format!(
            "源 LUT 只有 {} 点；已插值放大到 {target_grid} 点，但不能恢复原本不存在的颜色细节。",
            source.size,
        ));
    } else if source.size > target_grid {
        warnings.push(format!(
            "源 LUT 为 {} 点；已使用四面体插值降采样到 {target_grid} 点。",
            source.size,
        ));
    }
    if target_grid == 64 {
        warnings.push("64-grid 为通用格式，仅供桌面软件或存档，不能导入 LUMIX 相机。".to_owned());
    }

    let source_grid_size = source.size;
    let mut grid = if source.size == target_grid {
        source
    } else {
        CubeGrid {
            size: target_grid,
            values: resample_tetrahedral(&source, target_grid),
        }
    };
    let target_entry_count = target_grid.pow(3);
    let mut clipped_points = 0;
    for value in &mut grid.values {
        if value
            .iter()
            .any(|component| *component < 0.0 || *component > 1.0)
        {
            clipped_points += 1;
        }
        for component in value {
            // Preview and export share the exact six-decimal node values that
            // will be written to the CUBE, avoiding a subtle display/export split.
            *component = (component.clamp(0.0, 1.0) * 1_000_000.0).round() / 1_000_000.0;
        }
    }
    if clipped_points > 0 {
        warnings.push(format!(
            "为保证标准 CUBE 兼容，已裁切 {clipped_points}/{target_entry_count} 个超出 0–1 的节点（{:.2}%）。",
            clipped_points as f64 * 100.0 / target_entry_count as f64
        ));
    }

    let mut prepared = PreparedLut {
        source_grid_size,
        grid,
        clipped_points,
        warnings,
        grain_statistics,
        smoothing,
        reference_match: None,
        consistency: None,
    };
    if let Some(master) = photo_master {
        let session = photo_session.ok_or(LutError::MissingPhotoMasterBaseline)?;
        let baseline = session.baseline;
        let rect = session.calibration_rect;
        let (width, height) = baseline.dimensions();
        let graded = crop_photo_master(&master, rect, (width, height))?;
        let report = reference_fit::fit_reference_samples(
            &mut prepared.grid,
            width,
            height,
            baseline.pixels(),
            &graded,
            false,
        )?;
        prepared.clipped_points += report.clipped_nodes;
        prepared.warnings.push(format!(
            "照片母版校准：留出像素误差 {:.2}→{:.2}（8-bit 码值），暗部 {:.2}→{:.2}；修正 {} 个 LUT 节点。",
            report.before_mae_8bit,
            report.after_mae_8bit,
            report.dark_before_mae_8bit,
            report.dark_after_mae_8bit,
            report.corrected_nodes,
        ));
        let consistency = consistency::diagnose(
            &prepared.grid,
            baseline.pixels(),
            &graded,
            width,
            height,
            consistency_mode,
        );
        prepared
            .warnings
            .extend(consistency.warnings.iter().cloned());
        prepared.reference_match = Some(report);
        prepared.consistency = Some(consistency);
    }
    Ok(prepared)
}

fn crop_photo_master(
    master: &PhotoMasterRaster,
    (x, y, width, height): (u32, u32, u32, u32),
    expected: (u32, u32),
) -> Result<Vec<[u16; 3]>> {
    let size = master.canvas_size;
    if (width, height) != expected
        || x.checked_add(width).is_none_or(|right| right > size)
        || y.checked_add(height).is_none_or(|bottom| bottom > size)
        || master.photo_pixels.len() != (size as usize).saturating_mul(size as usize)
    {
        return Err(LutError::InvalidRaster(
            "导回母版的照片有效区与本次生成的参考图不匹配；请勿裁切、缩放或调整画布尺寸".to_owned(),
        ));
    }
    let mut cropped = Vec::with_capacity((width * height) as usize);
    for row in y..y + height {
        let start = (row * size + x) as usize;
        cropped.extend_from_slice(&master.photo_pixels[start..start + width as usize]);
    }
    Ok(cropped)
}

pub fn prepare_lut_with_reference(
    input: impl AsRef<Path>,
    target_grid: usize,
    smoothing_mode: SmoothingMode,
    reference: Option<&ReferenceMatch>,
) -> Result<PreparedLut> {
    let mut prepared = prepare_lut_with_smoothing(input, target_grid, smoothing_mode)?;
    if let Some(reference) = reference {
        let report = reference_fit::fit_reference(&mut prepared.grid, reference)?;
        prepared.clipped_points += report.clipped_nodes;
        prepared.warnings.push(format!(
            "参考照片匹配：留出像素平均误差 {:.2}→{:.2}（8-bit 码值），暗部 {:.2}→{:.2}；修正 {} 个节点，最大偏移 {:.4}。",
            report.before_mae_8bit,
            report.after_mae_8bit,
            report.dark_before_mae_8bit,
            report.dark_after_mae_8bit,
            report.corrected_nodes,
            report.maximum_offset,
        ));
        prepared.warnings.push(
            "参考匹配是该照片的最佳全局近似；Camera Raw 的局部明暗效果仍会随画面内容变化。"
                .to_owned(),
        );
        if report.assumed_srgb {
            prepared
                .warnings
                .push("参考 JPEG 缺少嵌入式 ICC，已按 sRGB 解释。".to_owned());
        }
        prepared.reference_match = Some(report);
    }
    Ok(prepared)
}

fn smooth_low_confidence_nodes(
    grid: &mut CubeGrid,
    confidence: &[NodeConfidence],
    mode: SmoothingMode,
) -> SmoothingReport {
    debug_assert_ne!(mode, SmoothingMode::Off);
    debug_assert_eq!(grid.values.len(), confidence.len());
    if grid.size < 3 || grid.values.len() != confidence.len() {
        return SmoothingReport {
            mode,
            corrected_node_count: 0,
            maximum_offset: 0.0,
        };
    }

    let mut noise_values: Vec<f64> = confidence
        .iter()
        .map(|node| node.noise_standard_deviation)
        .collect();
    noise_values.sort_by(f64::total_cmp);
    let percentile_index = ((noise_values.len() as f64 * mode.confidence_percentile()).ceil()
        as usize)
        .saturating_sub(1)
        .min(noise_values.len() - 1);
    let low_confidence_threshold = noise_values[percentile_index];
    let size = grid.size;
    let mut corrections = vec![[0.0; 3]; grid.values.len()];
    let mut corrected_node_count = 0;
    let mut maximum_offset = 0.0_f64;

    for blue in 1..size - 1 {
        for green in 1..size - 1 {
            for red in 1..size - 1 {
                let index = cube_index(size, red, green, blue);
                let node = confidence[index];
                let low_confidence = node.used_huber
                    || node.removed_clipped_samples > 0
                    || node.quantization_limited
                    || node.lossy_compression
                    || node.noise_standard_deviation > low_confidence_threshold + EPSILON;
                if !low_confidence {
                    continue;
                }

                let neighbors = [
                    (
                        cube_index(size, red - 1, green, blue),
                        cube_index(size, red + 1, green, blue),
                    ),
                    (
                        cube_index(size, red, green - 1, blue),
                        cube_index(size, red, green + 1, blue),
                    ),
                    (
                        cube_index(size, red, green, blue - 1),
                        cube_index(size, red, green, blue + 1),
                    ),
                ];
                let mut prediction = [0.0; 3];
                let mut weight_sum = 0.0;
                let mut weighted_neighbor_noise = 0.0;
                for (left, right) in neighbors {
                    let pair_noise = (confidence[left].noise_standard_deviation
                        + confidence[right].noise_standard_deviation)
                        * 0.5;
                    let pair_weight = 1.0 / pair_noise.max(1.0 / f64::from(u16::MAX));
                    for (channel, predicted) in prediction.iter_mut().enumerate() {
                        *predicted += (grid.values[left][channel] + grid.values[right][channel])
                            * 0.5
                            * pair_weight;
                    }
                    weighted_neighbor_noise += pair_noise * pair_weight;
                    weight_sum += pair_weight;
                }
                for channel in &mut prediction {
                    *channel /= weight_sum;
                }
                let neighbor_noise = weighted_neighbor_noise / weight_sum;
                let delta = std::array::from_fn(|channel| {
                    prediction[channel] - grid.values[index][channel]
                });
                let jump = vector_length(delta);
                let noise_range =
                    (node.noise_standard_deviation + neighbor_noise).max(1.0 / f64::from(u16::MAX));
                if jump <= mode.jump_sigma() * noise_range {
                    continue;
                }

                let mut confidence_weight = node.noise_standard_deviation
                    / (node.noise_standard_deviation + neighbor_noise + 1.0 / f64::from(u16::MAX));
                if node.used_huber || node.removed_clipped_samples > 0 {
                    confidence_weight = confidence_weight.max(0.5);
                }
                let mut correction =
                    delta.map(|value| value * mode.fit_strength() * confidence_weight);
                let correction_length = vector_length(correction);
                let maximum = mode.maximum_offset();
                if correction_length > maximum {
                    let scale = maximum / correction_length;
                    correction = correction.map(|value| value * scale);
                }
                let applied_length = vector_length(correction);
                if applied_length > 0.0 {
                    corrections[index] = correction;
                    corrected_node_count += 1;
                    maximum_offset = maximum_offset.max(applied_length);
                }
            }
        }
    }
    for (value, correction) in grid.values.iter_mut().zip(corrections) {
        for channel in 0..3 {
            value[channel] += correction[channel];
        }
    }
    SmoothingReport {
        mode,
        corrected_node_count,
        maximum_offset,
    }
}

fn resolve_smoothing_mode(
    requested: SmoothingMode,
    sampling: Option<crate::raster::NeutralSampling>,
) -> SmoothingMode {
    match (requested, sampling) {
        (
            SmoothingMode::Auto,
            Some(
                crate::raster::NeutralSampling::Average16
                | crate::raster::NeutralSampling::Average64,
            ),
        ) => SmoothingMode::Light,
        (SmoothingMode::Auto, _) => SmoothingMode::Off,
        (mode, _) => mode,
    }
}

#[derive(Debug, Clone, Copy)]
struct PrecisionRepairReport {
    corrected_node_count: usize,
    maximum_offset: f64,
}

/// Estimates sub-code values from symmetric second differences. Affine LUTs
/// have a zero second difference, so identity, exposure and color matrices are
/// fixed points of the predictor. The estimate is accepted only inside the
/// uncertainty interval of the decoded 8-bit/JPEG sample and is never used to
/// reconstruct clipped black or white values.
fn refine_low_precision_grid(grid: &mut CubeGrid, quality: RasterQuality) -> PrecisionRepairReport {
    if grid.size < 3 || (quality.bit_depth != 8 && !quality.lossy_jpeg) {
        return PrecisionRepairReport {
            corrected_node_count: 0,
            maximum_offset: 0.0,
        };
    }

    let code = 1.0 / 255.0;
    let uncertainty = if quality.lossy_jpeg {
        1.5 * code
    } else {
        0.5 * code
    };
    let strength = if quality.lossy_jpeg { 0.65 } else { 1.0 };
    let maximum = if quality.lossy_jpeg { code } else { 0.5 * code };
    let size = grid.size;
    let mut corrections = vec![[0.0; 3]; grid.values.len()];
    let mut corrected_node_count = 0;
    let mut maximum_offset = 0.0_f64;

    for blue in 1..size - 1 {
        for green in 1..size - 1 {
            for red in 1..size - 1 {
                let index = cube_index(size, red, green, blue);
                let neighbors = [
                    (
                        cube_index(size, red - 1, green, blue),
                        cube_index(size, red + 1, green, blue),
                    ),
                    (
                        cube_index(size, red, green - 1, blue),
                        cube_index(size, red, green + 1, blue),
                    ),
                    (
                        cube_index(size, red, green, blue - 1),
                        cube_index(size, red, green, blue + 1),
                    ),
                ];
                let mut correction = [0.0; 3];
                for (channel, component) in correction.iter_mut().enumerate() {
                    let observed = grid.values[index][channel];
                    if observed <= EPSILON || observed >= 1.0 - EPSILON {
                        continue;
                    }
                    let prediction = neighbors
                        .iter()
                        .map(|(left, right)| {
                            (grid.values[*left][channel] + grid.values[*right][channel]) * 0.5
                        })
                        .sum::<f64>()
                        / neighbors.len() as f64;
                    let residual = prediction - observed;
                    if residual.abs() <= uncertainty {
                        *component = residual * strength;
                    }
                }
                let length = vector_length(correction);
                if length > maximum {
                    let scale = maximum / length;
                    correction = correction.map(|component| component * scale);
                }
                let applied = vector_length(correction);
                if applied > EPSILON {
                    corrections[index] = correction;
                    corrected_node_count += 1;
                    maximum_offset = maximum_offset.max(applied);
                }
            }
        }
    }

    for (value, correction) in grid.values.iter_mut().zip(corrections) {
        for channel in 0..3 {
            value[channel] += correction[channel];
        }
    }
    PrecisionRepairReport {
        corrected_node_count,
        maximum_offset,
    }
}

fn cube_index(size: usize, red: usize, green: usize, blue: usize) -> usize {
    blue * size * size + green * size + red
}

fn vector_length(value: [f64; 3]) -> f64 {
    value
        .iter()
        .map(|component| component * component)
        .sum::<f64>()
        .sqrt()
}

pub fn convert_cube(
    input: impl AsRef<Path>,
    title: &str,
    target_grid: usize,
    destination: impl AsRef<Path>,
) -> Result<ConversionReport> {
    convert_cube_with_smoothing(input, title, target_grid, destination, SmoothingMode::Off)
}

pub fn convert_cube_with_smoothing(
    input: impl AsRef<Path>,
    title: &str,
    target_grid: usize,
    destination: impl AsRef<Path>,
    smoothing_mode: SmoothingMode,
) -> Result<ConversionReport> {
    convert_cube_with_reference(input, title, target_grid, destination, smoothing_mode, None)
}

pub fn convert_cube_with_reference(
    input: impl AsRef<Path>,
    title: &str,
    target_grid: usize,
    destination: impl AsRef<Path>,
    smoothing_mode: SmoothingMode,
    reference: Option<&ReferenceMatch>,
) -> Result<ConversionReport> {
    let input = input.as_ref();
    let destination = destination.as_ref();
    let title = sanitize_title(title)?;
    ensure_cube_extension(destination)?;
    let prepared = prepare_lut_with_reference(input, target_grid, smoothing_mode, reference)?;
    let mut warnings = prepared.warnings.clone();
    if target_grid <= 33
        && let Some(warning) = filename_warning(destination)
    {
        warnings.push(warning);
    }

    write_cube(destination, &title, target_grid, &prepared.grid.values)?;
    let mut validation = validate_cube(destination)?;
    validation.warnings.extend(warnings.clone());

    Ok(ConversionReport {
        output_path: destination.to_path_buf(),
        source_grid_size: prepared.source_grid_size,
        output_grid_size: target_grid,
        entry_count: prepared.grid.values.len(),
        clipped_points: prepared.clipped_points,
        grain_statistics: prepared.grain_statistics.clone(),
        smoothing: prepared.smoothing,
        reference_match: prepared.reference_match,
        consistency: prepared.consistency,
        warnings,
        validation,
    })
}

pub fn convert_cube_with_photo_master(
    input: impl AsRef<Path>,
    title: &str,
    target_grid: usize,
    destination: impl AsRef<Path>,
    smoothing_mode: SmoothingMode,
    session: Option<PhotoMasterSession<'_>>,
    consistency_mode: ConsistencyMode,
) -> Result<ConversionReport> {
    let input = input.as_ref();
    let destination = destination.as_ref();
    let title = sanitize_title(title)?;
    ensure_cube_extension(destination)?;
    let prepared = prepare_lut_with_photo_master(
        input,
        target_grid,
        smoothing_mode,
        session,
        consistency_mode,
    )?;
    export_prepared_cube(&prepared, &title, destination)
}

/// Writes the exact nodes already prepared for preview. Callers can validate
/// inputs before showing a save dialog and export without reprocessing them.
pub fn export_prepared_cube(
    prepared: &PreparedLut,
    title: &str,
    destination: impl AsRef<Path>,
) -> Result<ConversionReport> {
    let destination = destination.as_ref();
    let title = sanitize_title(title)?;
    ensure_cube_extension(destination)?;
    let target_grid = prepared.target_grid_size();
    let mut warnings = prepared.warnings.clone();
    if target_grid <= 33
        && let Some(warning) = filename_warning(destination)
    {
        warnings.push(warning);
    }
    write_cube(destination, &title, target_grid, &prepared.grid.values)?;
    let mut validation = validate_cube(destination)?;
    validation.warnings.extend(warnings.clone());
    Ok(ConversionReport {
        output_path: destination.to_path_buf(),
        source_grid_size: prepared.source_grid_size,
        output_grid_size: target_grid,
        entry_count: prepared.grid.values.len(),
        clipped_points: prepared.clipped_points,
        grain_statistics: prepared.grain_statistics.clone(),
        smoothing: prepared.smoothing,
        reference_match: prepared.reference_match,
        consistency: prepared.consistency.clone(),
        warnings,
        validation,
    })
}

pub fn validate_cube(path: impl AsRef<Path>) -> Result<ValidationReport> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path).map_err(|error| io_error(path, error))?;
    if text.contains('\r') {
        return Err(LutError::InvalidCube(
            "文件必须使用 UTF-8/LF 换行，检测到 CR 字符".to_owned(),
        ));
    }

    let mut title: Option<String> = None;
    let mut size = None;
    let mut domain_min = None;
    let mut domain_max = None;
    let mut values = Vec::new();

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(value) = line.strip_prefix("TITLE ") {
            if title.replace(parse_title(value)?).is_some() {
                return Err(LutError::InvalidCube("TITLE 重复".to_owned()));
            }
        } else if line.starts_with('#') {
            continue;
        } else if let Some(value) = line.strip_prefix("LUT_3D_SIZE ") {
            size = Some(parse_size(value)?);
        } else if let Some(value) = line.strip_prefix("DOMAIN_MIN ") {
            domain_min = Some(parse_triplet(value, "DOMAIN_MIN")?);
        } else if let Some(value) = line.strip_prefix("DOMAIN_MAX ") {
            domain_max = Some(parse_triplet(value, "DOMAIN_MAX")?);
        } else if line.starts_with("LUT_1D_SIZE") {
            return Err(LutError::InvalidCube("不能包含 LUT_1D_SIZE".to_owned()));
        } else {
            values.push(parse_triplet(line, "数据行")?);
        }
    }

    let title = title.ok_or_else(|| LutError::InvalidCube("缺少 TITLE".to_owned()))?;
    let grid_size = size.ok_or_else(|| LutError::InvalidCube("缺少 LUT_3D_SIZE".to_owned()))?;
    if !(2..=65).contains(&grid_size) {
        return Err(LutError::InvalidCube(format!(
            "仅支持校验 2–65 点 3D LUT，检测到 {grid_size}"
        )));
    }
    require_unit_domain(domain_min, domain_max)?;
    let expected_entry_count = grid_size.pow(3);
    if values.len() != expected_entry_count {
        return Err(LutError::InvalidCube(format!(
            "{grid_size} 点 LUT 应有 {expected_entry_count} 条数据，实际为 {} 条",
            values.len()
        )));
    }

    let mut min_value = f64::INFINITY;
    let mut max_value = f64::NEG_INFINITY;
    for value in &values {
        for component in value {
            if !component.is_finite() || *component < 0.0 || *component > 1.0 {
                return Err(LutError::InvalidCube(format!(
                    "输出数值必须为 0–1 的有限数，检测到 {component}"
                )));
            }
            min_value = min_value.min(*component);
            max_value = max_value.max(*component);
        }
    }

    let mut warnings = Vec::new();
    if grid_size <= 33
        && let Some(warning) = filename_warning(path)
    {
        warnings.push(warning);
    }
    if grid_size == 64 {
        warnings.push("64-grid 为通用格式，仅供桌面软件或存档，不能导入 LUMIX 相机。".to_owned());
    }

    Ok(ValidationReport {
        path: path.to_path_buf(),
        title,
        grid_size,
        entry_count: values.len(),
        min_value,
        max_value,
        warnings,
    })
}

fn parse_cube(path: &Path) -> Result<CubeGrid> {
    let file = File::open(path).map_err(|error| io_error(path, error))?;
    let reader = BufReader::new(file);
    let mut size = None;
    let mut domain_min = None;
    let mut domain_max = None;
    let mut input_range = None;
    let mut values = Vec::new();

    for (line_number, line) in reader.lines().enumerate() {
        let line = line.map_err(|error| io_error(path, error))?;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("TITLE ") {
            continue;
        }
        if line.starts_with("LUT_1D_SIZE") {
            return Err(LutError::InvalidCube(
                "第一版不支持 1D 或 1D+3D 组合 LUT".to_owned(),
            ));
        }
        if let Some(value) = line.strip_prefix("LUT_3D_SIZE ") {
            if size.replace(parse_size(value)?).is_some() {
                return Err(LutError::InvalidCube("LUT_3D_SIZE 重复".to_owned()));
            }
        } else if let Some(value) = line.strip_prefix("DOMAIN_MIN ") {
            domain_min = Some(parse_triplet(value, "DOMAIN_MIN")?);
        } else if let Some(value) = line.strip_prefix("DOMAIN_MAX ") {
            domain_max = Some(parse_triplet(value, "DOMAIN_MAX")?);
        } else if let Some(value) = line.strip_prefix("LUT_3D_INPUT_RANGE ") {
            let parts = parse_numbers(value)?;
            if parts.len() != 2 {
                return Err(LutError::InvalidCube(
                    "LUT_3D_INPUT_RANGE 必须有两个数值".to_owned(),
                ));
            }
            input_range = Some([parts[0], parts[1]]);
        } else {
            values.push(parse_triplet(line, &format!("第 {} 行", line_number + 1))?);
        }
    }

    let size = size.ok_or_else(|| LutError::InvalidCube("缺少 LUT_3D_SIZE".to_owned()))?;
    if !(2..=65).contains(&size) {
        return Err(LutError::InvalidCube(format!(
            "仅支持 2–65 点 3D LUT，检测到 {size}"
        )));
    }
    require_unit_domain(domain_min, domain_max)?;
    if let Some(range) = input_range
        && (!near(range[0], 0.0) || !near(range[1], 1.0))
    {
        return Err(LutError::InvalidCube(
            "仅接受 LUT_3D_INPUT_RANGE 0 1".to_owned(),
        ));
    }
    let expected = size * size * size;
    if values.len() != expected {
        return Err(LutError::InvalidCube(format!(
            "{size} 点 LUT 应有 {expected} 条数据，实际为 {} 条",
            values.len()
        )));
    }
    if values
        .iter()
        .flatten()
        .any(|component| !component.is_finite())
    {
        return Err(LutError::InvalidCube("包含 NaN 或 Infinity".to_owned()));
    }

    Ok(CubeGrid { size, values })
}

fn write_cube(path: &Path, title: &str, grid_size: usize, values: &[[f64; 3]]) -> Result<()> {
    let file = File::create(path).map_err(|error| write_error(path, error))?;
    let mut writer = BufWriter::new(file);
    writeln!(writer, "TITLE \"{title}\"").map_err(|error| write_error(path, error))?;
    writeln!(writer, "LUT_3D_SIZE {grid_size}").map_err(|error| write_error(path, error))?;
    writeln!(writer, "DOMAIN_MIN 0.000000 0.000000 0.000000")
        .map_err(|error| write_error(path, error))?;
    writeln!(writer, "DOMAIN_MAX 1.000000 1.000000 1.000000")
        .map_err(|error| write_error(path, error))?;
    for value in values {
        writeln!(writer, "{:.6} {:.6} {:.6}", value[0], value[1], value[2])
            .map_err(|error| write_error(path, error))?;
    }
    writer.flush().map_err(|error| write_error(path, error))
}

fn resample_tetrahedral(source: &CubeGrid, target_size: usize) -> Vec<[f64; 3]> {
    let mut output = Vec::with_capacity(target_size * target_size * target_size);
    let denominator = (target_size - 1) as f64;
    for blue in 0..target_size {
        for green in 0..target_size {
            for red in 0..target_size {
                output.push(sample_tetrahedral(
                    source,
                    red as f64 / denominator,
                    green as f64 / denominator,
                    blue as f64 / denominator,
                ));
            }
        }
    }
    output
}

fn sample_tetrahedral(grid: &CubeGrid, red: f64, green: f64, blue: f64) -> [f64; 3] {
    let max = (grid.size - 1) as f64;
    let x = red.clamp(0.0, 1.0) * max;
    let y = green.clamp(0.0, 1.0) * max;
    let z = blue.clamp(0.0, 1.0) * max;
    let r0 = (x.floor() as usize).min(grid.size - 1);
    let g0 = (y.floor() as usize).min(grid.size - 1);
    let b0 = (z.floor() as usize).min(grid.size - 1);
    let r1 = (r0 + 1).min(grid.size - 1);
    let g1 = (g0 + 1).min(grid.size - 1);
    let b1 = (b0 + 1).min(grid.size - 1);
    let dr = x - x.floor();
    let dg = y - y.floor();
    let db = z - z.floor();

    let c000 = value_at(grid, r0, g0, b0);
    let c100 = value_at(grid, r1, g0, b0);
    let c010 = value_at(grid, r0, g1, b0);
    let c110 = value_at(grid, r1, g1, b0);
    let c001 = value_at(grid, r0, g0, b1);
    let c101 = value_at(grid, r1, g0, b1);
    let c011 = value_at(grid, r0, g1, b1);
    let c111 = value_at(grid, r1, g1, b1);

    if dr >= dg {
        if dg >= db {
            combine(c000, [(dr, c100, c000), (dg, c110, c100), (db, c111, c110)])
        } else if dr >= db {
            combine(c000, [(dr, c100, c000), (db, c101, c100), (dg, c111, c101)])
        } else {
            combine(c000, [(db, c001, c000), (dr, c101, c001), (dg, c111, c101)])
        }
    } else if db >= dg {
        combine(c000, [(db, c001, c000), (dg, c011, c001), (dr, c111, c011)])
    } else if db >= dr {
        combine(c000, [(dg, c010, c000), (db, c011, c010), (dr, c111, c011)])
    } else {
        combine(c000, [(dg, c010, c000), (dr, c110, c010), (db, c111, c110)])
    }
}

fn combine(base: [f64; 3], terms: [(f64, [f64; 3], [f64; 3]); 3]) -> [f64; 3] {
    let mut result = base;
    for (weight, high, low) in terms {
        for channel in 0..3 {
            result[channel] += weight * (high[channel] - low[channel]);
        }
    }
    result
}

fn value_at(grid: &CubeGrid, red: usize, green: usize, blue: usize) -> [f64; 3] {
    grid.values[blue * grid.size * grid.size + green * grid.size + red]
}

fn sanitize_title(title: &str) -> Result<String> {
    let title = title
        .replace(['\n', '\r'], " ")
        .replace(['"', '\\'], "'")
        .trim()
        .to_owned();
    if title.is_empty() {
        Err(LutError::EmptyTitle)
    } else {
        Ok(title)
    }
}

fn parse_title(value: &str) -> Result<String> {
    let value = value.trim();
    if value.len() < 2 || !value.starts_with('"') || !value.ends_with('"') {
        return Err(LutError::InvalidCube("TITLE 必须使用双引号".to_owned()));
    }
    let title = value[1..value.len() - 1].trim().to_owned();
    if title.is_empty() {
        Err(LutError::InvalidCube("TITLE 不能为空".to_owned()))
    } else {
        Ok(title)
    }
}

fn parse_size(value: &str) -> Result<usize> {
    value
        .trim()
        .parse::<usize>()
        .map_err(|_| LutError::InvalidCube(format!("无效 LUT 尺寸：{value}")))
}

fn parse_triplet(value: &str, label: &str) -> Result<[f64; 3]> {
    let parts = parse_numbers(value)?;
    if parts.len() != 3 {
        return Err(LutError::InvalidCube(format!("{label} 必须包含三个数值")));
    }
    Ok([parts[0], parts[1], parts[2]])
}

fn parse_numbers(value: &str) -> Result<Vec<f64>> {
    value
        .split_whitespace()
        .map(|part| {
            part.parse::<f64>()
                .map_err(|_| LutError::InvalidCube(format!("无法解析数值：{part}")))
        })
        .collect()
}

fn require_unit_domain(minimum: Option<[f64; 3]>, maximum: Option<[f64; 3]>) -> Result<()> {
    if let Some(minimum) = minimum
        && minimum.iter().any(|value| !near(*value, 0.0))
    {
        return Err(LutError::InvalidCube("仅接受 DOMAIN_MIN 0 0 0".to_owned()));
    }
    if let Some(maximum) = maximum
        && maximum.iter().any(|value| !near(*value, 1.0))
    {
        return Err(LutError::InvalidCube("仅接受 DOMAIN_MAX 1 1 1".to_owned()));
    }
    Ok(())
}

fn near(left: f64, right: f64) -> bool {
    (left - right).abs() <= EPSILON
}

fn ensure_cube_extension(path: &Path) -> Result<()> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if extension.eq_ignore_ascii_case("cube") {
        Ok(())
    } else {
        Err(LutError::UnsupportedFormat(
            "输出文件必须使用 .cube 扩展名".to_owned(),
        ))
    }
}

fn filename_warning(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    if stem.len() <= 8
        && stem
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    {
        None
    } else {
        Some("文件名超过 8 位或含非 ASCII 字母数字：仅建议用于 exFAT 卡；FAT32 请改为 8 位字母数字。".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::BufWriter;

    use image::codecs::png::PngEncoder;
    use image::codecs::tiff::TiffEncoder;
    use image::{ExtendedColorType, ImageEncoder};

    use super::*;
    use crate::{
        NeutralFormat, NeutralSampling, NeutralSpec, PhotoSourceImage, generate_neutral,
        generate_photo_master, prepare_photo_reference,
    };

    #[test]
    fn tiff_and_png_neutral_round_trip_to_all_output_grids() {
        let directory = tempfile::tempdir().unwrap();
        for format in [NeutralFormat::Tiff, NeutralFormat::Png] {
            let neutral = directory
                .path()
                .join(format!("Neutral64.{}", format.extension()));
            generate_neutral(
                NeutralSpec {
                    format,
                    ..NeutralSpec::default()
                },
                &neutral,
            )
            .unwrap();

            for target in OUTPUT_GRID_PRESETS {
                let cube = directory
                    .path()
                    .join(format!("{}-{target}.cube", format.extension()));
                let report = convert_cube(&neutral, "Identity Test", target, &cube).unwrap();
                assert_eq!(report.entry_count, target.pow(3));
                assert_eq!(report.clipped_points, 0);
                assert_eq!(report.validation.grid_size, target);

                let parsed = parse_cube(&cube).unwrap();
                for blue in 0..target {
                    for green in 0..target {
                        for red in 0..target {
                            let value = value_at(&parsed, red, green, blue);
                            let denominator = (target - 1) as f64;
                            let expected = [
                                red as f64 / denominator,
                                green as f64 / denominator,
                                blue as f64 / denominator,
                            ];
                            for channel in 0..3 {
                                assert!((value[channel] - expected[channel]).abs() <= 1.0e-5);
                            }
                        }
                    }
                }

                let text = fs::read_to_string(cube).unwrap();
                assert!(
                    text.starts_with(&format!("TITLE \"Identity Test\"\nLUT_3D_SIZE {target}\n"))
                );
                assert!(!text.contains("#LUMIXPHOTOSTYLE"));
                assert!(!text.contains('\r'));
            }
        }
    }

    #[test]
    fn tetrahedral_resampling_preserves_identity_and_linear_matrix() {
        for target in OUTPUT_GRID_PRESETS {
            for matrix in [false, true] {
                let grid = test_grid(64, matrix);
                let output = resample_tetrahedral(&grid, target);
                let denominator = (target - 1) as f64;
                for blue in 0..target {
                    for green in 0..target {
                        for red in 0..target {
                            let value = output[blue * target * target + green * target + red];
                            let input = [
                                red as f64 / denominator,
                                green as f64 / denominator,
                                blue as f64 / denominator,
                            ];
                            let expected = transform(input, matrix);
                            for channel in 0..3 {
                                assert!((value[channel] - expected[channel]).abs() < 1.0e-12);
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn confidence_weighted_smoothing_preserves_affine_luts_and_caps_repairs() {
        let size = 5;
        let center = cube_index(size, 2, 2, 2);
        let mut confidence = vec![
            NodeConfidence {
                noise_standard_deviation: 0.0001,
                used_huber: false,
                removed_clipped_samples: 0,
                quantization_limited: false,
                lossy_compression: false,
            };
            size.pow(3)
        ];
        confidence[center].noise_standard_deviation = 0.01;

        for matrix in [false, true] {
            let mut affine = test_grid(size, matrix);
            let original = affine.values.clone();
            let report =
                smooth_low_confidence_nodes(&mut affine, &confidence, SmoothingMode::Light);
            assert_eq!(report.corrected_node_count, 0);
            assert_eq!(report.maximum_offset, 0.0);
            assert_eq!(affine.values, original);
        }

        let mut noisy = test_grid(size, false);
        noisy.values[center][0] += 0.2;
        let before = noisy.values[center][0];
        let report = smooth_low_confidence_nodes(&mut noisy, &confidence, SmoothingMode::Light);
        assert_eq!(report.corrected_node_count, 1);
        assert!(report.maximum_offset <= SmoothingMode::Light.maximum_offset());
        assert!(noisy.values[center][0] < before);
        assert!(
            before - noisy.values[center][0] <= SmoothingMode::Light.maximum_offset() + 1.0e-12
        );

        let mut medium = test_grid(size, false);
        medium.values[center][0] += 0.2;
        let report = smooth_low_confidence_nodes(&mut medium, &confidence, SmoothingMode::Medium);
        assert_eq!(report.corrected_node_count, 1);
        assert!(report.maximum_offset <= SmoothingMode::Medium.maximum_offset());
        assert!(medium.values[center][0] < noisy.values[center][0]);
    }

    #[test]
    fn automatic_smoothing_is_light_only_for_repeated_neutral_inputs() {
        use crate::raster::NeutralSampling;

        assert_eq!(
            resolve_smoothing_mode(SmoothingMode::Auto, Some(NeutralSampling::Average16)),
            SmoothingMode::Light
        );
        assert_eq!(
            resolve_smoothing_mode(SmoothingMode::Auto, Some(NeutralSampling::Average64)),
            SmoothingMode::Light
        );
        assert_eq!(
            resolve_smoothing_mode(SmoothingMode::Auto, Some(NeutralSampling::Single)),
            SmoothingMode::Off
        );
        assert_eq!(
            resolve_smoothing_mode(SmoothingMode::Auto, None),
            SmoothingMode::Off
        );
        assert_eq!(
            resolve_smoothing_mode(SmoothingMode::Medium, Some(NeutralSampling::Average64)),
            SmoothingMode::Medium
        );
    }

    #[test]
    fn low_precision_refinement_is_bounded_and_preserves_exact_affine_nodes() {
        let quality = RasterQuality {
            bit_depth: 8,
            lossy_jpeg: false,
            assumed_srgb: false,
        };
        let mut exact = test_grid(5, true);
        let original = exact.values.clone();
        let report = refine_low_precision_grid(&mut exact, quality);
        assert_eq!(report.corrected_node_count, 0);
        assert_eq!(exact.values, original);

        let mut quantized = test_grid(5, false);
        for value in &mut quantized.values {
            for channel in value {
                *channel = (*channel * 255.0).round() / 255.0;
            }
        }
        let center = cube_index(5, 2, 2, 2);
        let before = quantized.values[center][0];
        let report = refine_low_precision_grid(&mut quantized, quality);
        assert!(report.corrected_node_count > 0);
        assert!(report.maximum_offset <= 0.5 / 255.0 + 1.0e-12);
        assert!((quantized.values[center][0] - 0.5).abs() < (before - 0.5).abs());
    }

    #[test]
    fn prepared_lut_nodes_are_the_same_nodes_written_by_export() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source64.cube");
        let output = directory.path().join("PREVIEW.cube");
        write_grid(&source, &test_grid(64, true));

        let prepared = prepare_lut(&source, 25).unwrap();
        let report = convert_cube(&source, "Preview Match", 25, &output).unwrap();
        let exported = parse_cube(&output).unwrap();

        assert_eq!(prepared.source_grid_size(), report.source_grid_size);
        assert_eq!(prepared.target_grid_size(), report.output_grid_size);
        assert_eq!(prepared.entry_count(), report.entry_count);
        assert_eq!(prepared.clipped_points(), report.clipped_points);
        for (prepared_value, exported_value) in prepared.grid.values.iter().zip(&exported.values) {
            for channel in 0..3 {
                assert_eq!(prepared_value[channel], exported_value[channel]);
            }
        }
    }

    #[test]
    fn reference_match_improves_holdout_and_preview_matches_export() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.cube");
        let original = directory.path().join("original.png");
        let graded = directory.path().join("graded.png");
        let output = directory.path().join("MATCHED.cube");
        write_grid(&source, &test_grid(17, false));

        let (mut source_pixels, mut graded_pixels) = (Vec::new(), Vec::new());
        for y in 0..96_u32 {
            for x in 0..96_u32 {
                let rgb = [
                    (x * 7 % 256) as u8,
                    (y * 11 % 256) as u8,
                    ((x * 17 + y * 29) % 256) as u8,
                ];
                source_pixels.extend(rgb);
                graded_pixels.extend([
                    (0.90 * f64::from(rgb[0]) + 0.04 * f64::from(rgb[1]) + 5.0)
                        .round()
                        .clamp(0.0, 255.0) as u8,
                    (0.91 * f64::from(rgb[1]) + 0.05 * f64::from(rgb[2]) + 3.0)
                        .round()
                        .clamp(0.0, 255.0) as u8,
                    (0.91 * f64::from(rgb[2]) + 0.04 * f64::from(rgb[0]) + 4.0)
                        .round()
                        .clamp(0.0, 255.0) as u8,
                ]);
            }
        }
        for (path, pixels) in [(&original, &source_pixels), (&graded, &graded_pixels)] {
            let file = fs::File::create(path).unwrap();
            let mut encoder = PngEncoder::new(BufWriter::new(file));
            encoder
                .set_icc_profile(crate::raster::srgb_icc_profile().to_vec())
                .unwrap();
            encoder
                .write_image(pixels, 96, 96, ExtendedColorType::Rgb8)
                .unwrap();
        }
        let reference = ReferenceMatch {
            original_photo: original,
            graded_photo: graded,
        };
        let prepared =
            prepare_lut_with_reference(&source, 17, SmoothingMode::Off, Some(&reference)).unwrap();
        let match_report = prepared.reference_match_report().unwrap();
        assert!(match_report.after_mae_8bit < match_report.before_mae_8bit * 0.6);
        assert_eq!(match_report.sampled_pixels, 96 * 96);

        let report = convert_cube_with_reference(
            &source,
            "Matched",
            17,
            &output,
            SmoothingMode::Off,
            Some(&reference),
        )
        .unwrap();
        let exported = parse_cube(&output).unwrap();
        assert_eq!(report.reference_match, Some(match_report));
        assert_eq!(prepared.grid.values, exported.values);
    }

    #[test]
    fn photo_master_requires_session_baseline_and_shares_diagnostics_and_export_nodes() {
        let directory = tempfile::tempdir().unwrap();
        let master_path = directory.path().join("PhotoMaster512.png");
        let output = directory.path().join("MASTER.cube");
        let (width, height) = (48_u32, 36_u32);
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let value = ((x * 5 + y * 9) % 256) as u8;
                rgba.extend([value, value, value, 255]);
            }
        }
        let source_photo = PhotoSourceImage {
            width,
            height,
            rgba,
        };
        let master =
            generate_photo_master(source_photo.clone(), NeutralFormat::Png, &master_path).unwrap();

        let missing_session = prepare_lut_with_photo_master(
            &master_path,
            17,
            SmoothingMode::Off,
            None,
            ConsistencyMode::Comprehensive,
        );
        assert!(matches!(
            missing_session,
            Err(LutError::MissingPhotoMasterBaseline)
        ));

        let prepared = prepare_lut_with_photo_master(
            &master_path,
            17,
            SmoothingMode::Off,
            Some(PhotoMasterSession {
                baseline: &master.baseline,
                calibration_rect: master.calibration_rect,
            }),
            ConsistencyMode::Comprehensive,
        )
        .unwrap();
        let reopened_reference = prepare_photo_reference(source_photo).unwrap();
        let reopened = prepare_lut_with_photo_master(
            &master_path,
            17,
            SmoothingMode::Off,
            Some(PhotoMasterSession {
                baseline: &reopened_reference.baseline,
                calibration_rect: reopened_reference.calibration_rect,
            }),
            ConsistencyMode::Comprehensive,
        )
        .unwrap();
        assert_eq!(prepared.grid.values, reopened.grid.values);
        assert!(prepared.reference_match_report().is_some());
        let consistency = prepared.consistency_report().unwrap();
        assert_eq!(consistency.mode, ConsistencyMode::Comprehensive);
        assert!(consistency.heldout_mae_8bit.unwrap() < 0.01);
        assert!(consistency.edge_ssim.unwrap() > 0.90);
        assert!(
            !consistency.needs_confirmation(),
            "an unchanged photo master should not trigger the confirmation dialog: {:?}",
            consistency.warnings
        );

        let color_only = prepare_lut_with_photo_master(
            &master_path,
            17,
            SmoothingMode::Off,
            Some(PhotoMasterSession {
                baseline: &master.baseline,
                calibration_rect: master.calibration_rect,
            }),
            ConsistencyMode::ColorMapping,
        )
        .unwrap();
        assert_eq!(prepared.grid.values, color_only.grid.values);
        assert!(color_only.consistency_report().unwrap().edge_ssim.is_none());

        let report = convert_cube_with_photo_master(
            &master_path,
            "Photo Master",
            17,
            &output,
            SmoothingMode::Off,
            Some(PhotoMasterSession {
                baseline: &master.baseline,
                calibration_rect: master.calibration_rect,
            }),
            ConsistencyMode::Comprehensive,
        )
        .unwrap();
        let exported = parse_cube(&output).unwrap();
        assert_eq!(prepared.grid.values, exported.values);
        assert_eq!(report.consistency.unwrap(), *consistency);
        let cached_output = directory.path().join("CACHED.cube");
        let cached_report = export_prepared_cube(&prepared, "Cached", &cached_output).unwrap();
        assert_eq!(
            parse_cube(&cached_output).unwrap().values,
            prepared.grid.values
        );
        assert_eq!(cached_report.consistency.as_ref(), Some(consistency));
    }

    #[test]
    fn rgb8_precision_compensation_is_shared_by_preview_and_export() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("rgb8-neutral.tif");
        let output = directory.path().join("RGB8OUT.cube");
        let mut bytes = Vec::with_capacity(1089 * 33 * 3);
        for green in 0..33 {
            for blue in 0..33 {
                for red in 0..33 {
                    bytes.extend([
                        (red as f64 / 32.0 * 255.0).round() as u8,
                        (green as f64 / 32.0 * 255.0).round() as u8,
                        (blue as f64 / 32.0 * 255.0).round() as u8,
                    ]);
                }
            }
        }
        let file = std::fs::File::create(&source).unwrap();
        let mut encoder = TiffEncoder::new(BufWriter::new(file));
        encoder
            .set_icc_profile(crate::raster::srgb_icc_profile().to_vec())
            .unwrap();
        encoder
            .write_image(&bytes, 1089, 33, ExtendedColorType::Rgb8)
            .unwrap();

        let prepared = prepare_lut_with_smoothing(&source, 33, SmoothingMode::Auto).unwrap();
        let report =
            convert_cube_with_smoothing(&source, "RGB8 Match", 33, &output, SmoothingMode::Auto)
                .unwrap();
        let exported = parse_cube(&output).unwrap();
        assert_eq!(prepared.smoothing_report().mode, SmoothingMode::Off);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("RGB8"))
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("低精度补偿"))
        );
        for (prepared_value, exported_value) in prepared.grid.values.iter().zip(&exported.values) {
            assert_eq!(prepared_value, exported_value);
        }
    }

    #[test]
    fn anti_grain_average_is_shared_by_preview_and_export() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("Neutral64_Grain16.png");
        let output = directory.path().join("GRAIN16.cube");
        generate_neutral(
            NeutralSpec {
                sampling: NeutralSampling::Average16,
                ..NeutralSpec::default()
            },
            &source,
        )
        .unwrap();

        let prepared = prepare_lut_with_smoothing(&source, 33, SmoothingMode::Auto).unwrap();
        let report =
            convert_cube_with_smoothing(&source, "Grain Average", 33, &output, SmoothingMode::Auto)
                .unwrap();
        let exported = parse_cube(&output).unwrap();
        let prepared_statistics = prepared.grain_statistics().unwrap();
        let report_statistics = report.grain_statistics.as_ref().unwrap();

        assert_eq!(prepared_statistics.samples_per_node, 16);
        assert_eq!(prepared_statistics, report_statistics);
        assert_eq!(prepared_statistics.input_sample_count, 262_144 * 16);
        assert_eq!(prepared_statistics.averaged_node_count, 262_144);
        assert_eq!(prepared_statistics.max_standard_deviation, 0.0);
        assert_eq!(prepared_statistics.arithmetic_node_count, 262_144);
        assert_eq!(prepared_statistics.huber_node_count, 0);
        assert_eq!(prepared_statistics.removed_clipped_samples, 0);
        assert_eq!(prepared.smoothing_report().mode, SmoothingMode::Light);
        assert_eq!(report.smoothing.mode, SmoothingMode::Light);
        assert_eq!(report.smoothing.corrected_node_count, 0);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("自适应稳健聚合"))
        );
        for (prepared_value, exported_value) in prepared.grid.values.iter().zip(&exported.values) {
            assert_eq!(prepared_value, exported_value);
        }
    }

    #[test]
    fn reports_downsampling_upscaling_and_generic_64_output() {
        let directory = tempfile::tempdir().unwrap();
        let source_64 = directory.path().join("source64.cube");
        write_grid(&source_64, &test_grid(64, false));
        let downsampled = convert_cube(
            &source_64,
            "Downsample",
            17,
            directory.path().join("DOWN17.cube"),
        )
        .unwrap();
        assert!(
            downsampled
                .warnings
                .iter()
                .any(|warning| warning.contains("降采样"))
        );

        let source_17 = directory.path().join("source17.cube");
        write_grid(&source_17, &test_grid(17, false));
        let upscaled = convert_cube(
            &source_17,
            "Upscale",
            64,
            directory.path().join("up64.cube"),
        )
        .unwrap();
        assert!(
            upscaled
                .warnings
                .iter()
                .any(|warning| warning.contains("不能恢复"))
        );
        assert!(
            upscaled
                .warnings
                .iter()
                .any(|warning| warning.contains("不能导入 LUMIX 相机"))
        );
        assert_eq!(upscaled.entry_count, 64_usize.pow(3));
    }

    #[test]
    fn validator_ignores_comments_and_rejects_invalid_target() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("COMMENT.cube");
        let grid = test_grid(17, false);
        write_grid(&path, &grid);
        let mut text = fs::read_to_string(&path).unwrap();
        text = text.replacen('\n', "\n# an ordinary comment\n", 1);
        fs::write(&path, text).unwrap();
        assert_eq!(validate_cube(&path).unwrap().entry_count, 17_usize.pow(3));

        assert!(convert_cube(&path, "Invalid", 18, directory.path().join("bad.cube")).is_err());
    }

    #[test]
    fn rejects_non_unit_domain_and_one_dimensional_lut() {
        let directory = tempfile::tempdir().unwrap();
        let custom_domain = directory.path().join("domain.cube");
        fs::write(
            &custom_domain,
            "LUT_3D_SIZE 2\nDOMAIN_MIN -0.1 0 0\nDOMAIN_MAX 1 1 1\n0 0 0\n0 0 0\n0 0 0\n0 0 0\n0 0 0\n0 0 0\n0 0 0\n0 0 0\n",
        )
        .unwrap();
        assert!(parse_cube(&custom_domain).is_err());

        let one_d = directory.path().join("one.cube");
        fs::write(&one_d, "LUT_1D_SIZE 2\n0 0 0\n1 1 1\n").unwrap();
        assert!(parse_cube(&one_d).is_err());
    }

    #[test]
    fn rejects_incorrect_entry_counts_and_non_finite_values() {
        let directory = tempfile::tempdir().unwrap();
        let short = directory.path().join("short.cube");
        fs::write(
            &short,
            "LUT_3D_SIZE 2\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 1 1 1\n0 0 0\n",
        )
        .unwrap();
        assert!(parse_cube(&short).is_err());

        for (name, value) in [("nan", "NaN"), ("infinity", "inf")] {
            let path = directory.path().join(format!("{name}.cube"));
            let mut text = String::from("LUT_3D_SIZE 2\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 1 1 1\n");
            for index in 0..8 {
                let red = if index == 3 { value } else { "0" };
                text.push_str(&format!("{red} 0 0\n"));
            }
            fs::write(&path, text).unwrap();
            assert!(parse_cube(&path).is_err());
        }
    }

    #[test]
    fn clamps_out_of_range_values_and_reports_them() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.cube");
        let output = directory.path().join("CLAMPED.cube");
        let mut text = String::from("LUT_3D_SIZE 2\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 1 1 1\n");
        for index in 0..8 {
            let value = if index == 0 {
                -0.25
            } else {
                index as f64 / 7.0
            };
            text.push_str(&format!(
                "{value} {value} {}\n",
                if index == 7 { 1.25 } else { value }
            ));
        }
        fs::write(&source, text).unwrap();
        let report = convert_cube(&source, "Clamp", 17, output).unwrap();
        assert!(report.clipped_points > 0);
        assert!(report.validation.min_value >= 0.0);
        assert!(report.validation.max_value <= 1.0);
    }

    fn test_grid(size: usize, matrix: bool) -> CubeGrid {
        let denominator = (size - 1) as f64;
        let mut values = Vec::with_capacity(size.pow(3));
        for blue in 0..size {
            for green in 0..size {
                for red in 0..size {
                    values.push(transform(
                        [
                            red as f64 / denominator,
                            green as f64 / denominator,
                            blue as f64 / denominator,
                        ],
                        matrix,
                    ));
                }
            }
        }
        CubeGrid { size, values }
    }

    fn transform(input: [f64; 3], matrix: bool) -> [f64; 3] {
        if matrix {
            [
                0.8 * input[0] + 0.1 * input[1],
                0.7 * input[1] + 0.2 * input[2],
                0.1 * input[0] + 0.8 * input[2],
            ]
        } else {
            input
        }
    }

    fn write_grid(path: &Path, grid: &CubeGrid) {
        write_cube(path, "Test Grid", grid.size, &grid.values).unwrap();
    }
}
