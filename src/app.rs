use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;

use eframe::egui::{
    self, Align, Align2, Color32, FontData, FontDefinitions, FontFamily, FontId, Frame, Layout,
    Pos2, Rect, Response, RichText, Stroke, StrokeKind, TextStyle, TextureHandle, TextureId, Vec2,
};
use lumix_lut::{
    ConsistencyMode, ConsistencyReport, ConversionReport, GrainStatistics, NeutralFormat,
    NeutralSampling, NeutralSpec, OUTPUT_GRID_PRESETS, PhotoMasterReport, PhotoMasterSession,
    PhotoReferenceBuffer, PhotoReferenceReport, PhotoSourceImage, PreparedLut, SmoothingMode,
    SmoothingReport, export_prepared_cube, generate_neutral, generate_photo_master,
    photo_master_layout, prepare_lut_with_photo_master, prepare_photo_reference,
};

use crate::platform::{FILE_MANAGER_LABEL, reveal_file};
use crate::preview::{
    PreviewBitmap, apply_lut, decode_photo_for_master, decode_preview_photo, is_heif_photo,
    is_supported_photo,
};

const BG: Color32 = Color32::from_rgb(11, 11, 13);
const TEXT: Color32 = Color32::from_rgb(236, 237, 240);
const MUTED: Color32 = Color32::from_rgb(139, 143, 152);
const FAINT: Color32 = Color32::from_rgb(153, 157, 166);
const ACCENT: Color32 = Color32::from_rgb(232, 160, 76);
const PRIMARY: Color32 = Color32::from_rgb(233, 233, 236);
const PRIMARY_TEXT: Color32 = Color32::from_rgb(16, 16, 18);
const SUCCESS: Color32 = Color32::from_rgb(98, 200, 140);
const ERROR: Color32 = Color32::from_rgb(230, 100, 100);
const WARNING: Color32 = Color32::from_rgb(230, 180, 90);
const SURFACE: Color32 = Color32::from_rgb(18, 18, 21);
const SURFACE_RAISED: Color32 = Color32::from_rgb(22, 22, 26);

// Shared layout scale. Keep the app's visual rhythm independent from any
// individual control so cards, columns, and the window edge stay aligned.
const PAGE_PADDING: i8 = 24;
const FORM_GAP: f32 = 18.0;
const PREVIEW_GAP: f32 = 14.0;
const COLUMN_GAP: f32 = 16.0;
const CONTROL_GAP: f32 = 8.0;
const COMPACT_GAP: f32 = 4.0;
const MICRO_GAP: f32 = 2.0;
const CARD_PADDING_X: i8 = 16;
const CARD_PADDING_Y: i8 = 14;
const CARD_CORNER_RADIUS: u8 = 12;
const LEFT_COLUMN_WIDTH: f32 = 358.0;
const SAFETY_BUTTON_HEIGHT: f32 = 50.0;

