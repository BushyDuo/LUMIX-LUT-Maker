use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use image::codecs::png::PngEncoder;
use image::codecs::tiff::TiffEncoder;
use image::imageops::FilterType;
use image::{
    DynamicImage, ExtendedColorType, ImageBuffer, ImageDecoder, ImageEncoder, ImageReader, Rgb,
};

use crate::error::{LutError, Result, io_error};

const SRGB_ICC_PROFILE: &[u8] = include_bytes!("../assets/color/sRGB2014.icc");

/// Returns the exact sRGB profile embedded in generated neutral images and used
/// as the destination color space for platform preview decoders.
pub fn srgb_icc_profile() -> &'static [u8] {
    SRGB_ICC_PROFILE
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSpace {
    Srgb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeutralFormat {
    Tiff,
    Png,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeutralSampling {
    Single,
    Average16,
    Average64,
}

impl NeutralSampling {
    pub const fn repetitions_per_axis(self) -> u32 {
        match self {
            Self::Single => 1,
            Self::Average16 => 4,
            Self::Average64 => 8,
        }
    }

    pub const fn samples_per_node(self) -> usize {
        let repeats = self.repetitions_per_axis() as usize;
        repeats * repeats
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Single => "普通",
            Self::Average16 => "抗颗粒 16×",
            Self::Average64 => "抗颗粒 64×",
        }
    }

    pub const fn filename_stem(self) -> &'static str {
        match self {
            Self::Single => "Neutral64",
            Self::Average16 => "Neutral64_Grain16",
            Self::Average64 => "Neutral64_Grain64",
        }
    }

    pub const fn default_format(self) -> NeutralFormat {
        NeutralFormat::Png
    }

    pub const fn output_dimension(self, base_dimension: u32) -> u32 {
        base_dimension * self.repetitions_per_axis()
    }
}

impl NeutralFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Tiff => "tif",
            Self::Png => "png",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Tiff => "TIFF",
            Self::Png => "PNG",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NeutralSpec {
    pub grid_size: u32,
    pub bit_depth: u8,
    pub color_space: ColorSpace,
    pub format: NeutralFormat,
    pub sampling: NeutralSampling,
}

impl Default for NeutralSpec {
    fn default() -> Self {
        Self {
            grid_size: 64,
            bit_depth: 16,
            color_space: ColorSpace::Srgb,
            format: NeutralFormat::Png,
            sampling: NeutralSampling::Single,
        }
    }
}

/// Pixel buffer for the complete left calibration panel, including the
/// black-to-white ramp in any letterboxed area. Samples are unpremultiplied
/// sRGB RGB16, top-to-bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhotoReferenceBuffer {
    width: u32,
    height: u32,
    pixels: Vec<[u16; 3]>,
}

#[derive(Debug, Clone, Copy)]
pub struct PhotoMasterSession<'a> {
    pub baseline: &'a PhotoReferenceBuffer,
    pub calibration_rect: (u32, u32, u32, u32),
}

impl PhotoReferenceBuffer {
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub(crate) fn pixels(&self) -> &[[u16; 3]] {
        &self.pixels
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhotoMasterReport {
    pub output_path: PathBuf,
    pub canvas_size: u32,
    pub sampling: NeutralSampling,
    /// Bounds of the original photo after aspect-preserving fit.
    pub photo_rect: (u32, u32, u32, u32),
    /// Full square region used for photo and grayscale-ramp calibration.
    pub calibration_rect: (u32, u32, u32, u32),
    pub baseline: PhotoReferenceBuffer,
}

/// Reconstructs the calibration side of a master from the original photo.
/// This lets a graded master be reopened after an app restart without writing
/// a second, ungraded master to disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhotoReferenceReport {
    pub canvas_size: u32,
    pub photo_rect: (u32, u32, u32, u32),
    pub calibration_rect: (u32, u32, u32, u32),
    pub baseline: PhotoReferenceBuffer,
}

/// An sRGB RGBA8 photograph buffer. RGB channels are unpremultiplied; alpha
/// is composited over neutral gray when the calibration master is generated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhotoSourceImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhotoMasterLayout {
    pub canvas_size: u32,
    pub sampling: NeutralSampling,
}

pub fn photo_master_layout(width: u32, height: u32) -> Result<PhotoMasterLayout> {
    if width == 0 || height == 0 {
        return Err(LutError::InvalidRaster("原图尺寸无效".to_owned()));
    }
    let edge = width.max(height);
    let (canvas_size, sampling) = if edge <= 512 {
        (512, NeutralSampling::Single)
    } else if edge <= 2048 {
        (2048, NeutralSampling::Average16)
    } else {
        (4096, NeutralSampling::Average64)
    };
    Ok(PhotoMasterLayout {
        canvas_size,
        sampling,
    })
}

pub fn prepare_photo_reference(source: PhotoSourceImage) -> Result<PhotoReferenceReport> {
    let expected_bytes = source.width as usize * source.height as usize * 4;
    if source.width == 0 || source.height == 0 || source.rgba.len() != expected_bytes {
        return Err(LutError::InvalidRaster("原图像素数据尺寸无效".to_owned()));
    }
    let size = photo_master_layout(source.width, source.height)?.canvas_size;
    let scale = (size as f64 / source.width.max(source.height) as f64).min(1.0);
    let photo_width = ((source.width as f64 * scale).round() as u32).clamp(1, size);
    let photo_height = ((source.height as f64 * scale).round() as u32).clamp(1, size);
    let content_x = (size - photo_width) / 2;
    let content_y = (size - photo_height) / 2;
    let rgba = image::RgbaImage::from_raw(source.width, source.height, source.rgba)
        .ok_or_else(|| LutError::InvalidRaster("无法解码原图像素数据".to_owned()))?;
    let resized = image::imageops::resize(&rgba, photo_width, photo_height, FilterType::Lanczos3);
    let mut pixels = Vec::with_capacity((size * size) as usize);
    for y in 0..size {
        let in_photo_y = y >= content_y && y < content_y + photo_height;
        for x in 0..size {
            let value = if in_photo_y && x >= content_x && x < content_x + photo_width {
                let pixel = resized.get_pixel(x - content_x, y - content_y).0;
                let alpha = u32::from(pixel[3]);
                [pixel[0], pixel[1], pixel[2]].map(|channel| {
                    let composite = (u32::from(channel) * alpha + 128 * (255 - alpha) + 127) / 255;
                    composite as u16 * 257
                })
            } else {
                let coordinate = if !in_photo_y { x } else { y };
                let gray = scale_to_u16(coordinate, size);
                [gray; 3]
            };
            pixels.push(value);
        }
    }
    Ok(PhotoReferenceReport {
        canvas_size: size,
        photo_rect: (content_x, content_y, photo_width, photo_height),
        calibration_rect: (0, 0, size, size),
        baseline: PhotoReferenceBuffer {
            width: size,
            height: size,
            pixels,
        },
    })
}

