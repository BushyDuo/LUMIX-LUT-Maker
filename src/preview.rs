use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use lumix_lut::PreparedLut;
use rayon::prelude::*;

pub const MAX_PREVIEW_EDGE: usize = 2048;
pub const MAX_MASTER_EDGE: usize = 4096;

pub fn decode_preview_photo(path: &Path) -> Result<PreviewBitmap, String> {
    decode_photo_with_limit(path, MAX_PREVIEW_EDGE)
}

pub fn decode_photo_for_master(path: &Path) -> Result<PreviewBitmap, String> {
    decode_photo_with_limit(path, MAX_MASTER_EDGE)
}

#[derive(Debug, Clone)]
pub struct PreviewBitmap {
    pub width: usize,
    pub height: usize,
    /// Premultiplied sRGB RGBA8 pixels, stored from the top row to the bottom row.
    pub rgba: Vec<u8>,
    pub warnings: Vec<String>,
}

impl PreviewBitmap {
    pub fn is_valid(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.rgba.len() == self.width.saturating_mul(self.height).saturating_mul(4)
    }
}

/// Applies a LUT to a premultiplied RGBA bitmap. RGB values are sampled directly
/// as sRGB-encoded values; no additional gamma conversion is performed.
pub fn apply_lut(
    source: &PreviewBitmap,
    lut: &PreparedLut,
    cancellation: Option<(&AtomicU64, u64)>,
) -> Option<PreviewBitmap> {
    if !source.is_valid() {
        return None;
    }
    if cancellation
        .is_some_and(|(generation, expected)| generation.load(Ordering::Acquire) != expected)
    {
        return None;
    }

    let mut rgba = source.rgba.clone();
    rgba.par_chunks_mut(source.width * 4).for_each(|row| {
        if cancellation
            .is_some_and(|(generation, expected)| generation.load(Ordering::Relaxed) != expected)
        {
            return;
        }
        for pixel in row.as_chunks_mut::<4>().0 {
            let alpha = pixel[3];
            if alpha == 0 {
                pixel[0] = 0;
                pixel[1] = 0;
                pixel[2] = 0;
                continue;
            }

            let alpha_f = f32::from(alpha) / 255.0;
            let input = [
                (f32::from(pixel[0]) / 255.0 / alpha_f).clamp(0.0, 1.0),
                (f32::from(pixel[1]) / 255.0 / alpha_f).clamp(0.0, 1.0),
                (f32::from(pixel[2]) / 255.0 / alpha_f).clamp(0.0, 1.0),
            ];
            let output = lut.sample_tetrahedral(input);
            for channel in 0..3 {
                pixel[channel] = (output[channel].clamp(0.0, 1.0) * alpha_f * 255.0).round() as u8;
            }
        }
    });

    if cancellation
        .is_some_and(|(generation, expected)| generation.load(Ordering::Acquire) != expected)
    {
        return None;
    }

    Some(PreviewBitmap {
        width: source.width,
        height: source.height,
        rgba,
        warnings: source.warnings.clone(),
    })
}