fn tint(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn unpremultiply_rgba(rgba: &mut [u8]) {
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        if alpha == 0 {
            pixel[..3].fill(0);
        } else if alpha < 255 {
            for channel in &mut pixel[..3] {
                *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn unpremultiply_rgba(_rgba: &mut [u8]) {}

fn add_vertical_gap(ui: &mut egui::Ui, total_gap: f32) {
    let automatic_gap = ui.spacing().item_spacing.y;
    ui.add_space((total_gap - automatic_gap).max(0.0));
}

fn add_horizontal_gap(ui: &mut egui::Ui, total_gap: f32) {
    let automatic_gap = ui.spacing().item_spacing.x;
    ui.add_space((total_gap - automatic_gap).max(0.0));
}

struct BuiltInPreviewPhoto {
    label: &'static str,
    cache_name: &'static str,
    bytes: &'static [u8],
}

const BUILT_IN_PREVIEW_PHOTOS: [BuiltInPreviewPhoto; 2] = [
    BuiltInPreviewPhoto {
        label: "样片",
        cache_name: "img-0779-preview-8942909f.jpg",
        bytes: include_bytes!("../assets/default-preview/IMG_0779-preview.jpg"),
    },
    BuiltInPreviewPhoto {
        label: "色彩测试",
        cache_name: "lut-color-test-37b48ccd.png",
        bytes: include_bytes!("../LUT_Color_Test_Chart.png"),
    },
];

pub struct LumixApp {
    input_path: Option<PathBuf>,
    neutral_format: NeutralFormat,
    neutral_sampling: NeutralSampling,
    selected_source_path: Option<PathBuf>,
    master_source_path: Option<PathBuf>,
    reference_uses_builtin_sample: bool,
    master_baseline: Option<Arc<PhotoReferenceBuffer>>,
    master_calibration_rect: Option<(u32, u32, u32, u32)>,
    master_canvas_size: Option<u32>,
    generated_master_path: Option<PathBuf>,
    generated_master_modified: Option<std::time::SystemTime>,
    master_source_texture: Option<TextureHandle>,
    pending_inconsistency: Option<PendingInconsistency>,
    confirmed_suspect_input: Option<PathBuf>,
    target_grid: usize,
    smoothing_mode: SmoothingMode,
    automatic_smoothing_selection: bool,
    status: AppStatus,
    photo_path: Option<PathBuf>,
    preview_source: PreviewSource,
    custom_photo_path: Option<PathBuf>,
    preview_original: Option<Arc<PreviewBitmap>>,
    preview_original_texture: Option<TextureHandle>,
    preview_effect_texture: Option<TextureHandle>,
    preview_status: PreviewStatus,
    prepared_lut: Option<Arc<PreparedLut>>,
    preview_mode: PreviewMode,
    show_effect: bool,
    split_position: f32,
    zoom: f32,
    pan: Vec2,
    preview_generation: Arc<AtomicU64>,
    preview_sender: mpsc::Sender<PreviewResult>,
    preview_receiver: mpsc::Receiver<PreviewResult>,
    operation_sender: mpsc::Sender<OperationResult>,
    operation_receiver: mpsc::Receiver<OperationResult>,
    operation_busy: bool,
    lut_drop_rect: Option<Rect>,
    photo_drop_rect: Option<Rect>,
    show_safety_guide: bool,
    show_license_guide: bool,
}

enum AppStatus {
    Ready,
    Working(&'static str),
    Success {
        headline: String,
        details: Vec<String>,
        path: PathBuf,
    },
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewMode {
    Split,
    Toggle,
    Dual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewSource {
    BuiltIn(usize),
    Reference,
    Custom,
}

struct PendingInconsistency {
    input_path: PathBuf,
    report: ConsistencyReport,
}

enum PreviewStatus {
    Idle,
    Processing,
    Ready {
        source_grid: Option<usize>,
        target_grid: Option<usize>,
        clipped_points: usize,
        warnings: Vec<String>,
        grain_statistics: Option<Box<GrainStatistics>>,
        smoothing: SmoothingReport,
        consistency: Option<Box<ConsistencyReport>>,
    },
    Error(String),
}

struct PreviewResult {
    generation: u64,
    photo_path: PathBuf,
    original: Option<Arc<PreviewBitmap>>,
    prepared: Option<Arc<PreparedLut>>,
    effect: Option<PreviewBitmap>,
    source_grid: Option<usize>,
    target_grid: Option<usize>,
    clipped_points: usize,
    warnings: Vec<String>,
    grain_statistics: Option<GrainStatistics>,
    smoothing: SmoothingReport,
    consistency: Option<ConsistencyReport>,
    error: Option<String>,
}

enum OperationResult {
    Neutral(Result<(PathBuf, NeutralSpec), String>),
    Reference {
        source_path: PathBuf,
        result: Result<(PhotoReferenceReport, Option<PreviewBitmap>), String>,
    },
    Master {
        source_path: PathBuf,
        result: Result<(PhotoMasterReport, Option<PreviewBitmap>), String>,
    },
    Export(Box<Result<ConversionReport, String>>),
}

impl LumixApp {
    pub fn new(context: &eframe::CreationContext<'_>) -> Self {
        configure_fonts(&context.egui_ctx);
        configure_style(&context.egui_ctx);
        let (preview_sender, preview_receiver) = mpsc::channel();
        let (operation_sender, operation_receiver) = mpsc::channel();
        let mut app = Self {
            input_path: None,
            neutral_format: NeutralFormat::Png,
            neutral_sampling: NeutralSampling::Single,
            selected_source_path: None,
            master_source_path: None,
            reference_uses_builtin_sample: false,
            master_baseline: None,
            master_calibration_rect: None,
            master_canvas_size: None,
            generated_master_path: None,
            generated_master_modified: None,
            master_source_texture: None,
            pending_inconsistency: None,
            confirmed_suspect_input: None,
            target_grid: 33,
            smoothing_mode: SmoothingMode::Off,
            automatic_smoothing_selection: true,
            status: AppStatus::Ready,
            photo_path: None,
            preview_source: PreviewSource::BuiltIn(0),
            custom_photo_path: None,
            preview_original: None,
            preview_original_texture: None,
            preview_effect_texture: None,
            preview_status: PreviewStatus::Idle,
            prepared_lut: None,
            preview_mode: PreviewMode::Split,
            show_effect: true,
            split_position: 0.5,
            zoom: 1.0,
            pan: Vec2::ZERO,
            preview_generation: Arc::new(AtomicU64::new(0)),
            preview_sender,
            preview_receiver,
            operation_sender,
            operation_receiver,
            operation_busy: false,
            lut_drop_rect: None,
            photo_drop_rect: None,
            show_safety_guide: false,
            show_license_guide: false,
        };
        app.set_builtin_photo(0, &context.egui_ctx);
        app
    }

    fn handle_dropped_files(&mut self, context: &egui::Context) {
        let dropped_files = context.input(|input| input.raw.dropped_files.clone());
        let pointer = context.input(|input| input.pointer.hover_pos());
        for path in dropped_files.into_iter().filter_map(|file| file.path) {
            let over_lut = pointer
                .is_some_and(|point| self.lut_drop_rect.is_some_and(|rect| rect.contains(point)));
            let over_photo = pointer.is_some_and(|point| {
                self.photo_drop_rect
                    .is_some_and(|rect| rect.contains(point))
            });
            if over_lut {
                if is_supported_input(&path) {
                    self.set_input(path, context);
                } else {
                    self.status = AppStatus::Error(
                        "LUT 区支持 RGB16/RGB8 sRGB TIFF/PNG/JPEG、HALD 图像或纯 3D .cube。"
                            .to_owned(),
                    );
                }
            } else if over_photo {
                if is_supported_photo(&path) {
                    self.set_photo(path, context);
                } else {
                    self.preview_status = PreviewStatus::Error(
                        "照片区支持 HEIC/HEIF、JPEG、PNG 和 TIFF。".to_owned(),
                    );
                }
            } else if is_cube(&path) {
                self.set_input(path, context);
            } else if is_heif_photo(&path) || is_jpeg(&path) {
                self.set_photo(path, context);
            } else if is_png_or_tiff(&path) {
                self.preview_status = PreviewStatus::Error(
                    "PNG/TIFF 既可能是 LUT 中性图，也可能是预览照片；请拖到对应区域。".to_owned(),
                );
            } else {
                self.preview_status = PreviewStatus::Error("不支持该文件格式。".to_owned());
            }
        }
    }

    fn generate_neutral(&mut self, context: &egui::Context) {
        let extension = self.neutral_format.extension();
        let filter_name = format!("16-bit {}", self.neutral_format.display_name());
        let spec = NeutralSpec {
            format: self.neutral_format,
            sampling: self.neutral_sampling,
            ..NeutralSpec::default()
        };
        let filename = spec.default_filename();
        let path = rfd::FileDialog::new()
            .set_title("保存 64-grid 中性图")
            .set_file_name(&filename)
            .add_filter(&filter_name, &[extension])
            .save_file();
        let Some(path) = path else {
            return;
        };
        let path = ensure_extension(path, extension);
        self.operation_busy = true;
        self.status = AppStatus::Working("正在生成中性图…");
        let sender = self.operation_sender.clone();
        let repaint = context.clone();
        thread::spawn(move || {
            let result = generate_neutral(spec, &path)
                .map(|output| (output, spec))
                .map_err(|error| error.to_string());
            let _ = sender.send(OperationResult::Neutral(result));
            repaint.request_repaint();
        });
    }

    fn choose_master_source(&mut self, context: &egui::Context) {
        if let Some(source_path) = rfd::FileDialog::new()
            .set_title("选择参考照片")
            .add_filter(
                "照片",
                &["heic", "heif", "jpg", "jpeg", "png", "tif", "tiff"],
            )
            .pick_file()
        {
            self.begin_master_reference(source_path, false, context);
        }
    }

    fn begin_master_reference(
        &mut self,
        source_path: PathBuf,
        is_builtin_sample: bool,
        context: &egui::Context,
    ) {
        let reference_changed = self.master_source_path.as_ref() != Some(&source_path);
        if reference_changed {
            if self
                .prepared_lut
                .as_ref()
                .is_some_and(|prepared| prepared.consistency_report().is_some())
            {
                self.input_path = None;
            }
            self.prepared_lut = None;
            self.preview_effect_texture = None;
            self.master_source_path = None;
            self.master_baseline = None;
            self.master_calibration_rect = None;
            self.master_canvas_size = None;
            self.master_source_texture = None;
            self.generated_master_path = None;
            self.generated_master_modified = None;
        }
        self.selected_source_path = Some(source_path.clone());
        self.reference_uses_builtin_sample = is_builtin_sample;
        self.preview_source = PreviewSource::Reference;
        self.load_photo_path(source_path.clone(), context);
        self.pending_inconsistency = None;
        self.confirmed_suspect_input = None;
        self.operation_busy = true;
        self.status = AppStatus::Working("正在读取参考图…");
        let sender = self.operation_sender.clone();
        let repaint = context.clone();
        thread::spawn(move || {
            let result = decode_photo_for_master(&source_path)
                .map_err(|error| format!("无法读取参考图：{error}"))
                .and_then(|bitmap| {
                    let thumbnail = thumbnail_preview(&bitmap, 360);
                    prepare_photo_reference(photo_source_image(bitmap))
                        .map(|reference| (reference, thumbnail))
                        .map_err(|error| error.to_string())
                });
            let _ = sender.send(OperationResult::Reference {
                source_path,
                result,
            });
            repaint.request_repaint();
        });
    }

    fn generate_photo_master(&mut self, context: &egui::Context) {
        let using_builtin_sample = self.selected_source_path.is_none();
        let source_path = if let Some(source_path) = self.selected_source_path.clone() {
            source_path
        } else {
            match materialize_builtin_preview_photo(0) {
                Ok(source_path) => source_path,
                Err(error) => {
                    self.status = AppStatus::Error(error);
                    return;
                }
            }
        };
        let size = if let Some(size) = self.master_canvas_size {
            size
        } else if using_builtin_sample || self.reference_uses_builtin_sample {
            match builtin_photo_canvas_size(0) {
                Ok(size) => size,
                Err(error) => {
                    self.status = AppStatus::Error(error);
                    return;
                }
            }
        } else {
            self.status = AppStatus::Error("请等待参考图读取完成。".to_owned());
            return;
        };
        if using_builtin_sample {
            self.selected_source_path = Some(source_path.clone());
            self.reference_uses_builtin_sample = true;
            self.preview_source = PreviewSource::Reference;
            self.load_photo_path(source_path.clone(), context);
        }
        let extension = self.neutral_format.extension();
        let default_name = format!("PhotoMaster{}.{}", size, extension);
        let Some(path) = rfd::FileDialog::new()
            .set_title("保存调色母版")
            .set_file_name(default_name)
            .add_filter(self.neutral_format.display_name(), &[extension])
            .save_file()
        else {
            return;
        };
        let path = ensure_extension(path, extension);
        self.operation_busy = true;
        self.status = AppStatus::Working("正在合成照片校准母版…");
        let format = self.neutral_format;
        let sender = self.operation_sender.clone();
        let repaint = context.clone();
        thread::spawn(move || {
            let result = decode_photo_for_master(&source_path)
                .map_err(|error| format!("无法读取参考图：{error}"))
                .and_then(|bitmap| {
                    let thumbnail = thumbnail_preview(&bitmap, 360);
                    generate_photo_master(photo_source_image(bitmap), format, &path)
                        .map(|report| (report, thumbnail))
                        .map_err(|error| error.to_string())
                });
            let _ = sender.send(OperationResult::Master {
                source_path,
                result,
            });
            repaint.request_repaint();
        });
    }

    fn choose_input(&mut self, context: &egui::Context) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("选择调色后的母版、中性图或 CUBE")
            .add_filter("LUT 输入", &["tif", "tiff", "png", "jpg", "jpeg", "cube"])
            .pick_file()
        {
            self.set_input(path, context);
        }
    }

    fn set_input(&mut self, path: PathBuf, context: &egui::Context) {
        if self.generated_master_path.as_ref() == Some(&path)
            && self
                .generated_master_modified
                .is_some_and(|generated_time| {
                    std::fs::metadata(&path)
                        .and_then(|file| file.modified())
                        .is_ok_and(|current_time| current_time == generated_time)
                })
        {
            self.status = AppStatus::Error(
                "这是尚未调色的母版。请先在调色软件中编辑并另存，再导入另存的文件。".to_owned(),
            );
            return;
        }
        self.input_path = Some(path);
        self.prepared_lut = None;
        self.pending_inconsistency = None;
        self.confirmed_suspect_input = None;
        self.smoothing_mode = SmoothingMode::Off;
        self.automatic_smoothing_selection = true;
        self.status = AppStatus::Ready;
        self.schedule_preview(context, false);
    }

    fn receive_operation_results(&mut self, context: &egui::Context) {
        while let Ok(result) = self.operation_receiver.try_recv() {
            self.operation_busy = false;
            match result {
                OperationResult::Neutral(Ok((path, spec))) => {
                    let (width, height) = spec.output_dimensions();
                    self.status = AppStatus::Success {
                        headline: "中性图已保存".to_owned(),
                        details: vec![
                            format!(
                                "{width}×{height} · 16 位 RGB · sRGB · 每个颜色采样 {} 次",
                                spec.sampling.samples_per_node()
                            ),
                            "调色并另存后，再把新文件导入第 2 步。".to_owned(),
                        ],
                        path,
                    };
                }
                OperationResult::Reference {
                    source_path,
                    result,
                } => {
                    if self.selected_source_path.as_ref() != Some(&source_path) {
                        continue;
                    }
                    match result {
                        Ok((reference, thumbnail)) => {
                            self.master_source_path = Some(source_path.clone());
                            self.master_baseline = Some(Arc::new(reference.baseline));
                            self.master_calibration_rect = Some(reference.calibration_rect);
                            self.master_canvas_size = Some(reference.canvas_size);
                            self.master_source_texture = thumbnail.as_ref().map(|bitmap| {
                                load_bitmap_texture(context, "calibration-source", bitmap)
                            });
                            self.status = AppStatus::Ready;
                            if self.preview_source == PreviewSource::Reference
                                && self.input_path.is_some()
                            {
                                self.schedule_preview(context, false);
                            }
                        }
                        Err(error) => self.status = AppStatus::Error(error),
                    }
                }
                OperationResult::Master {
                    source_path,
                    result,
                } => {
                    if self.selected_source_path.as_ref() != Some(&source_path) {
                        continue;
                    }
                    match result {
                        Ok((report, thumbnail)) => {
                            let reference_name = if self.reference_uses_builtin_sample {
                                "内置样片".to_owned()
                            } else {
                                source_path
                                    .file_name()
                                    .and_then(|name| name.to_str())
                                    .unwrap_or("参考照片")
                                    .to_owned()
                            };
                            self.input_path = None;
                            self.prepared_lut = None;
                            self.preview_effect_texture = None;
                            self.master_source_path = Some(source_path.clone());
                            self.master_baseline = Some(Arc::new(report.baseline));
                            self.master_calibration_rect = Some(report.calibration_rect);
                            self.master_canvas_size = Some(report.canvas_size);
                            self.generated_master_path = Some(report.output_path.clone());
                            self.generated_master_modified = std::fs::metadata(&report.output_path)
                                .and_then(|file| file.modified())
                                .ok();
                            self.master_source_texture = thumbnail.as_ref().map(|bitmap| {
                                load_bitmap_texture(context, "calibration-source", bitmap)
                            });
                            self.schedule_preview(context, false);
                            self.status = AppStatus::Success {
                                headline: "调色母版已保存".to_owned(),
                                details: vec![
                                    format!("参考照片：{reference_name}"),
                                    format!(
                                        "{}×{} · 16 位 RGB · sRGB · 每个颜色采样 {} 次",
                                        report.canvas_size * 2,
                                        report.canvas_size,
                                        report.sampling.samples_per_node()
                                    ),
                                    "请在调色软件中编辑整张母版，另存后导入第 2 步。请保持画布大小和位置不变。".to_owned(),
                                ],
                                path: report.output_path,
                            };
                        }
                        Err(error) => self.status = AppStatus::Error(error),
                    }
                }
                OperationResult::Export(result) => match *result {
                    Ok(report) => self.show_conversion_success(report),
                    Err(error) => self.status = AppStatus::Error(error),
                },
                OperationResult::Neutral(Err(error)) => self.status = AppStatus::Error(error),
            }
        }
    }

    fn choose_photo(&mut self, context: &egui::Context) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("选择预览照片")
            .add_filter(
                "预览照片",
                &["heic", "heif", "jpg", "jpeg", "png", "tif", "tiff"],
            )
            .pick_file()
        {
            self.set_photo(path, context);
        }
    }

    fn set_photo(&mut self, path: PathBuf, context: &egui::Context) {
        self.preview_source = PreviewSource::Custom;
        self.custom_photo_path = Some(path.clone());
        self.load_photo_path(path, context);
    }

    fn set_reference_photo(&mut self, context: &egui::Context) {
        if let Some(path) = self.selected_source_path.clone() {
            if self.preview_source != PreviewSource::Reference {
                self.preview_source = PreviewSource::Reference;
                self.load_photo_path(path, context);
            }
        } else {
            match materialize_builtin_preview_photo(0) {
                Ok(path) => self.begin_master_reference(path, true, context),
                Err(error) => self.status = AppStatus::Error(error),
            }
        }
    }

    fn set_builtin_photo(&mut self, index: usize, context: &egui::Context) {
        match materialize_builtin_preview_photo(index) {
            Ok(path) => {
                self.preview_source = PreviewSource::BuiltIn(index);
                self.load_photo_path(path, context);
            }
            Err(error) => {
                self.preview_status =
                    PreviewStatus::Error(format!("无法准备内置预览照片：{error}"));
            }
        }
    }

    fn load_photo_path(&mut self, path: PathBuf, context: &egui::Context) {
        self.photo_path = Some(path);
        self.preview_original = None;
        self.preview_original_texture = None;
        self.preview_effect_texture = None;
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
        self.schedule_preview(context, true);
    }

    fn schedule_preview(&mut self, context: &egui::Context, force_decode: bool) {
        let photo_path = self.photo_path.clone();
        let generation = self.preview_generation.fetch_add(1, Ordering::AcqRel) + 1;
        let token = Arc::clone(&self.preview_generation);
        let sender = self.preview_sender.clone();
        let repaint = context.clone();
        let input_path = self.input_path.clone();
        let master_baseline = self.master_baseline.clone();
        let master_calibration_rect = self.master_calibration_rect;
        let target_grid = self.target_grid;
        let smoothing_mode = if self.automatic_smoothing_selection {
            SmoothingMode::Auto
        } else {
            self.smoothing_mode
        };
        let cached_original = if force_decode {
            None
        } else {
            self.preview_original.clone()
        };
        self.preview_effect_texture = None;
        self.prepared_lut = None;
        self.preview_status = PreviewStatus::Processing;

        thread::spawn(move || {
            let master_session = master_baseline.as_deref().zip(master_calibration_rect).map(
                |(baseline, calibration_rect)| PhotoMasterSession {
                    baseline,
                    calibration_rect,
                },
            );
            let prepared_result = input_path.as_ref().map(|path| {
                prepare_lut_with_photo_master(
                    path,
                    target_grid,
                    smoothing_mode,
                    master_session,
                    ConsistencyMode::Comprehensive,
                )
                .map(Arc::new)
            });
            if token.load(Ordering::Acquire) != generation {
                return;
            }
            let Some(photo_path) = photo_path else {
                let (prepared, error) = match prepared_result {
                    Some(Ok(prepared)) => (Some(prepared), None),
                    Some(Err(error)) => (None, Some(error.to_string())),
                    None => (None, None),
                };
                let _ = sender.send(PreviewResult {
                    generation,
                    photo_path: PathBuf::new(),
                    original: None,
                    prepared,
                    effect: None,
                    source_grid: None,
                    target_grid: None,
                    clipped_points: 0,
                    warnings: Vec::new(),
                    grain_statistics: None,
                    smoothing: SmoothingReport {
                        mode: SmoothingMode::Off,
                        corrected_node_count: 0,
                        maximum_offset: 0.0,
                    },
                    consistency: None,
                    error,
                });
                repaint.request_repaint();
                return;
            };
            let original = match cached_original {
                Some(bitmap) => bitmap,
                None => match decode_preview_photo(&photo_path) {
                    Ok(bitmap) => Arc::new(bitmap),
                    Err(error) => {
                        let _ = sender.send(PreviewResult {
                            generation,
                            photo_path,
                            original: None,
                            prepared: prepared_result.and_then(Result::ok),
                            effect: None,
                            source_grid: None,
                            target_grid: None,
                            clipped_points: 0,
                            warnings: Vec::new(),
                            grain_statistics: None,
                            smoothing: SmoothingReport {
                                mode: SmoothingMode::Off,
                                corrected_node_count: 0,
                                maximum_offset: 0.0,
                            },
                            consistency: None,
                            error: Some(error),
                        });
                        repaint.request_repaint();
                        return;
                    }
                },
            };
            if token.load(Ordering::Acquire) != generation {
                return;
            }

            let mut warnings = original.warnings.clone();
            let mut source_grid = None;
            let mut prepared_target = None;
            let mut clipped_points = 0;
            let mut grain_statistics = None;
            let mut smoothing = SmoothingReport {
                mode: SmoothingMode::Off,
                corrected_node_count: 0,
                maximum_offset: 0.0,
            };
            let mut consistency = None;
            let (effect, error, prepared) = if let Some(prepared_result) = prepared_result {
                match prepared_result {
                    Ok(prepared) => {
                        source_grid = Some(prepared.source_grid_size());
                        prepared_target = Some(prepared.target_grid_size());
                        clipped_points = prepared.clipped_points();
                        grain_statistics = prepared.grain_statistics().cloned();
                        smoothing = prepared.smoothing_report();
                        consistency = prepared.consistency_report().cloned();
                        warnings.extend(prepared.warnings().iter().cloned());
                        match apply_lut(&original, &prepared, Some((&token, generation))) {
                            Some(bitmap) => (Some(bitmap), None, Some(prepared)),
                            None => return,
                        }
                    }
                    Err(error) => (None, Some(error.to_string()), None),
                }
            } else {
                (None, None, None)
            };
            if token.load(Ordering::Acquire) != generation {
                return;
            }
            let _ = sender.send(PreviewResult {
                generation,
                photo_path,
                original: Some(original),
                prepared,
                effect,
                source_grid,
                target_grid: prepared_target,
                clipped_points,
                warnings,
                grain_statistics,
                smoothing,
                consistency,
                error,
            });
            repaint.request_repaint();
        });
    }

    fn receive_preview_results(&mut self, context: &egui::Context) {
        while let Ok(result) = self.preview_receiver.try_recv() {
            if result.generation != self.preview_generation.load(Ordering::Acquire)
                || (self.photo_path.as_ref() != Some(&result.photo_path)
                    && !(self.photo_path.is_none() && result.photo_path.as_os_str().is_empty()))
            {
                continue;
            }
            let suspicious_report = result
                .prepared
                .as_ref()
                .and_then(|prepared| prepared.consistency_report())
                .filter(|report| report.needs_confirmation())
                .cloned();
            let effective_smoothing = result
                .prepared
                .as_ref()
                .map(|prepared| prepared.smoothing_report())
                .unwrap_or(result.smoothing);
            self.prepared_lut = result.prepared;
            if let Some(original) = result.original {
                self.preview_original_texture = Some(load_bitmap_texture(
                    context,
                    "preview-original",
                    original.as_ref(),
                ));
                self.preview_original = Some(original);
            }
            self.preview_effect_texture = result
                .effect
                .as_ref()
                .map(|effect| load_bitmap_texture(context, "preview-effect", effect));
            self.preview_status = if let Some(error) = result.error {
                if self.prepared_lut.is_none() && self.input_path.is_some() {
                    self.status = AppStatus::Error(error.clone());
                }
                PreviewStatus::Error(error)
            } else {
                if self.automatic_smoothing_selection {
                    self.smoothing_mode = effective_smoothing.mode;
                }
                PreviewStatus::Ready {
                    source_grid: result.source_grid,
                    target_grid: result.target_grid,
                    clipped_points: result.clipped_points,
                    warnings: result.warnings,
                    grain_statistics: result.grain_statistics.map(Box::new),
                    smoothing: result.smoothing,
                    consistency: result.consistency.map(Box::new),
                }
            };
            if let (Some(input_path), Some(report)) = (self.input_path.clone(), suspicious_report)
                && self.confirmed_suspect_input.as_ref() != Some(&input_path)
            {
                self.pending_inconsistency = Some(PendingInconsistency { input_path, report });
            }
        }
    }

    fn convert(&mut self, context: &egui::Context) {
        if self.pending_inconsistency.is_some() {
            return;
        }
        if self.operation_busy || matches!(self.preview_status, PreviewStatus::Processing) {
            self.status = AppStatus::Error("文件仍在检查中，请稍候。".to_owned());
            return;
        }
        let Some(prepared) = self.prepared_lut.clone() else {
            self.status =
                AppStatus::Error("请先导入可用的调色文件；检查通过后才能导出。".to_owned());
            return;
        };
        let Some(input) = self.input_path.clone() else {
            return;
        };
        let input_title = suggested_title(&input).unwrap_or_else(|| "MyLUT".to_owned());
        let default_name = format!("{}.cube", safe_filename_stem(&input_title));
        let path = rfd::FileDialog::new()
            .set_title(format!("保存 {}-grid CUBE", self.target_grid))
            .set_file_name(default_name)
            .add_filter("Adobe CUBE", &["cube"])
            .save_file();
        let Some(path) = path else {
            return;
        };
        let path = ensure_extension(path, "cube");
        let output_title = suggested_title(&path).unwrap_or(input_title);
        self.operation_busy = true;
        self.status = AppStatus::Working("正在保存并校验 CUBE…");
        let sender = self.operation_sender.clone();
        let repaint = context.clone();
        thread::spawn(move || {
            let result = export_prepared_cube(&prepared, &output_title, &path)
                .map_err(|error| error.to_string());
            let _ = sender.send(OperationResult::Export(Box::new(result)));
            repaint.request_repaint();
        });
    }

    fn show_conversion_success(&mut self, report: ConversionReport) {
        let operation = match report.source_grid_size.cmp(&report.output_grid_size) {
            std::cmp::Ordering::Greater => "已缩减网格",
            std::cmp::Ordering::Less => "已插值放大（不能增加原有细节）",
            std::cmp::Ordering::Equal => "保持原网格",
        };
        let compatibility = if report.output_grid_size == 64 {
            "仅适合电脑软件，不可导入 LUMIX 相机"
        } else {
            "可导入 LUMIX 相机"
        };
        let clipped_percentage =
            report.clipped_points as f64 * 100.0 / report.entry_count.max(1) as f64;
        let mut details = vec![
            format!(
                "{}→{} 点 · {operation} · {compatibility}",
                report.source_grid_size, report.output_grid_size,
            ),
            format!(
                "共 {} 个颜色样本；超出范围的 {} 个已截取（{clipped_percentage:.2}%）",
                report.entry_count, report.clipped_points,
            ),
            format!(
                "数据范围 {:.6}–{:.6}",
                report.validation.min_value, report.validation.max_value,
            ),
        ];
        if let Some(statistics) = &report.grain_statistics {
            let sampling_method = if statistics.samples_per_node > 1 {
                "已合并重复采样"
            } else {
                "单次采样"
            };
            details.insert(
                0,
                format!(
                    "{}中性图 · {}×{} · 每个颜色 {} 次采样 · {sampling_method}",
                    statistics.sampling.display_name(),
                    statistics.input_width,
                    statistics.input_height,
                    statistics.samples_per_node
                ),
            );
            details.insert(
                1,
                format!(
                    "颗粒波动：平均 {:.6} · 95% 不超过 {:.6} · 最大 {:.6} · 样本 {}→颜色 {}",
                    statistics.mean_standard_deviation,
                    statistics.p95_standard_deviation,
                    statistics.max_standard_deviation,
                    statistics.input_sample_count,
                    statistics.averaged_node_count
                ),
            );
            details.insert(
                2,
                format!(
                    "重复采样：常规平均 {} 个颜色 · 稳健平均 {} 个颜色 · {} 个颜色中排除 {} 个过曝/欠曝样本",
                    statistics.arithmetic_node_count,
                    statistics.huber_node_count,
                    statistics.clipped_node_count,
                    statistics.removed_clipped_samples
                ),
            );
        }
        if report.smoothing.mode != SmoothingMode::Off {
            details.push(format!(
                "平滑降噪：{} · 调整 {} 个颜色 · 最大调整量 {:.6}",
                report.smoothing.mode.display_name(),
                report.smoothing.corrected_node_count,
                report.smoothing.maximum_offset
            ));
        }
        if let Some(consistency) = &report.consistency {
            details.push(format_consistency_report(consistency));
        }
        details.extend(report.warnings.iter().map(|warning| format!("⚠ {warning}")));
        self.status = AppStatus::Success {
            headline: format!("{} 点 CUBE 已保存并检查通过", report.output_grid_size),
            details,
            path: report.output_path,
        };
    }
}

impl eframe::App for LumixApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let context = ui.ctx().clone();
        self.receive_operation_results(&context);
        self.receive_preview_results(&context);
        self.handle_dropped_files(&context);
        Frame::central_panel(ui.style())
            .fill(BG)
            .inner_margin(egui::Margin::same(0))
            .show(ui, |ui| {
                paint_background(ui, ui.max_rect());
                Frame::new()
                    .inner_margin(egui::Margin::same(PAGE_PADDING))
                    .show(ui, |ui| {
                        render_header(ui, &mut self.show_license_guide, &self.status);
                        add_vertical_gap(ui, FORM_GAP);
                        let body_size = Vec2::new(ui.available_width(), ui.available_height());
                        ui.allocate_ui_with_layout(
                            body_size,
                            Layout::left_to_right(Align::Min),
                            |ui| {
                                let body_height = ui.available_height();
                                ui.allocate_ui_with_layout(
                                    Vec2::new(LEFT_COLUMN_WIDTH, body_height),
                                    Layout::top_down(Align::Min),
                                    |ui| {
                                        let content_height = (ui.available_height()
                                            - SAFETY_BUTTON_HEIGHT
                                            - FORM_GAP)
                                            .max(0.0);
                                        ui.allocate_ui_with_layout(
                                            Vec2::new(ui.available_width(), content_height),
                                            Layout::top_down(Align::Min),
                                            |ui| {
                                                egui::ScrollArea::vertical()
                                                    .auto_shrink([false, false])
                                                    .show(ui, |ui| {
                                                        module_frame().show(ui, |ui| {
                                                            render_generate_section(ui, self);
                                                        });
                                                        add_vertical_gap(ui, FORM_GAP);
                                                        module_frame().show(ui, |ui| {
                                                            render_convert_section(
                                                                ui, self, &context,
                                                            );
                                                        });
                                                        if !matches!(self.status, AppStatus::Ready)
                                                        {
                                                            add_vertical_gap(ui, FORM_GAP);
                                                            module_frame().show(ui, |ui| {
                                                                render_status(ui, &self.status);
                                                            });
                                                        }
                                                    });
                                            },
                                        );
                                        add_vertical_gap(ui, FORM_GAP);
                                        if safety_guide_button(ui) {
                                            self.show_safety_guide = true;
                                        }
                                    },
                                );
                                add_horizontal_gap(ui, COLUMN_GAP);
                                main_card_frame().show(ui, |ui| {
                                    ui.set_min_width(ui.available_width());
                                    let inner_height =
                                        (body_height - CARD_PADDING_Y as f32 * 2.0).max(0.0);
                                    ui.set_min_height(inner_height);
                                    ui.set_max_height(inner_height);
                                    ui.with_layout(Layout::top_down(Align::Min), |ui| {
                                        render_preview_panel(ui, self, &context);
                                    });
                                });
                            },
                        );
                    });
            });
        render_safety_popup(&context, self);
        render_license_popup(&context, self);
        render_inconsistency_modal(&context, self);
    }
}

fn module_frame() -> Frame {
    main_card_frame()
}

fn main_card_frame() -> Frame {
    Frame::new()
        .fill(SURFACE)
        .corner_radius(CARD_CORNER_RADIUS)
        .stroke(Stroke::new(1.0, Color32::from_white_alpha(12)))
        .inner_margin(egui::Margin::symmetric(CARD_PADDING_X, CARD_PADDING_Y))
}

fn render_header(ui: &mut egui::Ui, show_license_guide: &mut bool, status: &AppStatus) {
    ui.horizontal(|ui| {
        cube_mark(ui);
        ui.add_space(CONTROL_GAP);
        ui.label(
            RichText::new("LUMIX LUT Maker")
                .size(15.0)
                .strong()
                .color(TEXT),
        );
        let (summary, color) = match status {
            AppStatus::Ready => (None, MUTED),
            AppStatus::Working(message) => (Some(*message), WARNING),
            AppStatus::Success { headline, .. } => (Some(headline.as_str()), SUCCESS),
            AppStatus::Error(_) => (Some("文件处理失败，请查看左侧提示"), ERROR),
        };
        if let Some(summary) = summary {
            ui.add_space(CONTROL_GAP);
            ui.add_sized(
                [240.0, 20.0],
                egui::Label::new(RichText::new(summary).size(11.5).color(color)).truncate(),
            );
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add(
                    egui::Button::new(RichText::new("许可说明").size(10.5).color(MUTED))
                        .fill(Color32::TRANSPARENT)
                        .stroke(Stroke::NONE),
                )
                .clicked()
            {
                *show_license_guide = true;
            }
            ui.add_space(COMPACT_GAP);
            ui.label(
                RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                    .size(10.5)
                    .color(FAINT),
            );
        });
    });
}

fn cube_mark(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(20.0), egui::Sense::hover());
    let painter = ui.painter();
    let center = rect.center();
    let radius = 8.0;
    let stroke = Stroke::new(1.2, Color32::from_white_alpha(150));
    let points: Vec<Pos2> = (0..6)
        .map(|index| {
            let angle = std::f32::consts::TAU * index as f32 / 6.0 - std::f32::consts::FRAC_PI_2;
            Pos2::new(
                center.x + radius * angle.cos(),
                center.y + radius * angle.sin(),
            )
        })
        .collect();
    for index in 0..6 {
        painter.line_segment([points[index], points[(index + 1) % 6]], stroke);
    }
    painter.line_segment([points[0], center], stroke);
    painter.line_segment([points[2], center], stroke);
    painter.line_segment([points[4], center], stroke);
    painter.circle_filled(center, 1.6, ACCENT);
}

fn section_header(ui: &mut egui::Ui, number: &str, title: &str, caption: &str) {
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(number).size(12.0).strong().color(ACCENT));
            ui.add_space(MICRO_GAP);
            ui.label(RichText::new(title).size(15.0).strong().color(TEXT));
        });
        ui.label(RichText::new(caption).size(11.5).color(FAINT));
    });
}