/// Generates a side-by-side photo calibration master. The left photo is fit
/// without cropping or upscaling; the right side is the matching Neutral64
/// chart, repeated as complete tiles for robust grain averaging.
pub fn generate_photo_master(
    source: PhotoSourceImage,
    format: NeutralFormat,
    destination: impl AsRef<Path>,
) -> Result<PhotoMasterReport> {
    let expected_bytes = source.width as usize * source.height as usize * 4;
    if source.width == 0 || source.height == 0 || source.rgba.len() != expected_bytes {
        return Err(LutError::InvalidRaster("原图像素数据尺寸无效".to_owned()));
    }
    let destination = destination.as_ref();
    let extension = destination
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let extension_matches = match format {
        NeutralFormat::Tiff => matches!(extension.as_str(), "tif" | "tiff"),
        NeutralFormat::Png => extension == "png",
    };
    if !extension_matches {
        return Err(LutError::UnsupportedFormat(format!(
            "已选择 {}，输出扩展名必须为 .{}",
            format.display_name(),
            format.extension()
        )));
    }
    let layout = photo_master_layout(source.width, source.height)?;
    let size = layout.canvas_size;
    let scale = (size as f64 / source.width.max(source.height) as f64).min(1.0);
    let photo_width = ((source.width as f64 * scale).round() as u32).clamp(1, size);
    let photo_height = ((source.height as f64 * scale).round() as u32).clamp(1, size);
    let content_x = (size - photo_width) / 2;
    let content_y = (size - photo_height) / 2;

    let rgba = image::RgbaImage::from_raw(source.width, source.height, source.rgba.clone())
        .ok_or_else(|| LutError::InvalidRaster("无法解码原图像素数据".to_owned()))?;
    let resized = image::imageops::resize(&rgba, photo_width, photo_height, FilterType::Lanczos3);
    let mut chart_base = Vec::<u16>::with_capacity((512 * 512 * 3) as usize);
    for blue in 0..64 {
        for green in 0..64 {
            for red in 0..64 {
                chart_base.extend([
                    scale_to_u16(red, 64),
                    scale_to_u16(green, 64),
                    scale_to_u16(blue, 64),
                ]);
            }
        }
    }
    let mut bytes = Vec::with_capacity((size as usize) * (size as usize * 2) * 6);
    let mut baseline_pixels = Vec::with_capacity((size * size) as usize);
    for y in 0..size {
        let in_photo_y = y >= content_y && y < content_y + photo_height;
        let local_y = y as usize % 512;
        for x in 0..size * 2 {
            let value = if x < size {
                if in_photo_y && x >= content_x && x < content_x + photo_width {
                    let pixel = resized.get_pixel(x - content_x, y - content_y).0;
                    let alpha = u32::from(pixel[3]);
                    let composite = [pixel[0], pixel[1], pixel[2]].map(|channel| {
                        ((u32::from(channel) * alpha + 128 * (255 - alpha) + 127) / 255) as u8
                    });
                    composite.map(|channel| u16::from(channel) * 257)
                } else {
                    // Put a deterministic grayscale ramp into the unused
                    // letterbox. Landscape sources expose top/bottom strips,
                    // so those use a horizontal ramp; portrait sources expose
                    // left/right strips and use a vertical ramp. The ramp is
                    // also part of the calibration baseline below.
                    let coordinate = if !in_photo_y {
                        x
                    } else if x < content_x || x >= content_x + photo_width {
                        y
                    } else {
                        size / 2
                    };
                    let gray = scale_to_u16(coordinate, size);
                    [gray; 3]
                }
            } else {
                let local_x = (x - size) as usize % 512;
                let source_offset = local_y * 512 * 3 + local_x * 3;
                [
                    chart_base[source_offset],
                    chart_base[source_offset + 1],
                    chart_base[source_offset + 2],
                ]
            };
            if x < size {
                baseline_pixels.push(value);
            }
            for sample in value {
                bytes.extend_from_slice(&sample.to_ne_bytes());
            }
        }
    }
    let baseline = PhotoReferenceBuffer {
        width: size,
        height: size,
        pixels: baseline_pixels,
    };
    let file = File::create(destination).map_err(|error| io_error(destination, error))?;
    let writer = BufWriter::new(file);
    match format {
        NeutralFormat::Tiff => {
            let mut encoder = TiffEncoder::new(writer);
            encoder
                .set_icc_profile(SRGB_ICC_PROFILE.to_vec())
                .map_err(|error| LutError::InvalidRaster(format!("无法嵌入 sRGB ICC：{error}")))?;
            encoder.write_image(&bytes, size * 2, size, ExtendedColorType::Rgb16)?;
        }
        NeutralFormat::Png => {
            let mut encoder = PngEncoder::new(writer);
            encoder
                .set_icc_profile(SRGB_ICC_PROFILE.to_vec())
                .map_err(|error| LutError::InvalidRaster(format!("无法嵌入 sRGB ICC：{error}")))?;
            encoder.write_image(&bytes, size * 2, size, ExtendedColorType::Rgb16)?;
        }
    }
    Ok(PhotoMasterReport {
        output_path: destination.to_path_buf(),
        canvas_size: size,
        sampling: layout.sampling,
        photo_rect: (content_x, content_y, photo_width, photo_height),
        calibration_rect: (0, 0, size, size),
        baseline,
    })
}

impl NeutralSpec {
    pub fn base_dimensions(self) -> (u32, u32) {
        packed_dimensions(self.grid_size)
    }

    pub fn output_dimensions(self) -> (u32, u32) {
        let (width, height) = self.base_dimensions();
        (
            self.sampling.output_dimension(width),
            self.sampling.output_dimension(height),
        )
    }