#[cfg(target_os = "macos")]
fn decode_photo_with_limit(path: &Path, max_edge: usize) -> Result<PreviewBitmap, String> {
    use std::ffi::c_void;

    use objc2_core_foundation::{
        CFBoolean, CFDictionary, CFNumber, CFString, CFType, CFURL, CGPoint, CGRect, CGSize,
    };
    use objc2_core_graphics::{
        CGBitmapContextCreate, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo,
        CGImageByteOrderInfo, kCGColorSpaceSRGB,
    };
    use objc2_image_io::{
        CGImageSource, kCGImagePropertyProfileName, kCGImageSourceCreateThumbnailFromImageAlways,
        kCGImageSourceCreateThumbnailWithTransform, kCGImageSourceThumbnailMaxPixelSize,
    };

    if !is_supported_photo(path) {
        return Err("照片预览支持 HEIC/HEIF、JPEG、PNG 和 TIFF。".to_owned());
    }
    let url = CFURL::from_file_path(path)
        .ok_or_else(|| format!("无法读取照片路径：{}", path.display()))?;
    // SAFETY: The options dictionary is absent, so there is no generic type mismatch.
    let source = unsafe { CGImageSource::with_url(&url, None) }
        .ok_or_else(|| "ImageIO 无法打开照片，文件可能损坏或格式不受支持。".to_owned())?;
    // SAFETY: The source is a valid ImageIO image source and index zero is checked below.
    if unsafe { source.count() } == 0 {
        return Err("照片中没有可解码的图像。".to_owned());
    }

    // SAFETY: ImageIO returns a CFDictionary whose documented keys are CFString values.
    let has_icc = unsafe { source.properties_at_index(0, None) }.is_some_and(|properties| {
        let typed: &CFDictionary<CFString, CFType> = unsafe { properties.cast_unchecked() };
        // SAFETY: This ImageIO constant is initialized by the linked framework.
        typed.contains_key(unsafe { kCGImagePropertyProfileName })
    });

    let max_size = CFNumber::new_isize(max_edge as isize);
    // SAFETY: These framework constants are initialized by ImageIO.
    let thumbnail_always = unsafe { kCGImageSourceCreateThumbnailFromImageAlways };
    // SAFETY: These framework constants are initialized by ImageIO.
    let apply_transform = unsafe { kCGImageSourceCreateThumbnailWithTransform };
    // SAFETY: These framework constants are initialized by ImageIO.
    let maximum_size = unsafe { kCGImageSourceThumbnailMaxPixelSize };
    let options = CFDictionary::<CFType, CFType>::from_slices(
        &[
            thumbnail_always.as_ref(),
            apply_transform.as_ref(),
            maximum_size.as_ref(),
        ],
        &[
            CFBoolean::new(true).as_ref(),
            CFBoolean::new(true).as_ref(),
            max_size.as_ref(),
        ],
    );
    // SAFETY: All option keys and values use the types documented by ImageIO.
    let image = unsafe { source.thumbnail_at_index(0, Some(options.as_ref())) }
        .ok_or_else(|| "ImageIO 无法生成照片缩略图，文件可能损坏。".to_owned())?;
    let width = CGImage::width(Some(&image));
    let height = CGImage::height(Some(&image));
    if width == 0 || height == 0 || width.max(height) > max_edge {
        return Err("照片缩略图尺寸无效。".to_owned());
    }

    // SAFETY: This CoreGraphics constant is initialized by the linked framework.
    let srgb_name = unsafe { kCGColorSpaceSRGB };
    let color_space = CGColorSpace::with_name(Some(srgb_name))
        .ok_or_else(|| "无法创建 sRGB 预览色彩空间。".to_owned())?;
    let bytes_per_row = width
        .checked_mul(4)
        .ok_or_else(|| "照片尺寸过大。".to_owned())?;
    let mut rgba = vec![0_u8; bytes_per_row * height];
    let bitmap_info = CGImageAlphaInfo::PremultipliedLast.0 | CGImageByteOrderInfo::Order32Big.0;
    // SAFETY: `rgba` remains allocated and immovable until the context is dropped below.
    // Its length exactly matches the supplied dimensions and row stride.
    let bitmap = unsafe {
        CGBitmapContextCreate(
            rgba.as_mut_ptr().cast::<c_void>(),
            width,
            height,
            8,
            bytes_per_row,
            Some(&color_space),
            bitmap_info,
        )
    }
    .ok_or_else(|| "无法创建照片预览缓冲区。".to_owned())?;

    // A bitmap context's memory rows are already returned in the top-to-bottom
    // order expected by egui when drawing a CGImage into the full bounds.
    CGContext::draw_image(
        Some(&bitmap),
        CGRect::new(CGPoint::ZERO, CGSize::new(width as f64, height as f64)),
        Some(&image),
    );
    drop(bitmap);

    let warnings = if has_icc {
        Vec::new()
    } else {
        vec!["照片未嵌入 ICC，预览按 sRGB 解释。".to_owned()]
    };
    Ok(PreviewBitmap {
        width,
        height,
        rgba,
        warnings,
    })
}