fn format_consistency_report(report: &ConsistencyReport) -> String {
    let mut parts = vec!["综合一致性诊断".to_owned()];
    if let Some(mae) = report.heldout_mae_8bit {
        parts.push(format!("留出 RGB MAE {mae:.2}/255"));
    }
    if let Some(delta_e) = report.heldout_delta_e76 {
        parts.push(format!("ΔE76 {delta_e:.2}"));
    }
    if let Some(ssim) = report.edge_ssim {
        parts.push(format!("边缘 SSIM {ssim:.3}"));
    }
    if let Some((x, y)) = report.estimated_offset_pixels {
        parts.push(format!("估计偏移 {x:+.1}, {y:+.1}px"));
    }
    if let (Some(matches), Some(inliers)) = (report.feature_matches, report.ransac_inliers) {
        parts.push(format!("特征/RANSAC {inliers}/{matches}"));
    }
    if let Some(rotation) = report.estimated_rotation_degrees {
        parts.push(format!("旋转 {rotation:+.2}°"));
    }
    if let Some(scale) = report.estimated_scale_percent {
        parts.push(format!("缩放 {scale:.2}%"));
    }
    parts.join(" · ")
}

fn field_label(ui: &mut egui::Ui, label: &str) {
    ui.label(RichText::new(label).size(11.5).color(FAINT));
}