    pub fn default_filename(self) -> String {
        format!(
            "{}.{}",
            self.sampling.filename_stem(),
            self.format.extension()
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GrainStatistics {
    pub sampling: NeutralSampling,
    pub input_width: u32,
    pub input_height: u32,
    pub samples_per_node: usize,
    pub input_sample_count: usize,
    pub averaged_node_count: usize,
    pub mean_standard_deviation: f64,
    pub p95_standard_deviation: f64,
    pub max_standard_deviation: f64,
    pub arithmetic_node_count: usize,
    pub huber_node_count: usize,
    pub clipped_node_count: usize,
    pub removed_clipped_samples: usize,
}

pub fn generate_neutral(spec: NeutralSpec, destination: impl AsRef<Path>) -> Result<PathBuf> {
    if !(2..=64).contains(&spec.grid_size)
        || spec.bit_depth != 16
        || spec.color_space != ColorSpace::Srgb
    {
        return Err(LutError::InvalidRaster(
            "中性图必须为 2–64 grid、16-bit、sRGB".to_owned(),
        ));
    }

    let destination = destination.as_ref();
    let extension = destination
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let extension_matches = match spec.format {
        NeutralFormat::Tiff => matches!(extension.as_str(), "tif" | "tiff"),
        NeutralFormat::Png => extension == "png",
    };
    if !extension_matches {
        return Err(LutError::UnsupportedFormat(format!(
            "已选择 {}，输出扩展名必须为 .{}",
            spec.format.display_name(),
            spec.format.extension()
        )));
    }

    let size = spec.grid_size;
    let (base_width, base_height) = spec.base_dimensions();
    let (width, height) = spec.output_dimensions();
    let mut base_samples = Vec::<u16>::with_capacity((base_width * base_height * 3) as usize);

    // Packed row-major layout follows CUBE order: red fastest, then green, then blue.
    for blue in 0..size {
        for green in 0..size {
            for red in 0..size {
                base_samples.push(scale_to_u16(red, size));
                base_samples.push(scale_to_u16(green, size));
                base_samples.push(scale_to_u16(blue, size));
            }
        }
    }

    let repeats = spec.sampling.repetitions_per_axis();
    let base_row_samples = base_width as usize * 3;
    let mut bytes = Vec::with_capacity(width as usize * height as usize * 6);
    // Repeat the complete packed Neutral64 image as independent tiles. This is
    // intentionally different from enlarging each pixel into a solid block.
    for _tile_y in 0..repeats {
        for base_y in 0..base_height as usize {
            let row = &base_samples[base_y * base_row_samples..(base_y + 1) * base_row_samples];
            for _tile_x in 0..repeats {
                for sample in row {
                    bytes.extend_from_slice(&sample.to_ne_bytes());
                }
            }
        }
    }

    let file = File::create(destination).map_err(|error| io_error(destination, error))?;
    let writer = BufWriter::new(file);
    match spec.format {
        NeutralFormat::Tiff => {
            let mut encoder = TiffEncoder::new(writer);
            encoder
                .set_icc_profile(SRGB_ICC_PROFILE.to_vec())
                .map_err(|error| LutError::InvalidRaster(format!("无法嵌入 sRGB ICC：{error}")))?;
            encoder.write_image(&bytes, width, height, ExtendedColorType::Rgb16)?;
        }
        NeutralFormat::Png => {
            let mut encoder = PngEncoder::new(writer);
            encoder
                .set_icc_profile(SRGB_ICC_PROFILE.to_vec())
                .map_err(|error| LutError::InvalidRaster(format!("无法嵌入 sRGB ICC：{error}")))?;
            encoder.write_image(&bytes, width, height, ExtendedColorType::Rgb16)?;
        }
    }

    Ok(destination.to_path_buf())
}

fn scale_to_u16(value: u32, size: u32) -> u16 {
    ((value as f64 / (size - 1) as f64) * u16::MAX as f64).round() as u16
}

fn packed_dimensions(size: u32) -> (u32, u32) {
    let pixels = size.pow(3);
    let mut height = (pixels as f64).sqrt().floor() as u32;
    while !pixels.is_multiple_of(height) {
        height -= 1;
    }
    (pixels / height, height)
}

#[derive(Debug)]
pub(crate) struct RasterLut {
    pub size: usize,
    pub values: Vec<[f64; 3]>,
    pub grain_statistics: Option<GrainStatistics>,
    pub node_confidence: Option<Vec<NodeConfidence>>,
    pub quality: RasterQuality,
    pub layout_note: Option<String>,
    pub photo_master: Option<PhotoMasterRaster>,
}

#[derive(Debug, Clone)]
pub(crate) struct PhotoMasterRaster {
    pub canvas_size: u32,
    pub photo_pixels: Vec<[u16; 3]>,
}

const ACR_COMPOSITE_WIDTH: u32 = 14_762;
const ACR_COMPOSITE_HEIGHT: u32 = 5_999;
const ACR_COMPOSITE_PHOTO_WIDTH: u32 = 10_666;
const ACR_COMPOSITE_CHART_TOP: u32 = 951;
const ACR_COMPOSITE_CHART_SIZE: u32 = 4_096;

#[derive(Debug, Clone, Copy)]
pub(crate) struct RasterQuality {
    pub bit_depth: u8,
    pub lossy_jpeg: bool,
    pub assumed_srgb: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct NodeConfidence {
    pub noise_standard_deviation: f64,
    pub used_huber: bool,
    pub removed_clipped_samples: usize,
    pub quantization_limited: bool,
    pub lossy_compression: bool,
}

pub(crate) fn load_raster_lut(path: &Path) -> Result<RasterLut> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(extension.as_str(), "tif" | "tiff" | "png" | "jpg" | "jpeg") {
        return Err(LutError::UnsupportedFormat(format!(
            "不支持 .{extension} 图像；请选择 TIFF、PNG 或 JPEG"
        )));
    }

    let reader = ImageReader::open(path)
        .map_err(|error| io_error(path, error))?
        .with_guessed_format()
        .map_err(|error| io_error(path, error))?;
    let mut decoder = reader.into_decoder()?;
    let (source_width, source_height) = decoder.dimensions();
    let original_color = decoder.original_color_type();
    if !matches!(
        original_color,
        ExtendedColorType::Rgb16 | ExtendedColorType::Rgb8
    ) {
        return Err(LutError::InvalidRaster(format!(
            "检测到 {original_color:?}，要求 RGB 图像（不允许灰度、CMYK 或 Alpha）"
        )));
    }

    let lossy_jpeg = matches!(extension.as_str(), "jpg" | "jpeg");
    let icc = decoder.icc_profile()?;
    if icc
        .as_deref()
        .is_some_and(|profile| !is_srgb_profile(profile))
    {
        return Err(LutError::InvalidRaster(
            "ICC profile 不是 sRGB，请在调色软件的导出选项中选择 sRGB".to_owned(),
        ));
    }
    if icc.is_none() && !lossy_jpeg {
        return Err(LutError::InvalidRaster(
            "缺少 ICC profile，请从调色软件以 sRGB 导出".to_owned(),
        ));
    }

    let image = DynamicImage::from_decoder(decoder)?;
    let (mut rgb, bit_depth) = match image {
        DynamicImage::ImageRgb16(image) => (image, 16),
        DynamicImage::ImageRgb8(image) => (
            ImageBuffer::from_fn(image.width(), image.height(), |x, y| {
                Rgb(image.get_pixel(x, y).0.map(|value| u16::from(value) * 257))
            }),
            8,
        ),
        _ => {
            return Err(LutError::InvalidRaster(
                "解码器没有保留 RGB 样本".to_owned(),
            ));
        }
    };
    let mut photo_master = None;
    let layout_note = if let Some(master_size) = photo_master_canvas(source_width, source_height) {
        let mut photo_pixels = Vec::with_capacity((master_size * master_size) as usize);
        for y in 0..master_size {
            for x in 0..master_size {
                photo_pixels.push(rgb.get_pixel(x, y).0);
            }
        }
        let chart = ImageBuffer::from_fn(master_size, master_size, |x, y| {
            *rgb.get_pixel(master_size + x, y)
        });
        rgb = chart;
        photo_master = Some(PhotoMasterRaster {
            canvas_size: master_size,
            photo_pixels,
        });
        Some(format!(
            "已识别 {}×{} 照片校准母版：右侧中性图用于生成 LUT，左侧照片与黑白渐变留边参与本次会话校准",
            source_width, source_height
        ))
    } else if (source_width, source_height) == (ACR_COMPOSITE_WIDTH, ACR_COMPOSITE_HEIGHT) {
        if bit_depth != 16 {
            return Err(LutError::InvalidRaster(
                "ACR 合成图布局要求 RGB16 TIFF/PNG；请将 Camera Raw 输出位深设为 16 位".to_owned(),
            ));
        }
        let chart = ImageBuffer::from_fn(
            ACR_COMPOSITE_CHART_SIZE,
            ACR_COMPOSITE_CHART_SIZE,
            |x, y| *rgb.get_pixel(ACR_COMPOSITE_PHOTO_WIDTH + x, ACR_COMPOSITE_CHART_TOP + y),
        );
        rgb = chart;
        Some(format!(
            "已识别 ACR 合成图 {source_width}×{source_height}，按原像素从 x={}、y={} 提取右侧 {}×{} Neutral64 区域",
            ACR_COMPOSITE_PHOTO_WIDTH,
            ACR_COMPOSITE_CHART_TOP,
            ACR_COMPOSITE_CHART_SIZE,
            ACR_COMPOSITE_CHART_SIZE
        ))
    } else {
        None
    };
    let (width, height) = rgb.dimensions();
    let quality = RasterQuality {
        bit_depth,
        lossy_jpeg,
        assumed_srgb: icc.is_none(),
    };

    if let Some(sampling) = repeated_sampling(width, height) {
        let (values, grain_statistics, node_confidence) =
            average_repeated_neutral(&rgb, sampling, quality);
        return Ok(RasterLut {
            size: 64,
            values,
            grain_statistics: Some(grain_statistics),
            node_confidence: Some(node_confidence),
            quality,
            layout_note,
            photo_master,
        });
    }

    if let Some(size) = unwrapped_size(width, height) {
        let mut values = Vec::with_capacity(size * size * size);
        for blue in 0..size {
            for green in 0..size {
                for red in 0..size {
                    let pixel = rgb.get_pixel((blue * size + red) as u32, green as u32).0;
                    values.push(normalize(pixel));
                }
            }
        }
        let node_confidence = (bit_depth == 8 || lossy_jpeg)
            .then(|| vec![low_precision_confidence(quality); values.len()]);
        return Ok(RasterLut {
            size,
            values,
            grain_statistics: None,
            node_confidence,
            quality,
            layout_note,
            photo_master,
        });
    }

    if let Some(size) = packed_size(width, height) {
        let values: Vec<[f64; 3]> = rgb.pixels().map(|pixel| normalize(pixel.0)).collect();
        let grain_statistics =
            (width == 512 && height == 512 && size == 64).then(|| GrainStatistics {
                sampling: NeutralSampling::Single,
                input_width: width,
                input_height: height,
                samples_per_node: 1,
                input_sample_count: size.pow(3),
                averaged_node_count: size.pow(3),
                mean_standard_deviation: 0.0,
                p95_standard_deviation: 0.0,
                max_standard_deviation: 0.0,
                arithmetic_node_count: size.pow(3),
                huber_node_count: 0,
                clipped_node_count: 0,
                removed_clipped_samples: 0,
            });
        let node_confidence = (bit_depth == 8 || lossy_jpeg)
            .then(|| vec![low_precision_confidence(quality); values.len()]);
        return Ok(RasterLut {
            size,
            values,
            grain_statistics,
            node_confidence,
            quality,
            layout_note,
            photo_master,
        });
    }

    Err(LutError::InvalidRaster(format!(
        "尺寸为 {width}×{height}；像素总数必须恰好为 N³，或使用兼容的 N²×N 展开布局"
    )))
}

fn photo_master_canvas(width: u32, height: u32) -> Option<u32> {
    [(512, 1024), (2048, 4096), (4096, 8192)]
        .into_iter()
        .find_map(|(size, expected_width)| {
            ((width, height) == (expected_width, size)).then_some(size)
        })
}

fn repeated_sampling(width: u32, height: u32) -> Option<NeutralSampling> {
    match (width, height) {
        (2048, 2048) => Some(NeutralSampling::Average16),
        (4096, 4096) => Some(NeutralSampling::Average64),
        _ => None,
    }
}

fn combined_uncertainty(measured: f64, quality: RasterQuality) -> f64 {
    let quantization_sigma = if quality.bit_depth == 8 {
        1.0 / (12.0_f64.sqrt() * 255.0)
    } else {
        0.0
    };
    // JPEG's exact error cannot be reconstructed from the decoded raster. A
    // conservative sub-code uncertainty keeps later fitting inside the range
    // where block/DCT artefacts are more likely than an intentional LUT bend.
    let compression_sigma = if quality.lossy_jpeg {
        0.75 / 255.0
    } else {
        0.0
    };
    (measured * measured
        + quantization_sigma * quantization_sigma
        + compression_sigma * compression_sigma)
        .sqrt()
}

fn low_precision_confidence(quality: RasterQuality) -> NodeConfidence {
    NodeConfidence {
        noise_standard_deviation: combined_uncertainty(0.0, quality),
        used_huber: false,
        removed_clipped_samples: 0,
        quantization_limited: quality.bit_depth == 8,
        lossy_compression: quality.lossy_jpeg,
    }
}

fn average_repeated_neutral(
    image: &ImageBuffer<Rgb<u16>, Vec<u16>>,
    sampling: NeutralSampling,
    quality: RasterQuality,
) -> (Vec<[f64; 3]>, GrainStatistics, Vec<NodeConfidence>) {
    const BASE_DIMENSION: u32 = 512;
    let repeats = sampling.repetitions_per_axis();
    debug_assert_eq!(image.width(), BASE_DIMENSION * repeats);
    debug_assert_eq!(image.height(), BASE_DIMENSION * repeats);

    let sample_count = sampling.samples_per_node();
    let node_count = BASE_DIMENSION as usize * BASE_DIMENSION as usize;
    let mut values = Vec::with_capacity(node_count);
    let mut node_confidence = Vec::with_capacity(node_count);
    let mut deviations = Vec::with_capacity(node_count * 3);
    let mut samples = Vec::<[u16; 3]>::with_capacity(sample_count);
    let mut huber_node_count = 0;
    let mut clipped_node_count = 0;
    let mut removed_clipped_samples = 0;

    for y in 0..BASE_DIMENSION {
        for x in 0..BASE_DIMENSION {
            samples.clear();
            let mut sums = [0.0_f64; 3];
            let mut squared_sums = [0.0_f64; 3];
            for tile_y in 0..repeats {
                for tile_x in 0..repeats {
                    let pixel = image
                        .get_pixel(tile_x * BASE_DIMENSION + x, tile_y * BASE_DIMENSION + y)
                        .0;
                    samples.push(pixel);
                    for channel in 0..3 {
                        let value = f64::from(pixel[channel]);
                        sums[channel] += value;
                        squared_sums[channel] += value * value;
                    }
                }
            }

            let mut node_variance_sum = 0.0;
            for channel in 0..3 {
                let mean = sums[channel] / sample_count as f64;
                let variance = (squared_sums[channel] / sample_count as f64 - mean * mean).max(0.0);
                let standard_deviation = variance.sqrt() / f64::from(u16::MAX);
                deviations.push(standard_deviation);
                node_variance_sum += standard_deviation * standard_deviation;
            }

            let aggregate = aggregate_node_samples(&samples);
            if aggregate.used_huber {
                huber_node_count += 1;
            }
            if aggregate.removed_clipped_samples > 0 {
                clipped_node_count += 1;
                removed_clipped_samples += aggregate.removed_clipped_samples;
            }
            values.push(aggregate.value.map(|value| value / f64::from(u16::MAX)));
            node_confidence.push(NodeConfidence {
                noise_standard_deviation: combined_uncertainty(
                    (node_variance_sum / 3.0).sqrt(),
                    quality,
                ),
                used_huber: aggregate.used_huber,
                removed_clipped_samples: aggregate.removed_clipped_samples,
                quantization_limited: quality.bit_depth == 8,
                lossy_compression: quality.lossy_jpeg,
            });
        }
    }

    deviations.sort_by(f64::total_cmp);
    let deviation_count = deviations.len();
    let mean_standard_deviation = deviations.iter().sum::<f64>() / deviation_count as f64;
    let p95_index = ((deviation_count as f64 * 0.95).ceil() as usize)
        .saturating_sub(1)
        .min(deviation_count - 1);
    let p95_standard_deviation = deviations[p95_index];
    let max_standard_deviation = *deviations.last().unwrap_or(&0.0);

    (
        values,
        GrainStatistics {
            sampling,
            input_width: image.width(),
            input_height: image.height(),
            samples_per_node: sample_count,
            input_sample_count: node_count * sample_count,
            averaged_node_count: node_count,
            mean_standard_deviation,
            p95_standard_deviation,
            max_standard_deviation,
            arithmetic_node_count: node_count - huber_node_count,
            huber_node_count,
            clipped_node_count,
            removed_clipped_samples,
        },
        node_confidence,
    )
}

struct NodeAggregate {
    value: [f64; 3],
    used_huber: bool,
    removed_clipped_samples: usize,
}

fn aggregate_node_samples(samples: &[[u16; 3]]) -> NodeAggregate {
    debug_assert!(!samples.is_empty());
    if samples.iter().all(|sample| sample == &samples[0]) {
        return NodeAggregate {
            value: samples[0].map(f64::from),
            used_huber: false,
            removed_clipped_samples: 0,
        };
    }

    let medians: [f64; 3] = std::array::from_fn(|channel| {
        let mut values: Vec<u16> = samples.iter().map(|sample| sample[channel]).collect();
        median_u16(&mut values)
    });
    let mut retained: Vec<&[u16; 3]> = samples
        .iter()
        .filter(|sample| {
            !(0..3).any(|channel| {
                (sample[channel] == 0 && medians[channel] > 0.0)
                    || (sample[channel] == u16::MAX && medians[channel] < f64::from(u16::MAX))
            })
        })
        .collect();
    if retained.is_empty() {
        retained.extend(samples.iter());
    }
    let removed_clipped_samples = samples.len() - retained.len();

    let mut value = [0.0; 3];
    let mut used_huber = false;
    for channel in 0..3 {
        let channel_values: Vec<f64> = retained
            .iter()
            .map(|sample| f64::from(sample[channel]))
            .collect();
        let (channel_value, channel_used_huber) = adaptive_channel_mean(&channel_values);
        value[channel] = channel_value;
        used_huber |= channel_used_huber;
    }
    NodeAggregate {
        value,
        used_huber,
        removed_clipped_samples,
    }
}

fn median_u16(values: &mut [u16]) -> f64 {
    values.sort_unstable();
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (f64::from(values[middle - 1]) + f64::from(values[middle])) / 2.0
    } else {
        f64::from(values[middle])
    }
}

fn median_f64(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}

fn adaptive_channel_mean(values: &[f64]) -> (f64, bool) {
    let arithmetic_mean = values.iter().sum::<f64>() / values.len() as f64;
    let mut sorted = values.to_vec();
    let median = median_f64(&mut sorted);
    let mut deviations: Vec<f64> = values.iter().map(|value| (value - median).abs()).collect();
    let mad = median_f64(&mut deviations);
    if mad == 0.0 {
        return (arithmetic_mean, false);
    }

    let robust_sigma = 1.4826 * mad;
    let has_outlier = values
        .iter()
        .any(|value| (value - median).abs() > 3.5 * robust_sigma);
    if !has_outlier {
        return (arithmetic_mean, false);
    }

    let huber_delta = 1.345 * robust_sigma;
    let mut location = median;
    for _ in 0..5 {
        let mut weighted_sum = 0.0;
        let mut weight_sum = 0.0;
        for value in values {
            let residual = (value - location).abs();
            let weight = if residual <= huber_delta {
                1.0
            } else {
                huber_delta / residual
            };
            weighted_sum += value * weight;
            weight_sum += weight;
        }
        let next = weighted_sum / weight_sum;
        if (next - location).abs() < 1.0e-8 {
            location = next;
            break;
        }
        location = next;
    }
    (location, true)
}

fn normalize(pixel: [u16; 3]) -> [f64; 3] {
    [
        pixel[0] as f64 / u16::MAX as f64,
        pixel[1] as f64 / u16::MAX as f64,
        pixel[2] as f64 / u16::MAX as f64,
    ]
}

fn unwrapped_size(width: u32, height: u32) -> Option<usize> {
    let size = height as usize;
    (size >= 2 && width as usize == size * size).then_some(size)
}

fn packed_size(width: u32, height: u32) -> Option<usize> {
    let pixels = u64::from(width) * u64::from(height);
    let size = (pixels as f64).cbrt().round() as u64;
    ((2..=65).contains(&size) && size.pow(3) == pixels).then_some(size as usize)
}

pub(crate) fn is_srgb_profile(profile: &[u8]) -> bool {
    if profile.len() < 128
        || profile.get(16..20) != Some(b"RGB ")
        || profile.get(36..40) != Some(b"acsp")
    {
        return false;
    }

    profile
        .windows(4)
        .any(|window| window.eq_ignore_ascii_case(b"srgb"))
        || profile.windows(8).any(|window| {
            window == [0, b's', 0, b'R', 0, b'G', 0, b'B']
                || window == [b's', 0, b'R', 0, b'G', 0, b'B', 0]
        })
}

#[cfg(test)]
mod tests {
    use std::io::BufWriter;

    use image::codecs::jpeg::JpegEncoder;
    use image::codecs::png::PngEncoder;
    use image::codecs::tiff::TiffEncoder;
    use image::{ExtendedColorType, GenericImageView, ImageBuffer, ImageEncoder, ImageReader, Rgb};

    use super::*;

    #[test]
    fn generates_all_sampling_modes_as_rgb16_tiff_and_png() {
        let directory = tempfile::tempdir().unwrap();
        for (sampling, dimension, samples_per_node) in [
            (NeutralSampling::Single, 512, 1),
            (NeutralSampling::Average16, 2048, 16),
            (NeutralSampling::Average64, 4096, 64),
        ] {
            for format in [NeutralFormat::Tiff, NeutralFormat::Png] {
                let spec = NeutralSpec {
                    format,
                    sampling,
                    ..NeutralSpec::default()
                };
                let path = directory.path().join(spec.default_filename());
                generate_neutral(spec, &path).unwrap();

                let image = ImageReader::open(&path).unwrap().decode().unwrap();
                assert_eq!(image.dimensions(), (dimension, dimension));
                assert!(matches!(image, image::DynamicImage::ImageRgb16(_)));

                let reader = ImageReader::open(&path)
                    .unwrap()
                    .with_guessed_format()
                    .unwrap();
                let mut decoder = reader.into_decoder().unwrap();
                assert_eq!(
                    decoder.icc_profile().unwrap().as_deref(),
                    Some(SRGB_ICC_PROFILE)
                );

                let loaded = load_raster_lut(&path).unwrap();
                assert_eq!(loaded.size, 64);
                assert_eq!(loaded.values.len(), 262_144);
                let statistics = loaded.grain_statistics.unwrap();
                assert_eq!(statistics.sampling, sampling);
                assert_eq!(statistics.samples_per_node, samples_per_node);
                assert_eq!(statistics.input_sample_count, 262_144 * samples_per_node);
                assert_eq!(statistics.averaged_node_count, 262_144);
                assert!(statistics.max_standard_deviation <= f64::EPSILON);
                assert_eq!(statistics.arithmetic_node_count, 262_144);
                assert_eq!(statistics.huber_node_count, 0);
                assert_eq!(statistics.removed_clipped_samples, 0);
                for (index, value) in loaded.values.iter().enumerate() {
                    let red = index % 64;
                    let green = index / 64 % 64;
                    let blue = index / (64 * 64);
                    for (actual, component) in value.iter().zip([red, green, blue]) {
                        let expected = component as f64 / 63.0;
                        assert!((actual - expected).abs() <= 1.0e-5);
                    }
                }

                std::fs::remove_file(path).unwrap();
            }
        }
    }

    #[test]
    fn photo_master_selects_size_tier_and_preserves_photo_content_in_srgb_png() {
        assert_eq!(photo_master_layout(512, 300).unwrap().canvas_size, 512);
        assert_eq!(photo_master_layout(513, 300).unwrap().canvas_size, 2048);
        assert_eq!(photo_master_layout(2048, 1200).unwrap().canvas_size, 2048);
        assert_eq!(photo_master_layout(2049, 1200).unwrap().canvas_size, 4096);

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("PhotoMaster512.png");
        let width = 64_u32;
        let height = 48_u32;
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let value = ((x * 3 + y * 5) % 256) as u8;
                rgba.extend([value, value, value, 255]);
            }
        }
        let source = PhotoSourceImage {
            width,
            height,
            rgba,
        };
        let report = generate_photo_master(source.clone(), NeutralFormat::Png, &path).unwrap();
        let rebuilt = prepare_photo_reference(source.clone()).unwrap();
        assert_eq!(rebuilt.canvas_size, report.canvas_size);
        assert_eq!(rebuilt.photo_rect, report.photo_rect);
        assert_eq!(rebuilt.baseline, report.baseline);
        assert_eq!(report.canvas_size, 512);
        assert_eq!(report.sampling, NeutralSampling::Single);
        assert_eq!(report.photo_rect, (224, 232, width, height));
        assert_eq!(report.calibration_rect, (0, 0, 512, 512));
        assert_eq!(report.baseline.dimensions(), (512, 512));
        assert_eq!(report.baseline.pixels().len(), 512 * 512);
        assert_eq!(report.baseline.pixels()[0], [0, 0, 0]);
        assert_eq!(report.baseline.pixels()[511], [u16::MAX; 3]);
        let left_strip_start = 232 * 512;
        let left_strip_end = 279 * 512;
        let start_gray = scale_to_u16(232, 512);
        let end_gray = scale_to_u16(279, 512);
        assert_eq!(report.baseline.pixels()[left_strip_start], [start_gray; 3]);
        assert_eq!(report.baseline.pixels()[left_strip_end], [end_gray; 3]);

        let portrait_path = directory.path().join("PortraitMaster512.png");
        let portrait = generate_photo_master(
            PhotoSourceImage {
                width: 48,
                height: 64,
                rgba: vec![128; 48 * 64 * 4],
            },
            NeutralFormat::Png,
            &portrait_path,
        )
        .unwrap();
        assert_eq!(portrait.photo_rect, (232, 224, 48, 64));
        assert_eq!(portrait.baseline.pixels()[0], [0, 0, 0]);
        assert_eq!(portrait.baseline.pixels()[511], [u16::MAX; 3]);
        assert_eq!(
            portrait.baseline.pixels()[224 * 512 + 231],
            [scale_to_u16(224, 512); 3]
        );
        assert_eq!(
            portrait.baseline.pixels()[287 * 512 + 231],
            [scale_to_u16(287, 512); 3]
        );

        let reader = ImageReader::open(&path)
            .unwrap()
            .with_guessed_format()
            .unwrap();
        let mut decoder = reader.into_decoder().unwrap();
        assert_eq!(decoder.dimensions(), (1024, 512));
        assert_eq!(
            decoder.icc_profile().unwrap().as_deref(),
            Some(SRGB_ICC_PROFILE)
        );
        assert!(matches!(
            DynamicImage::from_decoder(decoder).unwrap(),
            DynamicImage::ImageRgb16(_)
        ));
        let loaded = load_raster_lut(&path).unwrap();
        assert_eq!(loaded.size, 64);
        assert!(loaded.photo_master.is_some());
        assert_eq!(loaded.grain_statistics.unwrap().samples_per_node, 1);
        assert_eq!(loaded.values[0], [0.0, 0.0, 0.0]);
        assert_eq!(loaded.values[63], [1.0, 0.0, 0.0]);

        let tiff_path = directory.path().join("PhotoMaster512.tif");
        generate_photo_master(source, NeutralFormat::Tiff, &tiff_path).unwrap();
        let mut tiff_decoder = ImageReader::open(tiff_path)
            .unwrap()
            .with_guessed_format()
            .unwrap()
            .into_decoder()
            .unwrap();
        assert_eq!(tiff_decoder.dimensions(), (1024, 512));
        assert_eq!(
            tiff_decoder.icc_profile().unwrap().as_deref(),
            Some(SRGB_ICC_PROFILE)
        );
    }