#[cfg(target_os = "windows")]
fn decode_photo_with_limit(path: &Path, max_edge: usize) -> Result<PreviewBitmap, String> {
    use std::iter;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    use image::{ImageDecoder, ImageReader};
    use lumix_lut::srgb_icc_profile;
    use windows::Win32::Foundation::GENERIC_READ;
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPRGBA, IWICBitmapSource,
        IWICImagingFactory, WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant,
        WICBitmapPaletteTypeCustom, WICBitmapTransformRotate0, WICDecodeMetadataCacheOnDemand,
    };
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows::core::{Interface, PCWSTR};

    if !is_supported_photo(path) {
        return Err("照片预览支持 HEIC/HEIF、JPEG、PNG 和 TIFF。".to_owned());
    }

    let heif_input = is_heif_photo(path);
    let (orientation, embedded_icc) = if heif_input {
        (None, None)
    } else {
        let reader = ImageReader::open(path)
            .map_err(|error| format!("无法打开照片：{error}"))?
            .with_guessed_format()
            .map_err(|error| format!("无法识别照片格式：{error}"))?;
        let mut metadata_decoder = reader
            .into_decoder()
            .map_err(|error| format!("无法读取照片信息：{error}"))?;
        let orientation = metadata_decoder
            .orientation()
            .map_err(|error| format!("无法读取照片方向：{error}"))?;
        let embedded_icc = metadata_decoder
            .icc_profile()
            .map_err(|error| format!("无法读取照片 ICC：{error}"))?;
        if let Some(profile) = &embedded_icc {
            validate_photo_icc(profile)?;
        }
        (Some(orientation), embedded_icc)
    };

    struct ComApartment;
    impl Drop for ComApartment {
        fn drop(&mut self) {
            // SAFETY: This guard is created only after CoInitializeEx succeeds on this thread.
            unsafe { CoUninitialize() };
        }
    }

    // SAFETY: The worker thread owns this COM apartment until the guard is dropped.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
        .ok()
        .map_err(|error| format!("无法初始化 Windows 图像服务：{error}"))?;
    let _apartment = ComApartment;

    // SAFETY: WIC is an in-process COM server and the returned interfaces remain
    // alive for the duration of this function.
    let factory: IWICImagingFactory =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
            .map_err(|error| format!("无法创建 Windows 图像解码器：{error}"))?;

    let wide_path: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect();
    // SAFETY: `wide_path` is NUL-terminated and remains alive during the call.
    let decoder = unsafe {
        factory.CreateDecoderFromFilename(
            PCWSTR(wide_path.as_ptr()),
            None,
            GENERIC_READ,
            WICDecodeMetadataCacheOnDemand,
        )
    }
    .map_err(|error| {
        if heif_input {
            format!("Windows 无法解码此 HEIC/HEIF 照片。请安装 Microsoft HEIF 图像扩展后重试；若文件使用 HEVC 编码，还需相应的视频扩展。原始错误：{error}")
        } else {
            format!("WIC 无法打开照片，文件可能损坏：{error}")
        }
    })?;
    // SAFETY: The decoder has at least one frame for a successfully opened supported photo.
    let frame = unsafe { decoder.GetFrame(0) }.map_err(|error| {
        if heif_input {
            format!("Windows HEIC/HEIF 解码器无法读取图像帧。请确认已安装 Microsoft HEIF 图像扩展；若照片使用 HEVC 编码，还需相应的视频扩展。原始错误：{error}")
        } else {
            format!("照片中没有可解码的图像：{error}")
        }
    })?;
    let (orientation, embedded_icc) = if heif_input {
        wic_heif_metadata(&frame)?
    } else {
        (orientation, embedded_icc)
    };
    let mut source: IWICBitmapSource = frame
        .cast()
        .map_err(|error| format!("无法读取照片像素：{error}"))?;

    let transform =
        windows_orientation(orientation.unwrap_or(image::metadata::Orientation::NoTransforms));
    if transform != WICBitmapTransformRotate0 {
        // SAFETY: The source and transform flags are valid WIC inputs.
        let rotator = unsafe { factory.CreateBitmapFlipRotator() }
            .map_err(|error| format!("无法创建照片方向转换：{error}"))?;
        unsafe { rotator.Initialize(&source, transform) }
            .map_err(|error| format!("无法应用照片方向：{error}"))?;
        source = rotator
            .cast()
            .map_err(|error| format!("无法读取旋转后的照片：{error}"))?;
    }

    let warnings = if let Some(profile) = embedded_icc {
        validate_photo_icc(&profile)?;
        // SAFETY: Both color contexts are initialized from complete in-memory ICC profiles.
        let source_context = unsafe { factory.CreateColorContext() }
            .map_err(|error| format!("无法创建照片 ICC 上下文：{error}"))?;
        unsafe { source_context.InitializeFromMemory(&profile) }
            .map_err(|error| format!("照片包含损坏或不受支持的 ICC：{error}"))?;
        let destination_context = unsafe { factory.CreateColorContext() }
            .map_err(|error| format!("无法创建 sRGB 上下文：{error}"))?;
        unsafe { destination_context.InitializeFromMemory(srgb_icc_profile()) }
            .map_err(|error| format!("无法载入内置 sRGB ICC：{error}"))?;
        let color_transform = unsafe { factory.CreateColorTransformer() }
            .map_err(|error| format!("无法创建 ICC 色彩转换：{error}"))?;
        unsafe {
            color_transform.Initialize(
                &source,
                &source_context,
                &destination_context,
                &GUID_WICPixelFormat32bppPRGBA,
            )
        }
        .map_err(|error| format!("无法将照片 ICC 颜色转换为 sRGB：{error}"))?;
        source = color_transform
            .cast()
            .map_err(|error| format!("无法读取 sRGB 照片：{error}"))?;
        Vec::new()
    } else {
        // With no profile, preserve encoded RGB values and explicitly treat them as sRGB.
        let converter = unsafe { factory.CreateFormatConverter() }
            .map_err(|error| format!("无法创建照片像素转换器：{error}"))?;
        unsafe {
            converter.Initialize(
                &source,
                &GUID_WICPixelFormat32bppPRGBA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
        }
        .map_err(|error| format!("无法转换照片像素格式：{error}"))?;
        source = converter
            .cast()
            .map_err(|error| format!("无法读取转换后的照片：{error}"))?;
        vec!["照片未嵌入 ICC，预览按 sRGB 解释。".to_owned()]
    };

    let (mut width, mut height) = wic_size(&source)?;
    if width == 0 || height == 0 {
        return Err("照片尺寸无效。".to_owned());
    }
    let longest = width.max(height);
    if longest > max_edge as u32 {
        let scale = max_edge as f64 / f64::from(longest);
        width = (f64::from(width) * scale).round().max(1.0) as u32;
        height = (f64::from(height) * scale).round().max(1.0) as u32;
        // SAFETY: The requested dimensions are non-zero and bounded by max_edge.
        let scaler = unsafe { factory.CreateBitmapScaler() }
            .map_err(|error| format!("无法创建照片缩放器：{error}"))?;
        unsafe { scaler.Initialize(&source, width, height, WICBitmapInterpolationModeFant) }
            .map_err(|error| format!("无法缩放预览照片：{error}"))?;
        source = scaler
            .cast()
            .map_err(|error| format!("无法读取缩放后的照片：{error}"))?;
    }

    let stride = width
        .checked_mul(4)
        .ok_or_else(|| "照片尺寸过大。".to_owned())?;
    let buffer_size = stride
        .checked_mul(height)
        .ok_or_else(|| "照片尺寸过大。".to_owned())?;
    let mut rgba = vec![0_u8; buffer_size as usize];
    // SAFETY: The buffer has exactly `stride * height` bytes and WIC writes RGBA8 rows.
    unsafe { source.CopyPixels(ptr::null(), stride, &mut rgba) }
        .map_err(|error| format!("无法复制照片像素：{error}"))?;

    Ok(PreviewBitmap {
        width: width as usize,
        height: height as usize,
        rgba,
        warnings,
    })
}