fn render_generate_section(ui: &mut egui::Ui, app: &mut LumixApp) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("01").size(12.0).strong().color(ACCENT));
        ui.add_space(MICRO_GAP);
        ui.label(
            RichText::new("制作调色母版")
                .size(15.0)
                .strong()
                .color(TEXT),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.allocate_ui(Vec2::new(126.0, 30.0), |ui| {
                let formats = [NeutralFormat::Png, NeutralFormat::Tiff];
                let labels = ["PNG", "TIFF"];
                let selected = formats
                    .iter()
                    .position(|format| *format == app.neutral_format)
                    .unwrap_or(0);
                if let Some(clicked) = segmented_control(ui, "master-format", &labels, selected) {
                    app.neutral_format = formats[clicked];
                }
            });
        });
    });
    let source_label = app
        .selected_source_path
        .as_deref()
        .or(app.master_source_path.as_deref())
        .and_then(Path::file_name)
        .and_then(|value| value.to_str())
        .unwrap_or("未选择照片");
    let source_caption = if app.reference_uses_builtin_sample {
        if let Some(size) = app.master_canvas_size {
            format!("参考照片：内置样片 · 已就绪（{size} 点）")
        } else {
            "参考照片：内置样片".to_owned()
        }
    } else if app.selected_source_path.is_none() {
        "参考照片：未上传，将自动使用内置样片".to_owned()
    } else if let Some(size) = app.master_canvas_size {
        format!("参考照片：{source_label} · 已就绪（{size} 点）")
    } else {
        format!("参考照片：{source_label}")
    };
    ui.add(egui::Label::new(RichText::new(&source_caption).size(11.5).color(MUTED)).truncate())
        .on_hover_text(source_caption);
    add_vertical_gap(ui, CONTROL_GAP);
    ui.horizontal(|ui| {
        let gap = ui.spacing().item_spacing.x;
        let button_width = ((ui.available_width() - gap) / 2.0).max(0.0);
        ui.allocate_ui(Vec2::new(button_width, 44.0), |ui| {
            if primary_button(ui, "选择参考照片", !app.operation_busy) {
                app.choose_master_source(ui.ctx());
            }
        });
        ui.allocate_ui(Vec2::new(button_width, 44.0), |ui| {
            let using_default_sample =
                app.selected_source_path.is_none() || app.reference_uses_builtin_sample;
            let generate_label = if using_default_sample {
                "用内置样片生成母版"
            } else {
                "保存调色母版"
            };
            if primary_button(
                ui,
                generate_label,
                (app.master_baseline.is_some() || using_default_sample) && !app.operation_busy,
            ) {
                app.generate_photo_master(ui.ctx());
            }
        });
    });
    add_vertical_gap(ui, CONTROL_GAP);
    egui::CollapsingHeader::new(
        RichText::new("不使用参考照片？单独生成中性图")
            .size(11.5)
            .color(MUTED),
    )
    .id_salt("standalone-neutral")
    .default_open(false)
    .show(ui, |ui| render_standalone_neutral_section(ui, app));
}