    #[test]
    fn accepts_rgb8_png_and_jpeg_photo_master_layouts() {
        let directory = tempfile::tempdir().unwrap();
        let rgb16_master = directory.path().join("source-master.png");
        let mut rgba = Vec::new();
        for y in 0..32_u32 {
            for x in 0..48_u32 {
                rgba.extend([((x * 5) % 256) as u8, ((y * 7) % 256) as u8, 90, 255]);
            }
        }
        generate_photo_master(
            PhotoSourceImage {
                width: 48,
                height: 32,
                rgba,
            },
            NeutralFormat::Png,
            &rgb16_master,
        )
        .unwrap();
        let rgb8 = ImageReader::open(&rgb16_master)
            .unwrap()
            .decode()
            .unwrap()
            .to_rgb8();

        let rgb8_png = directory.path().join("rgb8-master.png");
        let mut png = PngEncoder::new(BufWriter::new(File::create(&rgb8_png).unwrap()));
        png.set_icc_profile(SRGB_ICC_PROFILE.to_vec()).unwrap();
        png.write_image(&rgb8, rgb8.width(), rgb8.height(), ExtendedColorType::Rgb8)
            .unwrap();
        let loaded_png = load_raster_lut(&rgb8_png).unwrap();
        assert_eq!(loaded_png.size, 64);
        assert!(loaded_png.photo_master.is_some());
        assert_eq!(loaded_png.quality.bit_depth, 8);
        assert!(!loaded_png.quality.lossy_jpeg);

        let jpeg_path = directory.path().join("jpeg-master.jpg");
        JpegEncoder::new_with_quality(BufWriter::new(File::create(&jpeg_path).unwrap()), 100)
            .write_image(&rgb8, rgb8.width(), rgb8.height(), ExtendedColorType::Rgb8)
            .unwrap();
        let loaded_jpeg = load_raster_lut(&jpeg_path).unwrap();
        assert_eq!(loaded_jpeg.size, 64);
        assert!(loaded_jpeg.photo_master.is_some());
        assert_eq!(loaded_jpeg.quality.bit_depth, 8);
        assert!(loaded_jpeg.quality.lossy_jpeg);
        assert!(loaded_jpeg.quality.assumed_srgb);
    }