#[cfg(target_os = "windows")]
fn windows_orientation(
    orientation: image::metadata::Orientation,
) -> windows::Win32::Graphics::Imaging::WICBitmapTransformOptions {
    use image::metadata::Orientation;
    use windows::Win32::Graphics::Imaging::{
        WICBitmapTransformFlipHorizontal, WICBitmapTransformFlipVertical,
        WICBitmapTransformOptions, WICBitmapTransformRotate0, WICBitmapTransformRotate90,
        WICBitmapTransformRotate180, WICBitmapTransformRotate270,
    };

    match orientation {
        Orientation::NoTransforms => WICBitmapTransformRotate0,
        Orientation::Rotate90 => WICBitmapTransformRotate90,
        Orientation::Rotate180 => WICBitmapTransformRotate180,
        Orientation::Rotate270 => WICBitmapTransformRotate270,
        Orientation::FlipHorizontal => WICBitmapTransformFlipHorizontal,
        Orientation::FlipVertical => WICBitmapTransformFlipVertical,
        // WIC combines the horizontal flip before rotation; image::Orientation
        // names it after rotation. Reverse the angle for the mirrored cases.
        Orientation::Rotate90FlipH => WICBitmapTransformOptions(
            WICBitmapTransformRotate270.0 | WICBitmapTransformFlipHorizontal.0,
        ),
        Orientation::Rotate270FlipH => WICBitmapTransformOptions(
            WICBitmapTransformRotate90.0 | WICBitmapTransformFlipHorizontal.0,
        ),
    }
}