fn render_standalone_neutral_section(ui: &mut egui::Ui, app: &mut LumixApp) {
    let spec = NeutralSpec {
        format: app.neutral_format,
        sampling: app.neutral_sampling,
        ..NeutralSpec::default()
    };
    let (width, height) = spec.output_dimensions();
    let caption = format!(
        "{width}×{height} · 16 位 RGB · sRGB · 每个颜色采样 {} 次",
        app.neutral_sampling.samples_per_node()
    );
    ui.label(RichText::new(caption).size(11.0).color(FAINT));
    add_vertical_gap(ui, CONTROL_GAP);
    if app.neutral_sampling == NeutralSampling::Single {
        field_label(ui, "采样模式");
    } else if app.neutral_sampling == NeutralSampling::Average16 {
        ui.label(
            RichText::new("采样模式 · 推荐抗颗粒档 · 仅减弱随机颗粒")
                .size(10.5)
                .color(WARNING),
        );
    } else {
        ui.label(
            RichText::new("采样模式 · 仅减弱随机颗粒，空间效果仍无法转换")
                .size(10.5)
                .color(WARNING),
        );
    }
    add_vertical_gap(ui, COMPACT_GAP);
    let samplings = [
        NeutralSampling::Single,
        NeutralSampling::Average16,
        NeutralSampling::Average64,
    ];
    let sampling_labels = ["普通", "抗颗粒16×", "抗颗粒64×"];
    let sampling_selected = samplings
        .iter()
        .position(|sampling| *sampling == app.neutral_sampling)
        .unwrap_or(0);
    if let Some(clicked) =
        segmented_control(ui, "neutral-sampling", &sampling_labels, sampling_selected)
    {
        let sampling = samplings[clicked];
        if sampling != app.neutral_sampling {
            app.neutral_sampling = sampling;
            app.neutral_format = sampling.default_format();
        }
    }
    add_vertical_gap(ui, CONTROL_GAP);
    field_label(ui, "文件格式");
    add_vertical_gap(ui, COMPACT_GAP);
    let formats = [NeutralFormat::Png, NeutralFormat::Tiff];
    let labels: Vec<&str> = formats.iter().map(|format| format.display_name()).collect();
    let selected = formats
        .iter()
        .position(|format| *format == app.neutral_format)
        .unwrap_or(0);
    if let Some(clicked) = segmented_control(ui, "format", &labels, selected) {
        app.neutral_format = formats[clicked];
    }
    add_vertical_gap(ui, CONTROL_GAP);
    let label = format!("生成 {}", spec.default_filename());
    if primary_button(ui, &label, !app.operation_busy) {
        app.generate_neutral(ui.ctx());
    }
}

fn render_convert_section(ui: &mut egui::Ui, app: &mut LumixApp, context: &egui::Context) {
    section_header(ui, "02", "导回调色结果", "另存整张母版后导入这里");
    add_vertical_gap(ui, CONTROL_GAP);
    let input_text = app
        .input_path
        .as_deref()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str())
        .unwrap_or("拖入调色后的文件，或点击选择");
    let input_drop = drop_zone(ui, input_text, app.input_path.is_some());
    app.lut_drop_rect = Some(input_drop.rect);
    if input_drop.clicked() {
        app.choose_input(context);
    }
    if app.input_path.is_some() {
        let (message, color) = match &app.preview_status {
            PreviewStatus::Processing => ("正在检查导入文件…", WARNING),
            _ if app.prepared_lut.is_some() => ("文件检查通过，可以导出。", SUCCESS),
            PreviewStatus::Error(error) => (error.as_str(), ERROR),
            _ => ("等待文件检查。", MUTED),
        };
        ui.add(egui::Label::new(RichText::new(message).size(11.0).color(color)).truncate())
            .on_hover_text(message);
    } else if app.generated_master_path.is_some() {
        ui.label(
            RichText::new("母版已保存。调色并另存后，把新文件放到这里。")
                .size(11.0)
                .color(WARNING),
        );
    }
    add_vertical_gap(ui, CONTROL_GAP);
    field_label(ui, "输出网格（每边点数）");
    add_vertical_gap(ui, COMPACT_GAP);
    let labels: Vec<String> = OUTPUT_GRID_PRESETS
        .iter()
        .map(|grid| grid.to_string())
        .collect();
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let selected = OUTPUT_GRID_PRESETS
        .iter()
        .position(|grid| *grid == app.target_grid)
        .unwrap_or(2);
    if let Some(clicked) = segmented_control(ui, "grid", &label_refs, selected) {
        let next_grid = OUTPUT_GRID_PRESETS[clicked];
        if app.target_grid != next_grid {
            app.target_grid = next_grid;
            app.schedule_preview(context, false);
        }
    }
    add_vertical_gap(ui, CONTROL_GAP);
    field_label(
        ui,
        if app.automatic_smoothing_selection {
            "平滑降噪 · 抗颗粒图默认轻度"
        } else {
            "平滑降噪"
        },
    );
    add_vertical_gap(ui, COMPACT_GAP);
    let smoothing_modes = [
        SmoothingMode::Auto,
        SmoothingMode::Off,
        SmoothingMode::Light,
        SmoothingMode::Medium,
    ];
    let smoothing_labels = ["自动", "关闭", "轻度", "中度"];
    let smoothing_selected = if app.automatic_smoothing_selection {
        0
    } else {
        smoothing_modes
            .iter()
            .position(|mode| *mode == app.smoothing_mode)
            .unwrap_or(1)
    };
    if let Some(clicked) =
        segmented_control(ui, "smoothing-mode", &smoothing_labels, smoothing_selected)
    {
        app.smoothing_mode = smoothing_modes[clicked];
        app.automatic_smoothing_selection = clicked == 0;
        app.schedule_preview(context, false);
    }
    add_vertical_gap(ui, CONTROL_GAP);
    let compatibility = if app.target_grid == 64 {
        RichText::new("64 点：仅适合电脑软件，不能导入 LUMIX 相机")
            .size(11.0)
            .color(WARNING)
    } else {
        RichText::new(format!("{} 点：可导入 LUMIX 相机", app.target_grid))
            .size(11.0)
            .color(FAINT)
    };
    ui.label(compatibility);
    add_vertical_gap(ui, CONTROL_GAP);
    let label = format!("导出 {} 点 CUBE", app.target_grid);
    let can_convert =
        app.prepared_lut.is_some() && app.pending_inconsistency.is_none() && !app.operation_busy;
    if primary_button(ui, &label, can_convert) {
        app.convert(context);
    }
}

fn segmented_control(
    ui: &mut egui::Ui,
    id_salt: &str,
    options: &[&str],
    selected: usize,
) -> Option<usize> {
    let width = ui.available_width();
    let height = 34.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 8, Color32::from_white_alpha(5));
    painter.rect_stroke(
        rect,
        8,
        Stroke::new(1.0, Color32::from_white_alpha(14)),
        StrokeKind::Inside,
    );
    let inner = rect.shrink(3.0);
    let segment_width = inner.width() / options.len() as f32;
    let mut clicked = None;
    for (index, label) in options.iter().enumerate() {
        let segment = Rect::from_min_size(
            Pos2::new(inner.left() + segment_width * index as f32, inner.top()),
            Vec2::new(segment_width, inner.height()),
        );
        let response = ui.interact(
            segment,
            ui.id().with(id_salt).with(index),
            egui::Sense::click(),
        );
        let is_selected = index == selected;
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, is_selected, *label)
        });
        if is_selected {
            painter.rect_filled(segment.shrink(1.0), 6, tint(ACCENT, 24));
            painter.rect_stroke(
                segment.shrink(1.0),
                6,
                Stroke::new(1.0, tint(ACCENT, 90)),
                StrokeKind::Inside,
            );
        } else if response.hovered() {
            painter.rect_filled(segment.shrink(1.0), 6, Color32::from_white_alpha(6));
        }
        let color = if is_selected { TEXT } else { MUTED };
        painter.text(
            segment.center(),
            Align2::CENTER_CENTER,
            *label,
            FontId::proportional(12.5),
            color,
        );
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if response.clicked() {
            clicked = Some(index);
        }
    }
    clicked
}

fn drop_zone(ui: &mut egui::Ui, text: &str, has_file: bool) -> Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 56.0), egui::Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, text));
    let painter = ui.painter();
    let hovered = response.hovered();
    painter.rect_filled(
        rect,
        10,
        Color32::from_white_alpha(if hovered { 7 } else { 4 }),
    );
    painter.rect_stroke(
        rect,
        10,
        Stroke::new(
            1.0,
            if hovered {
                tint(ACCENT, 120)
            } else {
                Color32::from_white_alpha(20)
            },
        ),
        StrokeKind::Inside,
    );
    let icon_center = Pos2::new(rect.left() + 30.0, rect.center().y);
    let icon_stroke = Stroke::new(
        1.4,
        if hovered {
            ACCENT
        } else {
            Color32::from_white_alpha(110)
        },
    );
    painter.line_segment(
        [
            Pos2::new(icon_center.x - 8.0, icon_center.y + 1.0),
            Pos2::new(icon_center.x - 8.0, icon_center.y + 6.0),
        ],
        icon_stroke,
    );
    painter.line_segment(
        [
            Pos2::new(icon_center.x - 8.0, icon_center.y + 6.0),
            Pos2::new(icon_center.x + 8.0, icon_center.y + 6.0),
        ],
        icon_stroke,
    );
    painter.line_segment(
        [
            Pos2::new(icon_center.x + 8.0, icon_center.y + 6.0),
            Pos2::new(icon_center.x + 8.0, icon_center.y + 1.0),
        ],
        icon_stroke,
    );
    painter.line_segment(
        [
            Pos2::new(icon_center.x, icon_center.y - 7.0),
            Pos2::new(icon_center.x, icon_center.y + 2.0),
        ],
        icon_stroke,
    );
    painter.line_segment(
        [
            Pos2::new(icon_center.x - 3.5, icon_center.y - 1.5),
            Pos2::new(icon_center.x, icon_center.y + 2.0),
        ],
        icon_stroke,
    );
    painter.line_segment(
        [
            Pos2::new(icon_center.x + 3.5, icon_center.y - 1.5),
            Pos2::new(icon_center.x, icon_center.y + 2.0),
        ],
        icon_stroke,
    );
    painter
        .with_clip_rect(Rect::from_min_max(
            Pos2::new(rect.left() + 50.0, rect.top()),
            Pos2::new(rect.right() - 12.0, rect.bottom()),
        ))
        .text(
            Pos2::new(rect.left() + 50.0, rect.center().y),
            Align2::LEFT_CENTER,
            text,
            FontId::proportional(12.5),
            if has_file { TEXT } else { MUTED },
        );
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.on_hover_text(text)
}

fn primary_button(ui: &mut egui::Ui, label: &str, enabled: bool) -> bool {
    ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(label).size(13.5).color(PRIMARY_TEXT))
            .fill(PRIMARY)
            .corner_radius(8)
            .min_size(Vec2::new(ui.available_width(), 44.0)),
    )
    .clicked()
}