    #[test]
    fn anti_grain_generation_tiles_the_complete_base_image() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Neutral64_Grain16.png");
        generate_neutral(
            NeutralSpec {
                sampling: NeutralSampling::Average16,
                format: NeutralFormat::Png,
                ..NeutralSpec::default()
            },
            &path,
        )
        .unwrap();
        let rgb = ImageReader::open(path)
            .unwrap()
            .decode()
            .unwrap()
            .to_rgb16();
        for (x, y) in [(0, 0), (1, 0), (511, 0), (17, 233), (511, 511)] {
            let expected = rgb.get_pixel(x, y);
            for tile_y in 0..4 {
                for tile_x in 0..4 {
                    assert_eq!(rgb.get_pixel(tile_x * 512 + x, tile_y * 512 + y), expected);
                }
            }
        }
        assert_ne!(rgb.get_pixel(0, 0), rgb.get_pixel(1, 0));
    }

    #[test]
    fn repeated_sampling_averages_zero_mean_noise_but_keeps_bias_and_precision() {
        let mut image = ImageBuffer::from_pixel(2048, 2048, Rgb([0_u16; 3]));
        let deviations = [-8_i32, -7, -6, -5, -4, -3, -2, -1, 1, 2, 3, 4, 5, 6, 7, 8];
        for (sample, deviation) in deviations.into_iter().enumerate() {
            let tile_x = sample as u32 % 4;
            let tile_y = sample as u32 / 4;
            image.put_pixel(
                tile_x * 512,
                tile_y * 512,
                Rgb([(1000_i32 + deviation) as u16, 1008, 1001]),
            );
            image.put_pixel(tile_x * 512 + 1, tile_y * 512, Rgb([1001, 1008, 1002]));
            let robust_value = if sample == 15 {
                20_000
            } else {
                1000 + [-2_i32, -1, 0, 1, 2][sample % 5]
            };
            image.put_pixel(
                tile_x * 512 + 2,
                tile_y * 512,
                Rgb([robust_value as u16, 2000, 3000]),
            );
            let clipped_value = match sample {
                0 => 0,
                1 => u16::MAX,
                _ => 1000,
            };
            image.put_pixel(
                tile_x * 512 + 3,
                tile_y * 512,
                Rgb([clipped_value, 2000, 3000]),
            );
        }

        let (values, statistics, _) = average_repeated_neutral(
            &image,
            NeutralSampling::Average16,
            RasterQuality {
                bit_depth: 16,
                lossy_jpeg: false,
                assumed_srgb: false,
            },
        );
        assert!((values[0][0] - 1000.0 / 65_535.0).abs() < 1.0 / 65_535.0);
        assert_eq!(values[0][1], 1008.0 / 65_535.0);
        assert!((values[1][0] - values[0][0] - 1.0 / 65_535.0).abs() < f64::EPSILON);
        assert_eq!(statistics.samples_per_node, 16);
        assert!(statistics.max_standard_deviation > 0.0);
        assert_eq!(statistics.huber_node_count, 1);
        assert_eq!(statistics.clipped_node_count, 1);
        assert_eq!(statistics.removed_clipped_samples, 2);
    }

    #[test]
    fn adaptive_aggregation_uses_huber_only_for_mad_detected_outliers() {
        let mut samples = Vec::new();
        for offset in [-2_i32, -1, 0, 1, 2].into_iter().cycle().take(15) {
            samples.push([(1000 + offset) as u16, 2000, 3000]);
        }
        samples.push([20_000, 2000, 3000]);
        let robust = aggregate_node_samples(&samples);
        let arithmetic = samples
            .iter()
            .map(|sample| f64::from(sample[0]))
            .sum::<f64>()
            / 16.0;
        assert!(robust.used_huber);
        assert!((robust.value[0] - 1000.0).abs() < 2.0);
        assert!(arithmetic - robust.value[0] > 1000.0);

        let mut zero_mad = vec![[1000, 2000, 3000]; 16];
        zero_mad[15][0] = 20_000;
        let fallback = aggregate_node_samples(&zero_mad);
        assert!(!fallback.used_huber);
        assert_eq!(fallback.value[0], (15.0 * 1000.0 + 20_000.0) / 16.0);
    }

    #[test]
    fn clipped_samples_are_removed_without_damaging_true_boundary_nodes() {
        let mut samples = vec![[1000, 2000, 3000]; 16];
        samples[0][0] = 0;
        samples[1][0] = u16::MAX;
        let clipped = aggregate_node_samples(&samples);
        assert_eq!(clipped.removed_clipped_samples, 2);
        assert_eq!(clipped.value, [1000.0, 2000.0, 3000.0]);

        let boundary: Vec<[u16; 3]> = (0..16).map(|offset| [0, 2000 + offset, u16::MAX]).collect();
        let retained = aggregate_node_samples(&boundary);
        assert_eq!(retained.removed_clipped_samples, 0);
        assert_eq!(retained.value[0], 0.0);
        assert_eq!(retained.value[2], f64::from(u16::MAX));
    }

    #[test]
    fn preserves_single_code_value_differences() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("precise.tif");
        let mut samples = vec![0_u16; 1089 * 33 * 3];
        samples[3] = 1;
        write_test_tiff(&path, &samples, ExtendedColorType::Rgb16);

        let loaded = load_raster_lut(&path).unwrap();
        assert_eq!(loaded.size, 33);
        assert_eq!(loaded.values[0][0], 0.0);
        assert!((loaded.values[1][0] - 1.0 / 65_535.0).abs() < f64::EPSILON);
    }

    #[test]
    fn accepts_eight_bit_rgb_but_rejects_alpha_and_grayscale() {
        let directory = tempfile::tempdir().unwrap();
        let eight_bit = directory.path().join("eight.tif");
        let mut samples = vec![0_u16; 1089 * 33 * 3];
        samples[3] = 1;
        write_test_tiff(&eight_bit, &samples, ExtendedColorType::Rgb8);
        let loaded = load_raster_lut(&eight_bit).unwrap();
        assert_eq!(loaded.quality.bit_depth, 8);
        assert!(!loaded.quality.lossy_jpeg);
        assert_eq!(loaded.values[1][0], 1.0 / 255.0);
        assert!(loaded.node_confidence.unwrap()[0].quantization_limited);

        let alpha = directory.path().join("alpha.tif");
        write_test_tiff(&alpha, &[0_u16; 1089 * 33 * 4], ExtendedColorType::Rgba16);
        assert!(load_raster_lut(&alpha).is_err());

        let grayscale = directory.path().join("grayscale.tif");
        write_test_tiff(&grayscale, &[0_u16; 1089 * 33], ExtendedColorType::L16);
        assert!(load_raster_lut(&grayscale).is_err());
    }

    #[test]
    fn accepts_rgb8_jpeg_without_icc_as_assumed_srgb() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("neutral.jpg");
        let file = File::create(&path).unwrap();
        JpegEncoder::new_with_quality(BufWriter::new(file), 95)
            .write_image(
                &vec![128_u8; 512 * 512 * 3],
                512,
                512,
                ExtendedColorType::Rgb8,
            )
            .unwrap();

        let loaded = load_raster_lut(&path).unwrap();
        assert_eq!(loaded.size, 64);
        assert_eq!(loaded.quality.bit_depth, 8);
        assert!(loaded.quality.lossy_jpeg);
        assert!(loaded.quality.assumed_srgb);
        let confidence = loaded.node_confidence.unwrap();
        assert!(confidence[0].quantization_limited);
        assert!(confidence[0].lossy_compression);
    }

    #[test]
    fn rejects_mismatched_extensions_missing_profiles_and_corrupt_images() {
        let directory = tempfile::tempdir().unwrap();
        assert!(
            generate_neutral(NeutralSpec::default(), directory.path().join("wrong.tif")).is_err()
        );

        let missing_profile = directory.path().join("missing-profile.tif");
        let file = File::create(&missing_profile).unwrap();
        let encoder = TiffEncoder::new(BufWriter::new(file));
        encoder
            .write_image(&[0_u8; 4 * 2 * 3 * 2], 4, 2, ExtendedColorType::Rgb16)
            .unwrap();
        assert!(load_raster_lut(&missing_profile).is_err());

        let corrupt = directory.path().join("corrupt.tif");
        std::fs::write(&corrupt, b"not a TIFF image").unwrap();
        assert!(load_raster_lut(&corrupt).is_err());
    }

    #[test]
    fn recognizes_only_rgb_srgb_profiles() {
        assert!(is_srgb_profile(SRGB_ICC_PROFILE));
        let mut changed = SRGB_ICC_PROFILE.to_vec();
        for byte in &mut changed {
            if *byte == b's' || *byte == b'S' {
                *byte = b'x';
            }
        }
        assert!(!is_srgb_profile(&changed));
    }

    #[test]
    fn recognizes_only_exact_supported_repeated_dimensions() {
        assert_eq!(NeutralSampling::Single.default_format(), NeutralFormat::Png);
        assert_eq!(
            NeutralSampling::Average16.default_format(),
            NeutralFormat::Png
        );
        assert_eq!(
            NeutralSampling::Average64.default_format(),
            NeutralFormat::Png
        );
        assert_eq!(repeated_sampling(512, 512), None);
        assert_eq!(
            repeated_sampling(2048, 2048),
            Some(NeutralSampling::Average16)
        );
        assert_eq!(
            repeated_sampling(4096, 4096),
            Some(NeutralSampling::Average64)
        );
        assert_eq!(repeated_sampling(2048, 2047), None);
        assert_eq!(repeated_sampling(4095, 4096), None);
    }

    fn write_test_tiff(path: &Path, samples: &[u16], color: ExtendedColorType) {
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        match color {
            ExtendedColorType::Rgb8 => {
                bytes.extend(samples.iter().map(|value| *value as u8));
            }
            _ => {
                for sample in samples {
                    bytes.extend_from_slice(&sample.to_ne_bytes());
                }
            }
        }
        let file = File::create(path).unwrap();
        let mut encoder = TiffEncoder::new(BufWriter::new(file));
        encoder.set_icc_profile(SRGB_ICC_PROFILE.to_vec()).unwrap();
        encoder.write_image(&bytes, 1089, 33, color).unwrap();
    }
}