#[cfg(target_os = "windows")]
fn validate_photo_icc(profile: &[u8]) -> Result<(), String> {
    // Reject broken headers before WIC opens the frame; some codecs otherwise
    // report only a generic image error for malformed embedded profiles.
    if profile.len() < 132 || profile.get(36..40) != Some(b"acsp") {
        return Err("照片包含损坏的 ICC：配置文件头无效。".to_owned());
    }
    let declared = u32::from_be_bytes(profile[..4].try_into().unwrap()) as usize;
    let tags = u32::from_be_bytes(profile[128..132].try_into().unwrap()) as usize;
    if declared < 132 || declared > profile.len() || tags > (declared - 132) / 12 {
        return Err("照片包含损坏的 ICC：配置文件或标签表长度无效。".to_owned());
    }
    for entry in profile[132..132 + tags * 12].chunks_exact(12) {
        let offset = u32::from_be_bytes(entry[4..8].try_into().unwrap()) as usize;
        let size = u32::from_be_bytes(entry[8..12].try_into().unwrap()) as usize;
        if offset > declared || size > declared - offset {
            return Err("照片包含损坏的 ICC：标签数据越界。".to_owned());
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn wic_heif_metadata(
    frame: &windows::Win32::Graphics::Imaging::IWICBitmapFrameDecode,
) -> Result<(Option<image::metadata::Orientation>, Option<Vec<u8>>), String> {
    use std::iter;

    use windows::Win32::Graphics::Imaging::WICColorContextProfile;
    use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
    use windows::Win32::System::Variant::{VT_I2, VT_I4, VT_UI2, VT_UI4};
    use windows::core::PCWSTR;

    let orientation = unsafe { frame.GetMetadataQueryReader() }
        .ok()
        .and_then(|reader| {
            [
                "/ifd/{ushort=274}",
                "/exif/{ushort=274}",
                "/app1/ifd/{ushort=274}",
            ]
            .into_iter()
            .find_map(|query| {
                let query: Vec<u16> = query.encode_utf16().chain(iter::once(0)).collect();
                let mut value = PROPVARIANT::default();
                // SAFETY: The query is NUL-terminated and `value` is writable storage.
                unsafe { reader.GetMetadataByName(PCWSTR(query.as_ptr()), &mut value) }.ok()?;
                // SAFETY: WIC metadata for EXIF orientation is a 16- or 32-bit integer.
                let exif_value = unsafe {
                    let inner = &*value.Anonymous.Anonymous;
                    match inner.vt {
                        VT_UI2 => Some(inner.Anonymous.uiVal as u8),
                        VT_I2 => u8::try_from(inner.Anonymous.iVal).ok(),
                        VT_UI4 => u8::try_from(inner.Anonymous.ulVal).ok(),
                        VT_I4 => u8::try_from(inner.Anonymous.lVal).ok(),
                        _ => None,
                    }
                };
                // SAFETY: PropVariantClear releases any storage allocated by WIC.
                let _ = unsafe { PropVariantClear(&mut value) };
                exif_value.and_then(image::metadata::Orientation::from_exif)
            })
        });

    let mut contexts = [None];
    let mut actual_contexts = 0;
    // SAFETY: The one-element output array and count pointer are valid for this call.
    let has_contexts = unsafe { frame.GetColorContexts(&mut contexts, &mut actual_contexts) }
        .is_ok()
        && actual_contexts > 0;
    let profile = if has_contexts {
        if let Some(context) = contexts[0].as_ref() {
            // SAFETY: `context` is a valid WIC color context returned by the frame.
            if unsafe { context.GetType() }.ok() == Some(WICColorContextProfile) {
                // ICC profiles are normally small; cap allocation to avoid trusting corrupt
                // metadata for an unbounded buffer request.
                let mut bytes = vec![0; 4 * 1024 * 1024];
                let mut actual_bytes = 0;
                // SAFETY: The buffer is writable and its length matches the allocation.
                unsafe { context.GetProfileBytes(&mut bytes, &mut actual_bytes) }
                    .map_err(|error| format!("无法读取 HEIC/HEIF 的 ICC 配置：{error}"))?;
                let actual_bytes = usize::try_from(actual_bytes)
                    .map_err(|error| format!("HEIC/HEIF 的 ICC 配置长度无效：{error}"))?;
                if actual_bytes == 0 || actual_bytes > bytes.len() {
                    return Err("HEIC/HEIF 的 ICC 配置为空或超过安全读取上限。".to_owned());
                }
                bytes.truncate(actual_bytes);
                Some(bytes)
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    Ok((orientation, profile))
}

#[cfg(target_os = "windows")]
fn wic_size(
    source: &windows::Win32::Graphics::Imaging::IWICBitmapSource,
) -> Result<(u32, u32), String> {
    let mut width = 0;
    let mut height = 0;
    // SAFETY: Both pointers refer to live u32 values for the duration of the call.
    unsafe { source.GetSize(&mut width, &mut height) }
        .map_err(|error| format!("无法读取照片尺寸：{error}"))?;
    Ok((width, height))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn decode_photo_with_limit(path: &Path, max_edge: usize) -> Result<PreviewBitmap, String> {
    use image::{ImageDecoder, ImageReader};

    if !is_supported_photo(path) {
        return Err("照片预览支持 HEIC/HEIF、JPEG、PNG 和 TIFF。".to_owned());
    }
    let reader = ImageReader::open(path)
        .map_err(|error| error.to_string())?
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    let mut decoder = reader.into_decoder().map_err(|error| error.to_string())?;
    let orientation = decoder.orientation().map_err(|error| error.to_string())?;
    let has_icc = decoder
        .icc_profile()
        .map_err(|error| error.to_string())?
        .is_some();
    let mut image =
        image::DynamicImage::from_decoder(decoder).map_err(|error| error.to_string())?;
    image.apply_orientation(orientation);
    let image = image.thumbnail(max_edge as u32, max_edge as u32);
    let rgba = image.into_rgba8();
    let mut warnings = vec!["此平台的预览不执行 ICC 色彩转换。".to_owned()];
    if !has_icc {
        warnings.push("照片未嵌入 ICC，预览按 sRGB 解释。".to_owned());
    }
    Ok(PreviewBitmap {
        width: rgba.width() as usize,
        height: rgba.height() as usize,
        rgba: rgba.into_raw(),
        warnings,
    })
}

pub fn is_supported_photo(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "heic" | "heif" | "jpg" | "jpeg" | "png" | "tif" | "tiff"
            )
        })
}

pub fn is_heif_photo(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "heic" | "heif"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::fs::File;
    use std::io::BufWriter;
    use std::sync::atomic::AtomicU64;

    use image::codecs::png::PngEncoder;
    use image::metadata::Orientation;
    use image::{ExtendedColorType, ImageEncoder};
    use lumix_lut::prepare_lut;

    use super::*;

    #[test]
    fn photo_filter_accepts_heif_jpeg_and_png_case_insensitively() {
        for name in [
            "original.HEIC",
            "original.heif",
            "original.JPG",
            "original.jpeg",
            "original.PNG",
            "original.tiff",
        ] {
            assert!(is_supported_photo(std::path::Path::new(name)), "{name}");
        }
        assert!(is_heif_photo(std::path::Path::new("original.HEIC")));
        assert!(is_heif_photo(std::path::Path::new("original.heif")));
        assert!(!is_heif_photo(std::path::Path::new("original.jpg")));
        assert!(!is_supported_photo(std::path::Path::new("original.psd")));
    }

    #[test]
    fn identity_lut_preserves_opaque_and_transparent_pixels() {
        let directory = tempfile::tempdir().unwrap();
        let cube = directory.path().join("identity.cube");
        write_cube(&cube, |red, green, blue| [red, green, blue]);
        let lut = prepare_lut(&cube, 17).unwrap();
        let source = PreviewBitmap {
            width: 3,
            height: 1,
            rgba: vec![12, 97, 221, 255, 64, 32, 16, 128, 0, 0, 0, 0],
            warnings: Vec::new(),
        };
        let output = apply_lut(&source, &lut, None).unwrap();
        for (actual, expected) in output.rgba.iter().zip(&source.rgba) {
            assert!((i16::from(*actual) - i16::from(*expected)).abs() <= 1);
        }
    }

    #[test]
    fn channel_permutation_matches_expected_rgb_and_honors_cancellation() {
        let directory = tempfile::tempdir().unwrap();
        let cube = directory.path().join("permutation.cube");
        write_cube(&cube, |red, green, blue| [green, blue, red]);
        let lut = prepare_lut(&cube, 33).unwrap();
        let source = PreviewBitmap {
            width: 1,
            height: 1,
            rgba: vec![40, 120, 220, 255],
            warnings: Vec::new(),
        };
        assert_eq!(
            apply_lut(&source, &lut, None).unwrap().rgba,
            [120, 220, 40, 255]
        );

        let generation = AtomicU64::new(9);
        assert!(apply_lut(&source, &lut, Some((&generation, 8))).is_none());
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn platform_decoder_returns_top_down_rgba_and_caps_long_edge() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("orientation.png");
        let image = image::RgbaImage::from_raw(
            2,
            2,
            vec![
                255, 0, 0, 255, 0, 255, 0, 255, // top: red, green
                0, 0, 255, 255, 255, 255, 255, 255, // bottom: blue, white
            ],
        )
        .unwrap();
        image.save(&path).unwrap();

        let decoded = decode_preview_photo(&path).unwrap();
        assert_eq!((decoded.width, decoded.height), (2, 2));
        assert_eq!(decoded.rgba, image.into_raw());
        assert!(!decoded.warnings.is_empty());
        assert!(decoded.width.max(decoded.height) <= MAX_PREVIEW_EDGE);

        let wide_path = directory.path().join("wide.png");
        image::RgbaImage::new(2050, 2).save(&wide_path).unwrap();
        let wide = decode_preview_photo(&wide_path).unwrap();
        assert_eq!(wide.width, MAX_PREVIEW_EDGE);
        assert!(wide.height <= 2);
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn platform_decoder_applies_all_eight_exif_orientations() {
        let directory = tempfile::tempdir().unwrap();
        let original = image::RgbaImage::from_raw(
            3,
            2,
            vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, // top row
                255, 255, 0, 255, 0, 255, 255, 255, 255, 0, 255, 255, // bottom row
            ],
        )
        .unwrap();

        for exif in 1..=8 {
            let orientation = Orientation::from_exif(exif).unwrap();
            let path = directory.path().join(format!("orientation-{exif}.png"));
            write_oriented_png(&path, &original, orientation);

            let mut expected = image::DynamicImage::ImageRgba8(original.clone());
            expected.apply_orientation(orientation);
            let expected = expected.into_rgba8();
            let decoded = decode_preview_photo(&path).unwrap();
            assert_eq!(
                (decoded.width, decoded.height),
                (expected.width() as usize, expected.height() as usize),
                "EXIF orientation {exif} has incorrect dimensions"
            );
            assert_eq!(
                decoded.rgba,
                expected.into_raw(),
                "EXIF orientation {exif} has incorrect pixel order"
            );
        }
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn platform_decoder_preserves_alpha_as_premultiplied_rgba() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("alpha.png");
        let image = image::RgbaImage::from_raw(1, 1, vec![200, 100, 50, 128]).unwrap();
        image.save(&path).unwrap();

        let decoded = decode_preview_photo(&path).unwrap();
        assert_eq!((decoded.width, decoded.height), (1, 1));
        assert_eq!(decoded.rgba[3], 128);
        for (actual, expected) in decoded.rgba[..3].iter().zip([100_u8, 50, 25]) {
            assert!((i16::from(*actual) - i16::from(expected)).abs() <= 1);
        }
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn platform_decoder_converts_display_p3_and_adobe_rgb_to_srgb() {
        const DISPLAY_P3: &[u8] = include_bytes!("../tests/fixtures/icc/DisplayP3Compat-v4.icc");
        const ADOBE_RGB: &[u8] = include_bytes!("../tests/fixtures/icc/AdobeCompat-v2.icc");
        const SAMPLE: [u8; 3] = [126, 164, 91];
        const P3_TO_XYZ: [[f64; 3]; 3] = [
            [0.486_570_95, 0.265_667_69, 0.198_217_29],
            [0.228_974_56, 0.691_738_52, 0.079_286_91],
            [0.0, 0.045_113_38, 1.043_944_37],
        ];
        const ADOBE_TO_XYZ: [[f64; 3]; 3] = [
            [0.576_730_9, 0.185_554_0, 0.188_185_2],
            [0.297_376_9, 0.627_349_1, 0.075_274_1],
            [0.027_034_3, 0.070_687_2, 0.991_108_5],
        ];

        let directory = tempfile::tempdir().unwrap();
        for (name, profile, matrix, gamma) in [
            ("display-p3", DISPLAY_P3, P3_TO_XYZ, None),
            ("adobe-rgb", ADOBE_RGB, ADOBE_TO_XYZ, Some(2.199_218_75)),
        ] {
            let path = directory.path().join(format!("{name}.png"));
            write_profiled_png(&path, SAMPLE, profile);
            let decoded = decode_preview_photo(&path).unwrap();
            let expected = wide_gamut_to_srgb(SAMPLE, matrix, gamma);
            assert_eq!((decoded.width, decoded.height), (1, 1));
            assert_eq!(decoded.rgba[3], 255);
            for (actual, expected) in decoded.rgba[..3].iter().zip(expected) {
                assert!(
                    (i16::from(*actual) - i16::from(expected)).abs() <= 2,
                    "{name} conversion returned {:?}, expected {:?}",
                    &decoded.rgba[..3],
                    expected
                );
            }
            assert!(decoded.warnings.is_empty());
        }
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn platform_decoder_supports_jpeg_tiff_and_sixteen_bit_png() {
        let directory = tempfile::tempdir().unwrap();
        let rgb8 = image::RgbImage::from_raw(2, 1, vec![240, 30, 10, 20, 80, 220]).unwrap();
        for extension in ["jpg", "tiff"] {
            let path = directory.path().join(format!("photo.{extension}"));
            rgb8.save(&path).unwrap();
            let decoded = decode_preview_photo(&path).unwrap();
            assert_eq!((decoded.width, decoded.height), (2, 1));
        }

        let sixteen_bit = directory.path().join("sixteen-bit.png");
        let image = image::ImageBuffer::<image::Rgb<u16>, Vec<u16>>::from_raw(
            1,
            1,
            vec![32_896, 16_448, 65_535],
        )
        .unwrap();
        image.save(&sixteen_bit).unwrap();
        let decoded = decode_preview_photo(&sixteen_bit).unwrap();
        assert_eq!((decoded.width, decoded.height), (1, 1));
        for (actual, expected) in decoded.rgba[..3].iter().zip([128_u8, 64, 255]) {
            assert!((i16::from(*actual) - i16::from(expected)).abs() <= 1);
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn wic_rejects_a_damaged_embedded_icc_profile() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("damaged-icc.png");
        write_profiled_png(&path, [128, 128, 128], b"not an ICC profile");
        let error = decode_preview_photo(&path).unwrap_err();
        assert!(error.contains("ICC"), "Unexpected error: {error}");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn wic_icc_validation_accepts_bundled_profiles_and_rejects_truncated_tags() {
        for profile in [
            lumix_lut::srgb_icc_profile(),
            include_bytes!("../tests/fixtures/icc/DisplayP3Compat-v4.icc").as_slice(),
            include_bytes!("../tests/fixtures/icc/AdobeCompat-v2.icc").as_slice(),
        ] {
            validate_photo_icc(profile).unwrap();
        }
        assert!(validate_photo_icc(b"not an ICC profile").is_err());
        let mut broken = lumix_lut::srgb_icc_profile().to_vec();
        broken.truncate(132);
        assert!(validate_photo_icc(&broken).is_err());
        let mut broken = lumix_lut::srgb_icc_profile().to_vec();
        broken[136..140].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(validate_photo_icc(&broken).is_err());
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn platform_decoder_rejects_corrupt_and_unsupported_photos() {
        let directory = tempfile::tempdir().unwrap();
        let corrupt = directory.path().join("corrupt.jpg");
        fs::write(&corrupt, b"not a photograph").unwrap();
        assert!(decode_preview_photo(&corrupt).is_err());

        let unsupported = directory.path().join("photo.webp");
        fs::write(&unsupported, b"not supported").unwrap();
        assert!(decode_preview_photo(&unsupported).is_err());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn wic_orientation_mapping_covers_all_eight_exif_transforms() {
        use image::metadata::Orientation;

        let mut flags = vec![
            Orientation::NoTransforms,
            Orientation::Rotate90,
            Orientation::Rotate180,
            Orientation::Rotate270,
            Orientation::FlipHorizontal,
            Orientation::FlipVertical,
            Orientation::Rotate90FlipH,
            Orientation::Rotate270FlipH,
        ]
        .into_iter()
        .map(|orientation| windows_orientation(orientation).0)
        .collect::<Vec<_>>();
        flags.sort_unstable();
        flags.dedup();
        assert_eq!(flags.len(), 8);
    }

    fn write_cube(path: &Path, transform: impl Fn(f64, f64, f64) -> [f64; 3]) {
        let mut text = String::from(
            "TITLE \"Preview Test\"\nLUT_3D_SIZE 2\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 1 1 1\n",
        );
        for blue in 0..2 {
            for green in 0..2 {
                for red in 0..2 {
                    let value = transform(red as f64, green as f64, blue as f64);
                    text.push_str(&format!("{} {} {}\n", value[0], value[1], value[2]));
                }
            }
        }
        fs::write(path, text).unwrap();
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn write_oriented_png(path: &Path, image: &image::RgbaImage, orientation: Orientation) {
        let file = File::create(path).unwrap();
        let mut encoder = PngEncoder::new(BufWriter::new(file));
        encoder
            .set_exif_metadata(exif_orientation_chunk(orientation))
            .unwrap();
        encoder
            .write_image(
                image.as_raw(),
                image.width(),
                image.height(),
                ExtendedColorType::Rgba8,
            )
            .unwrap();
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn write_profiled_png(path: &Path, rgb: [u8; 3], profile: &[u8]) {
        let file = File::create(path).unwrap();
        let mut encoder = PngEncoder::new(BufWriter::new(file));
        encoder.set_icc_profile(profile.to_vec()).unwrap();
        encoder
            .write_image(&rgb, 1, 1, ExtendedColorType::Rgb8)
            .unwrap();
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn wide_gamut_to_srgb(
        rgb: [u8; 3],
        to_xyz: [[f64; 3]; 3],
        constant_gamma: Option<f64>,
    ) -> [u8; 3] {
        const XYZ_TO_SRGB: [[f64; 3]; 3] = [
            [3.240_454_2, -1.537_138_5, -0.498_531_4],
            [-0.969_266, 1.876_010_8, 0.041_556],
            [0.055_643_4, -0.204_025_9, 1.057_225_2],
        ];
        let encoded = rgb.map(|channel| f64::from(channel) / 255.0);
        let linear = encoded.map(|channel| match constant_gamma {
            Some(gamma) => channel.powf(gamma),
            None if channel <= 0.040_45 => channel / 12.92,
            None => ((channel + 0.055) / 1.055).powf(2.4),
        });
        let xyz = matrix_vector(to_xyz, linear);
        matrix_vector(XYZ_TO_SRGB, xyz).map(|channel| {
            let encoded = if channel <= 0.003_130_8 {
                channel * 12.92
            } else {
                1.055 * channel.max(0.0).powf(1.0 / 2.4) - 0.055
            };
            (encoded.clamp(0.0, 1.0) * 255.0).round() as u8
        })
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn matrix_vector(matrix: [[f64; 3]; 3], vector: [f64; 3]) -> [f64; 3] {
        matrix.map(|row| row[0] * vector[0] + row[1] * vector[1] + row[2] * vector[2])
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn exif_orientation_chunk(orientation: Orientation) -> Vec<u8> {
        vec![
            b'I',
            b'I',
            42,
            0, // little-endian TIFF header
            8,
            0,
            0,
            0, // first IFD offset
            1,
            0, // one IFD entry
            0x12,
            0x01, // orientation tag
            3,
            0, // SHORT
            1,
            0,
            0,
            0, // one value
            orientation.to_exif(),
            0,
            0,
            0, // inline u16 value and padding
            0,
            0,
            0,
            0, // no next IFD
        ]
    }
}