fn render_preview_panel(ui: &mut egui::Ui, app: &mut LumixApp, context: &egui::Context) {
    let source_width = (ui.available_width() * 0.58).clamp(240.0, 388.0);
    let heading_width = (ui.available_width() - source_width - CONTROL_GAP).max(90.0);
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(heading_width, 34.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.label(RichText::new("照片预览").size(15.0).strong().color(TEXT));
                let source_name = if app.reference_uses_builtin_sample
                    && matches!(app.preview_source, PreviewSource::Reference)
                {
                    Some("内置样片")
                } else if !matches!(app.preview_source, PreviewSource::BuiltIn(_)) {
                    app.photo_path
                        .as_deref()
                        .and_then(Path::file_name)
                        .and_then(|name| name.to_str())
                } else {
                    None
                };
                if let Some(source_name) = source_name {
                    ui.add(
                        egui::Label::new(RichText::new(source_name).size(11.0).color(MUTED))
                            .truncate(),
                    )
                    .on_hover_text(source_name);
                }
            },
        );
        add_horizontal_gap(ui, CONTROL_GAP);
        ui.allocate_ui(Vec2::new(source_width, 34.0), |ui| {
            let mut labels: Vec<&str> = BUILT_IN_PREVIEW_PHOTOS
                .iter()
                .map(|photo| photo.label)
                .collect();
            labels.push("参考照片");
            labels.push("导入照片");
            let selected = match app.preview_source {
                PreviewSource::BuiltIn(index) => index,
                PreviewSource::Reference => BUILT_IN_PREVIEW_PHOTOS.len(),
                PreviewSource::Custom => BUILT_IN_PREVIEW_PHOTOS.len() + 1,
            };
            if let Some(clicked) = segmented_control(ui, "preview-source", &labels, selected) {
                if clicked < BUILT_IN_PREVIEW_PHOTOS.len() {
                    if app.preview_source != PreviewSource::BuiltIn(clicked) {
                        app.set_builtin_photo(clicked, context);
                    }
                } else if clicked == BUILT_IN_PREVIEW_PHOTOS.len() {
                    app.set_reference_photo(context);
                } else if app.preview_source == PreviewSource::Custom
                    || app.custom_photo_path.is_none()
                {
                    app.choose_photo(context);
                } else {
                    app.set_photo(app.custom_photo_path.clone().unwrap(), context);
                }
            }
        });
    });
    add_vertical_gap(ui, PREVIEW_GAP);
    ui.horizontal(|ui| {
        ui.allocate_ui(Vec2::new(232.0, 34.0), |ui| {
            let modes = [PreviewMode::Split, PreviewMode::Toggle, PreviewMode::Dual];
            let labels = ["分割", "切换", "双图"];
            let selected = modes
                .iter()
                .position(|mode| *mode == app.preview_mode)
                .unwrap_or(0);
            if let Some(clicked) = segmented_control(ui, "preview-mode", &labels, selected) {
                app.preview_mode = modes[clicked];
                app.pan = Vec2::ZERO;
            }
        });
        if app.preview_mode == PreviewMode::Toggle {
            ui.add_space(CONTROL_GAP);
            ui.allocate_ui(Vec2::new(130.0, 34.0), |ui| {
                let selected = usize::from(app.show_effect);
                if let Some(clicked) =
                    segmented_control(ui, "preview-toggle", &["原图", "LUT"], selected)
                {
                    app.show_effect = clicked == 1;
                }
            });
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if toolbar_button(ui, "适应") {
                app.zoom = 1.0;
                app.pan = Vec2::ZERO;
            }
            if toolbar_button(ui, "+") {
                app.zoom = (app.zoom * 1.2).min(8.0);
            }
            ui.label(
                RichText::new(format!("{:.0}%", app.zoom * 100.0))
                    .monospace()
                    .size(10.5)
                    .color(MUTED),
            );
            if toolbar_button(ui, "−") {
                app.zoom = (app.zoom / 1.2).max(0.25);
            }
        });
    });
    add_vertical_gap(ui, PREVIEW_GAP);

    let info_height = 30.0;
    let canvas_height = (ui.available_height() - info_height).max(0.0);
    let (canvas, canvas_response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), canvas_height),
        egui::Sense::click_and_drag(),
    );
    app.photo_drop_rect = Some(canvas);
    let painter = ui.painter();
    painter.rect_filled(canvas, 12, Color32::from_rgb(9, 9, 11));
    painter.rect_stroke(
        canvas,
        12,
        Stroke::new(
            1.0,
            if canvas_response.hovered() {
                Color32::from_white_alpha(28)
            } else {
                Color32::from_white_alpha(13)
            },
        ),
        StrokeKind::Inside,
    );

    if canvas_response.double_clicked() {
        app.zoom = 1.0;
        app.pan = Vec2::ZERO;
    }
    if canvas_response.hovered() {
        let scroll = ui.input(|input| input.smooth_scroll_delta.y);
        if scroll.abs() > f32::EPSILON {
            let factor = (scroll * 0.0025).exp();
            app.zoom = (app.zoom * factor).clamp(0.25, 8.0);
        }
    }

    let original = app.preview_original_texture.as_ref().map(TextureHandle::id);
    let effect = app.preview_effect_texture.as_ref().map(TextureHandle::id);
    let dimensions = app
        .preview_original
        .as_ref()
        .map(|bitmap| Vec2::new(bitmap.width as f32, bitmap.height as f32));

    let mut dragging_split = false;
    if let (Some(original), Some(dimensions)) = (original, dimensions) {
        match app.preview_mode {
            PreviewMode::Split => {
                let image_rect =
                    fitted_image_rect(canvas.shrink(8.0), dimensions, app.zoom, app.pan);
                let split_x = canvas.left() + canvas.width() * app.split_position;
                let left_clip = Rect::from_min_max(canvas.min, Pos2::new(split_x, canvas.bottom()));
                let right_clip = Rect::from_min_max(Pos2::new(split_x, canvas.top()), canvas.max);
                paint_texture(ui, original, image_rect, left_clip);
                if let Some(effect) = effect {
                    paint_texture(ui, effect, image_rect, right_clip);
                } else {
                    paint_texture(ui, original, image_rect, right_clip);
                }
                painter.line_segment(
                    [
                        Pos2::new(split_x, canvas.top()),
                        Pos2::new(split_x, canvas.bottom()),
                    ],
                    Stroke::new(1.5, Color32::from_white_alpha(210)),
                );
                painter.circle_filled(
                    Pos2::new(split_x, canvas.center().y),
                    12.0,
                    Color32::from_black_alpha(190),
                );
                painter.circle_stroke(
                    Pos2::new(split_x, canvas.center().y),
                    12.0,
                    Stroke::new(1.0, Color32::from_white_alpha(100)),
                );
                painter.text(
                    Pos2::new(canvas.left() + 14.0, canvas.top() + 14.0),
                    Align2::LEFT_TOP,
                    "原图",
                    FontId::proportional(10.5),
                    Color32::from_white_alpha(180),
                );
                painter.text(
                    Pos2::new(canvas.right() - 14.0, canvas.top() + 14.0),
                    Align2::RIGHT_TOP,
                    "LUT",
                    FontId::proportional(10.5),
                    Color32::from_white_alpha(180),
                );
                let handle_rect = Rect::from_center_size(
                    Pos2::new(split_x, canvas.center().y),
                    Vec2::new(26.0, canvas.height()),
                );
                let handle = ui.interact(
                    handle_rect,
                    ui.id().with("preview-split-handle"),
                    egui::Sense::drag(),
                );
                handle.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Slider,
                        true,
                        "原图与 LUT 效果分割位置",
                    )
                });
                if handle.hovered() || handle.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                if handle.has_focus() {
                    if ui.input(|input| input.key_pressed(egui::Key::ArrowLeft)) {
                        app.split_position = (app.split_position - 0.05).max(0.03);
                    }
                    if ui.input(|input| input.key_pressed(egui::Key::ArrowRight)) {
                        app.split_position = (app.split_position + 0.05).min(0.97);
                    }
                }
                if handle.dragged()
                    && let Some(pointer) = handle.interact_pointer_pos()
                {
                    app.split_position =
                        ((pointer.x - canvas.left()) / canvas.width()).clamp(0.03, 0.97);
                    dragging_split = true;
                }
            }
            PreviewMode::Toggle => {
                let image_rect =
                    fitted_image_rect(canvas.shrink(8.0), dimensions, app.zoom, app.pan);
                let texture = if app.show_effect {
                    effect.unwrap_or(original)
                } else {
                    original
                };
                paint_texture(ui, texture, image_rect, canvas);
                painter.text(
                    Pos2::new(canvas.left() + 14.0, canvas.top() + 14.0),
                    Align2::LEFT_TOP,
                    if app.show_effect {
                        "LUT 效果"
                    } else {
                        "原图"
                    },
                    FontId::proportional(10.5),
                    Color32::from_white_alpha(180),
                );
            }
            PreviewMode::Dual => {
                let gap = 8.0;
                let width = (canvas.width() - gap) / 2.0;
                let left = Rect::from_min_size(canvas.min, Vec2::new(width, canvas.height()));
                let right = Rect::from_min_size(
                    Pos2::new(left.right() + gap, canvas.top()),
                    Vec2::new(width, canvas.height()),
                );
                let left_image = fitted_image_rect(left.shrink(6.0), dimensions, app.zoom, app.pan);
                let right_image =
                    fitted_image_rect(right.shrink(6.0), dimensions, app.zoom, app.pan);
                paint_texture(ui, original, left_image, left);
                paint_texture(ui, effect.unwrap_or(original), right_image, right);
                painter.text(
                    Pos2::new(left.left() + 12.0, left.top() + 12.0),
                    Align2::LEFT_TOP,
                    "原图",
                    FontId::proportional(10.5),
                    Color32::from_white_alpha(180),
                );
                painter.text(
                    Pos2::new(right.left() + 12.0, right.top() + 12.0),
                    Align2::LEFT_TOP,
                    "LUT 效果",
                    FontId::proportional(10.5),
                    Color32::from_white_alpha(180),
                );
            }
        }
    } else {
        render_preview_empty(ui, canvas, app);
    }

    if canvas_response.dragged() && !dragging_split {
        app.pan += ui.input(|input| input.pointer.delta());
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    } else if canvas_response.hovered() && original.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }

    if matches!(app.preview_status, PreviewStatus::Processing) {
        painter.rect_filled(canvas, 12, Color32::from_black_alpha(70));
        painter.text(
            canvas.center(),
            Align2::CENTER_CENTER,
            "正在生成预览…",
            FontId::proportional(13.0),
            TEXT,
        );
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    }
    ui.add_space(CONTROL_GAP);
    render_preview_info(ui, app);
}

fn render_preview_empty(ui: &egui::Ui, canvas: Rect, app: &LumixApp) {
    let (title, detail) = if app.photo_path.is_none() {
        (
            "拖入一张照片开始预览",
            "支持 HEIC、JPEG、PNG、TIFF · 也可点击右上角选择",
        )
    } else {
        ("正在准备照片", "完成后将在这里显示原图与 LUT 效果")
    };
    ui.painter().text(
        canvas.center() - Vec2::new(0.0, 10.0),
        Align2::CENTER_CENTER,
        title,
        FontId::proportional(14.0),
        MUTED,
    );
    ui.painter().text(
        canvas.center() + Vec2::new(0.0, 14.0),
        Align2::CENTER_CENTER,
        detail,
        FontId::proportional(10.5),
        FAINT,
    );
}

fn render_preview_info(ui: &mut egui::Ui, app: &LumixApp) {
    let (message, color, detail) = match &app.preview_status {
        PreviewStatus::Idle => (
            "选择调色文件后，这里会显示应用 LUT 前后的对比。".to_owned(),
            MUTED,
            None,
        ),
        PreviewStatus::Processing => ("正在检查文件并生成预览…".to_owned(), WARNING, None),
        PreviewStatus::Error(error) => (format!("无法预览：{error}"), ERROR, None),
        PreviewStatus::Ready {
            source_grid,
            target_grid,
            clipped_points,
            warnings,
            grain_statistics,
            smoothing,
            consistency,
        } => {
            let message = if let (Some(source), Some(target)) = (source_grid, target_grid) {
                format!(
                    "预览与导出使用同一结果 · {source}→{target} 点 · 裁切 {clipped_points} 个颜色"
                )
            } else {
                "照片已就绪 · 导入调色文件后即可对比".to_owned()
            };
            let mut details = warnings.clone();
            if let Some(statistics) = grain_statistics
                && statistics.samples_per_node > 1
            {
                details.push(format!("每个颜色采样 {} 次", statistics.samples_per_node));
            }
            if smoothing.mode != SmoothingMode::Off {
                details.push(format!(
                    "平滑降噪：{}，修正 {} 个颜色",
                    smoothing.mode.display_name(),
                    smoothing.corrected_node_count
                ));
            }
            if let Some(consistency) = consistency {
                details.push(format_consistency_report(consistency));
            }
            (
                message,
                if source_grid.is_some() {
                    SUCCESS
                } else {
                    MUTED
                },
                Some(details.join("\n")),
            )
        }
    };
    let label = ui.add_sized(
        [ui.available_width(), 18.0],
        egui::Label::new(RichText::new(&message).size(11.0).color(color)).truncate(),
    );
    if let Some(detail) = detail {
        label.on_hover_text(detail);
    }
}

fn fitted_image_rect(container: Rect, dimensions: Vec2, zoom: f32, pan: Vec2) -> Rect {
    let fit = (container.width() / dimensions.x)
        .min(container.height() / dimensions.y)
        .max(0.0001);
    let size = dimensions * fit * zoom;
    Rect::from_center_size(container.center() + pan, size)
}

fn paint_texture(ui: &egui::Ui, texture: TextureId, image_rect: Rect, clip: Rect) {
    ui.painter().with_clip_rect(clip).image(
        texture,
        image_rect,
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        Color32::WHITE,
    );
}

fn toolbar_button(ui: &mut egui::Ui, label: &str) -> bool {
    ui.add_sized(
        [36.0, 28.0],
        egui::Button::new(RichText::new(label).size(10.5).color(MUTED))
            .fill(Color32::from_white_alpha(5))
            .stroke(Stroke::new(1.0, Color32::from_white_alpha(14)))
            .corner_radius(6),
    )
    .clicked()
}

fn load_bitmap_texture(
    context: &egui::Context,
    name: &str,
    bitmap: &PreviewBitmap,
) -> TextureHandle {
    let image = egui::ColorImage::from_rgba_premultiplied(
        [bitmap.width, bitmap.height],
        bitmap.rgba.as_slice(),
    );
    context.load_texture(name, image, egui::TextureOptions::LINEAR)
}

fn thumbnail_preview(source: &PreviewBitmap, max_edge: u32) -> Option<PreviewBitmap> {
    let rgba = image::RgbaImage::from_raw(
        source.width.try_into().ok()?,
        source.height.try_into().ok()?,
        source.rgba.clone(),
    )?;
    let thumbnail = image::imageops::thumbnail(&rgba, max_edge, max_edge);
    Some(PreviewBitmap {
        width: thumbnail.width() as usize,
        height: thumbnail.height() as usize,
        rgba: thumbnail.into_raw(),
        warnings: source.warnings.clone(),
    })
}

fn photo_source_image(source: PreviewBitmap) -> PhotoSourceImage {
    let mut rgba = source.rgba;
    unpremultiply_rgba(&mut rgba);
    PhotoSourceImage {
        width: source.width as u32,
        height: source.height as u32,
        rgba,
    }
}

fn render_status(ui: &mut egui::Ui, status: &AppStatus) {
    match status {
        AppStatus::Ready => {}
        AppStatus::Working(message) => {
            banner_frame(WARNING).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new(*message).size(12.0).color(TEXT));
                });
            });
        }
        AppStatus::Error(message) => {
            banner_frame(ERROR).show(ui, |ui| {
                ui.horizontal(|ui| {
                    status_dot(ui, ERROR);
                    ui.add_space(COMPACT_GAP);
                    ui.label(RichText::new("处理失败").size(13.0).strong().color(ERROR));
                });
                ui.add_space(COMPACT_GAP);
                ui.label(RichText::new(message).size(12.0).color(TEXT));
            });
        }
        AppStatus::Success {
            headline,
            details,
            path,
        } => {
            banner_frame(SUCCESS).show(ui, |ui| {
                ui.horizontal(|ui| {
                    status_dot(ui, SUCCESS);
                    ui.add_space(COMPACT_GAP);
                    ui.label(RichText::new(headline).size(13.0).strong().color(SUCCESS));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(FILE_MANAGER_LABEL).size(11.5).color(TEXT),
                                )
                                .fill(Color32::from_white_alpha(6))
                                .stroke(Stroke::new(1.0, Color32::from_white_alpha(18)))
                                .corner_radius(7),
                            )
                            .clicked()
                        {
                            let _ = reveal_file(path);
                        }
                    });
                });
                ui.add_space(COMPACT_GAP);
                for detail in details.iter().take(2) {
                    let color = if detail.starts_with('⚠') {
                        WARNING
                    } else {
                        MUTED
                    };
                    ui.label(RichText::new(detail).size(12.0).color(color));
                }
                if details.len() > 2 {
                    egui::CollapsingHeader::new(
                        RichText::new("查看检查详情").size(11.0).color(MUTED),
                    )
                    .id_salt("operation-details")
                    .show(ui, |ui| {
                        for detail in details.iter().skip(2) {
                            let color = if detail.starts_with('⚠') {
                                WARNING
                            } else {
                                MUTED
                            };
                            ui.label(RichText::new(detail).size(11.0).color(color));
                        }
                    });
                }
                let path_text = path.display().to_string();
                ui.add(
                    egui::Label::new(RichText::new(&path_text).size(10.5).color(FAINT)).truncate(),
                )
                .on_hover_text(path_text);
            });
        }
    }
}

fn banner_frame(color: Color32) -> Frame {
    Frame::new()
        .fill(tint(color, 10))
        .corner_radius(10)
        .stroke(Stroke::new(1.0, tint(color, 40)))
        .inner_margin(egui::Margin::symmetric(14, 12))
}

fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(8.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.0, color);
}

fn render_safety_popup(context: &egui::Context, app: &mut LumixApp) {
    if !app.show_safety_guide {
        return;
    }
    let mut open = app.show_safety_guide;
    egui::Window::new("调色与导回注意事项")
        .id(egui::Id::new("color-grading-safety-guide"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(520.0)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .frame(
            Frame::window(context.style_of(egui::Theme::Dark).as_ref())
                .fill(SURFACE_RAISED)
                .corner_radius(14)
                .stroke(Stroke::new(1.0, tint(ACCENT, 85)))
                .inner_margin(egui::Margin::symmetric(22, 18)),
        )
        .show(context, |ui| {
            ui.set_min_width(476.0);
            ui.label(
                RichText::new("按下面的方法保存，能减少调色结果与 LUT 不一致的情况。")
                    .size(12.0)
                    .color(WARNING),
            );
            add_vertical_gap(ui, FORM_GAP);
            for line in [
                "1. 可选择自己的参考照片；如果未选择，保存母版时会自动使用内置样片。之后在调色软件中编辑整张母版。".to_owned(),
                "2. 调色后另存为新文件，保持画布尺寸与左右位置不变，并使用 sRGB。推荐 16 位 PNG 或 TIFF。".to_owned(),
                "3. 全局颜色、曲线、曝光、对比度、HSL 等调整适合转换为 LUT。".to_owned(),
                "4. 裁切、几何、锐化、降噪、纹理、清晰度、暗角、蒙版和局部调整无法可靠地转换为 LUT；如使用了这些效果，结果可能不同。".to_owned(),
                "5. 无法关闭颗粒时，可展开“单独生成中性图”并选择抗颗粒模式；它主要减弱随机颗粒，不能消除固定纹理。".to_owned(),
                "6. 也可导回 8 位图或 JPEG，但压缩和较低色深造成的损失无法完全恢复。".to_owned(),
                "7. 关闭应用后若要继续处理已有母版，请在右侧预览中重新选择制作时用的参考照片；内置样片也可直接恢复。".to_owned(),
            ] {
                ui.label(RichText::new(line).size(12.5).color(TEXT));
                ui.add_space(CONTROL_GAP);
            }
            ui.add_space(CONTROL_GAP);
            ui.label(
                RichText::new("所有文件仅在本机处理，不会上传。")
                    .size(11.0)
                    .color(FAINT),
            );
        });
    app.show_safety_guide = open;
}

fn render_inconsistency_modal(context: &egui::Context, app: &mut LumixApp) {
    let Some((input_path, report)) = app
        .pending_inconsistency
        .as_ref()
        .map(|pending| (pending.input_path.clone(), pending.report.clone()))
    else {
        return;
    };

    #[derive(Clone, Copy)]
    enum Choice {
        Force,
        Replace,
    }
    let mut choice = None;
    egui::Modal::new(egui::Id::new("photo-master-consistency-warning"))
        .backdrop_color(Color32::from_black_alpha(190))
        .frame(
            Frame::window(context.style_of(egui::Theme::Dark).as_ref())
                .fill(SURFACE_RAISED)
                .corner_radius(14)
                .stroke(Stroke::new(1.0, tint(WARNING, 150)))
                .inner_margin(egui::Margin::symmetric(22, 18)),
        )
        .show(context, |ui| {
            ui.set_max_width(620.0);
            ui.label(
                RichText::new("这张调色图可能不是当前参考照片对应的母版")
                    .size(18.0)
                    .strong()
                    .color(WARNING),
            );
            add_vertical_gap(ui, CONTROL_GAP);
            ui.label(
                RichText::new("照片内容或位置的检查结果有疑点。请核对下方参考照片和导入文件；也可能是调色造成的误报。确认后会返回主界面，仍需点击“导出 CUBE”。")
                    .size(12.0)
                    .color(TEXT),
            );
            add_vertical_gap(ui, CONTROL_GAP);
            ui.horizontal(|ui| {
                if let Some(texture) = &app.master_source_texture {
                    let source_size = texture.size_vec2();
                    let scale = (360.0 / source_size.x)
                        .min(240.0 / source_size.y)
                        .min(1.0);
                    ui.add(
                        egui::Image::new((texture.id(), source_size * scale))
                            .bg_fill(Color32::BLACK),
                    );
                }
                ui.vertical(|ui| {
                    let file_name = input_path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("当前导入文件");
                    ui.label(RichText::new(format!("回传文件：{file_name}"))
                        .size(11.5)
                        .color(MUTED));
                    add_vertical_gap(ui, COMPACT_GAP);
                    ui.label(
                        RichText::new(format_consistency_report(&report))
                            .size(11.0)
                            .color(TEXT),
                    );
                    add_vertical_gap(ui, COMPACT_GAP);
                    for warning in &report.warnings {
                        ui.label(RichText::new(format!("• {warning}"))
                            .size(10.5)
                            .color(WARNING));
                    }
                });
            });
            add_vertical_gap(ui, CONTROL_GAP);
            ui.horizontal(|ui| {
                if ui
                    .add(egui::Button::new("选择其他调色文件")
                        .fill(Color32::from_white_alpha(12))
                        .stroke(Stroke::new(1.0, Color32::from_white_alpha(32))))
                    .clicked()
                {
                    choice = Some(Choice::Replace);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add(egui::Button::new(RichText::new("确认并返回导出").color(PRIMARY_TEXT))
                            .fill(WARNING)
                            .stroke(Stroke::new(1.0, tint(WARNING, 180))))
                        .clicked()
                    {
                        choice = Some(Choice::Force);
                    }
                });
            });
        });

    match choice {
        Some(Choice::Force) => {
            app.confirmed_suspect_input = Some(input_path);
            app.pending_inconsistency = None;
        }
        Some(Choice::Replace) => app.choose_input(context),
        None => {}
    }
}

fn render_license_popup(context: &egui::Context, app: &mut LumixApp) {
    if !app.show_license_guide {
        return;
    }
    let mut open = app.show_license_guide;
    egui::Window::new("许可与第三方声明")
        .id(egui::Id::new("license-guide"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(Vec2::new(680.0, 520.0))
        .min_size(Vec2::new(460.0, 320.0))
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .frame(
            Frame::window(context.style_of(egui::Theme::Dark).as_ref())
                .fill(SURFACE_RAISED)
                .corner_radius(14)
                .stroke(Stroke::new(1.0, Color32::from_white_alpha(28)))
                .inner_margin(egui::Margin::symmetric(20, 16)),
        )
        .show(context, |ui| {
            ui.label(
                RichText::new("许可文本已内嵌在程序中，单独分发 EXE 时仍可随时查看。")
                    .size(11.5)
                    .color(MUTED),
            );
            ui.add_space(CONTROL_GAP);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (title, contents, open_by_default) in [
                        ("LUMIX LUT Maker · MIT License", PROJECT_LICENSE_TEXT, true),
                        ("第三方声明", THIRD_PARTY_TEXT, true),
                        (
                            "lut-utility · MIT License · Christian Schrinner",
                            LUT_UTILITY_LICENSE_TEXT,
                            false,
                        ),
                        (
                            "Noto Sans CJK · SIL Open Font License 1.1",
                            NOTO_OFL_TEXT,
                            false,
                        ),
                    ] {
                        egui::CollapsingHeader::new(
                            RichText::new(title).size(12.5).strong().color(TEXT),
                        )
                        .default_open(open_by_default)
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(contents).size(10.5).monospace().color(MUTED),
                                )
                                .selectable(true)
                                .wrap(),
                            );
                        });
                        ui.add_space(CONTROL_GAP);
                    }
                });
        });
    app.show_license_guide = open;
}

fn safety_guide_button(ui: &mut egui::Ui) -> bool {
    ui.add_sized(
        [ui.available_width(), SAFETY_BUTTON_HEIGHT],
        egui::Button::new(
            RichText::new("调色与导回说明")
                .size(14.0)
                .strong()
                .color(PRIMARY_TEXT),
        )
        .fill(ACCENT)
        .stroke(Stroke::new(1.0, Color32::from_rgb(255, 199, 125)))
        .corner_radius(10),
    )
    .on_hover_text("查看母版调色、保存和导回的注意事项")
    .clicked()
}

fn paint_background(ui: &egui::Ui, rect: Rect) {
    ui.painter().rect_filled(rect, 0, BG);
}

fn configure_fonts(context: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "noto-sans-sc".to_owned(),
        FontData::from_static(CJK_FONT_BYTES).into(),
    );
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .push("noto-sans-sc".to_owned());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .push("noto-sans-sc".to_owned());
    context.set_fonts(fonts);
}

const CJK_FONT_BYTES: &[u8] = include_bytes!("../assets/fonts/NotoSansCJKsc-Regular.otf");
const PROJECT_LICENSE_TEXT: &str = include_str!("../LICENSE");
const THIRD_PARTY_TEXT: &str = include_str!("../THIRD_PARTY_NOTICES.md");
const LUT_UTILITY_LICENSE_TEXT: &str = include_str!("../licenses/lut-utility-MIT.txt");
const NOTO_OFL_TEXT: &str = include_str!("../assets/fonts/OFL.txt");

fn configure_style(context: &egui::Context) {
    context.set_theme(egui::Theme::Dark);
    let mut style = (*context.style_of(egui::Theme::Dark)).clone();
    style.visuals.window_fill = BG;
    style.visuals.panel_fill = BG;
    style.visuals.extreme_bg_color = SURFACE_RAISED;
    style.visuals.selection.bg_fill = tint(ACCENT, 60);
    style.visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    style.visuals.widgets.inactive.bg_fill = Color32::from_white_alpha(5);
    style.visuals.widgets.inactive.weak_bg_fill = Color32::from_white_alpha(5);
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_white_alpha(14));
    style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, MUTED);
    style.visuals.widgets.hovered.bg_fill = Color32::from_white_alpha(9);
    style.visuals.widgets.hovered.weak_bg_fill = Color32::from_white_alpha(9);
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_white_alpha(24));
    style.visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    style.visuals.widgets.active.bg_fill = Color32::from_white_alpha(14);
    style.visuals.widgets.active.weak_bg_fill = Color32::from_white_alpha(14);
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.0, Color32::from_white_alpha(40));
    style.visuals.widgets.inactive.corner_radius = 8.into();
    style.visuals.widgets.hovered.corner_radius = 8.into();
    style.visuals.widgets.active.corner_radius = 8.into();
    style.spacing.item_spacing = Vec2::new(8.0, 6.0);
    style.spacing.button_padding = Vec2::new(12.0, 8.0);
    style
        .text_styles
        .insert(TextStyle::Body, FontId::proportional(13.0));
    style
        .text_styles
        .insert(TextStyle::Button, FontId::proportional(13.0));
    context.set_style_of(egui::Theme::Dark, style);
}

fn is_supported_input(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "tif" | "tiff" | "png" | "jpg" | "jpeg" | "cube"
            )
        })
        .unwrap_or(false)
}

fn materialize_builtin_preview_photo(index: usize) -> Result<PathBuf, String> {
    let directory = std::env::temp_dir().join(format!(
        "lumix-lut-maker-{}-preview",
        env!("CARGO_PKG_VERSION")
    ));
    materialize_builtin_preview_photo_in(index, &directory)
}

fn materialize_builtin_preview_photo_in(index: usize, directory: &Path) -> Result<PathBuf, String> {
    let photo = BUILT_IN_PREVIEW_PHOTOS
        .get(index)
        .ok_or_else(|| "内置照片编号无效。".to_owned())?;
    std::fs::create_dir_all(directory).map_err(|error| format!("无法创建预览缓存目录：{error}"))?;
    let path = directory.join(photo.cache_name);
    let is_current = std::fs::read(&path)
        .map(|existing| existing == photo.bytes)
        .unwrap_or(false);
    if !is_current {
        std::fs::write(&path, photo.bytes).map_err(|error| format!("无法写入预览缓存：{error}"))?;
    }
    Ok(path)
}

fn builtin_photo_canvas_size(index: usize) -> Result<u32, String> {
    let photo = BUILT_IN_PREVIEW_PHOTOS
        .get(index)
        .ok_or_else(|| "内置照片编号无效。".to_owned())?;
    let reader = image::ImageReader::new(std::io::Cursor::new(photo.bytes))
        .with_guessed_format()
        .map_err(|error| format!("无法识别内置样片格式：{error}"))?;
    let (width, height) = reader
        .into_dimensions()
        .map_err(|error| format!("无法读取内置样片尺寸：{error}"))?;
    photo_master_layout(width, height)
        .map(|layout| layout.canvas_size)
        .map_err(|error| error.to_string())
}

fn extension_is(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| {
            let value = value.to_ascii_lowercase();
            extensions.contains(&value.as_str())
        })
}

fn is_cube(path: &Path) -> bool {
    extension_is(path, &["cube"])
}

fn is_jpeg(path: &Path) -> bool {
    extension_is(path, &["jpg", "jpeg"])
}

fn is_png_or_tiff(path: &Path) -> bool {
    extension_is(path, &["png", "tif", "tiff"])
}

fn ensure_extension(mut path: PathBuf, extension: &str) -> PathBuf {
    if path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case(extension))
        != Some(true)
    {
        path.set_extension(extension);
    }
    path
}

fn suggested_title(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn safe_filename_stem(title: &str) -> String {
    let stem: String = title
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
                )
            {
                '_'
            } else {
                character
            }
        })
        .collect();
    let stem = stem.trim().trim_end_matches(['.', ' ']);
    if stem.is_empty() {
        "LUMIXLUT".to_owned()
    } else if is_windows_reserved_filename(stem) {
        format!("_{stem}")
    } else {
        stem.to_owned()
    }
}

fn is_windows_reserved_filename(stem: &str) -> bool {
    let basename = stem
        .split('.')
        .next()
        .unwrap_or(stem)
        .trim_end_matches(['.', ' '])
        .to_ascii_uppercase();
    matches!(basename.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || basename
            .strip_prefix("COM")
            .or_else(|| basename.strip_prefix("LPT"))
            .is_some_and(|suffix| suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::Path;

    use image::{ImageDecoder, ImageReader};

    use super::{
        BUILT_IN_PREVIEW_PHOTOS, CJK_FONT_BYTES, LUT_UTILITY_LICENSE_TEXT, NOTO_OFL_TEXT,
        PROJECT_LICENSE_TEXT, THIRD_PARTY_TEXT, materialize_builtin_preview_photo_in,
        safe_filename_stem, suggested_title,
    };

    #[test]
    fn embedded_ui_font_contains_chinese_interface_glyphs() {
        let face = ttf_parser::Face::parse(CJK_FONT_BYTES, 0)
            .expect("embedded UI font must be valid OpenType");

        for character in "生成中性图转换相机通用格式采样放大警告选择文件校验完成错误说明".chars()
        {
            assert!(
                face.glyph_index(character).is_some(),
                "UI font is missing glyph {character}"
            );
        }
    }

    #[test]
    fn input_and_output_filenames_provide_safe_cube_titles() {
        assert_eq!(
            suggested_title(Path::new("/tmp/电影感 Look.tif")).as_deref(),
            Some("电影感 Look")
        );
        assert_eq!(safe_filename_stem("电影感 Look"), "电影感 Look");
        assert_eq!(safe_filename_stem("Look/A:*?"), "Look_A___");
        assert_eq!(safe_filename_stem("CON"), "_CON");
        assert_eq!(safe_filename_stem("CON .look"), "_CON .look");
        assert_eq!(safe_filename_stem("lpt9.look"), "_lpt9.look");
        assert_eq!(safe_filename_stem("COM10"), "COM10");
    }

    #[test]
    fn single_executable_contains_required_license_texts() {
        assert!(PROJECT_LICENSE_TEXT.contains("MIT License"));
        assert!(THIRD_PARTY_TEXT.contains("Noto Sans CJK SC"));
        let notices = THIRD_PARTY_TEXT
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(notices.contains("No permission for commercial reuse"));
        assert!(LUT_UTILITY_LICENSE_TEXT.contains("Copyright (c) 2025 Christian Schrinner"));
        assert!(NOTO_OFL_TEXT.contains("SIL OPEN FONT LICENSE Version 1.1"));
    }

    #[test]
    fn built_in_preview_images_are_valid_srgb_files() {
        for photo in &BUILT_IN_PREVIEW_PHOTOS {
            let reader = ImageReader::new(Cursor::new(photo.bytes))
                .with_guessed_format()
                .expect("embedded preview photo must have a recognized format");
            let mut decoder = reader
                .into_decoder()
                .expect("embedded preview photo must decode");
            let (width, height) = decoder.dimensions();
            assert_eq!(width.max(height), 2048);
            assert!(width > 0 && height > 0);
            assert!(
                decoder
                    .icc_profile()
                    .expect("ICC metadata must be readable")
                    .is_some(),
                "{} must contain an embedded sRGB ICC profile",
                photo.label
            );
        }
    }

    #[test]
    fn built_in_preview_photo_materialization_is_exact_and_repeatable() {
        let directory = tempfile::tempdir().unwrap();
        for (index, photo) in BUILT_IN_PREVIEW_PHOTOS.iter().enumerate() {
            let first = materialize_builtin_preview_photo_in(index, directory.path()).unwrap();
            let second = materialize_builtin_preview_photo_in(index, directory.path()).unwrap();
            assert_eq!(first, second);
            assert_eq!(std::fs::read(first).unwrap(), photo.bytes);
        }
    }
}
