use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum LutError {
    #[error("无法读取文件 {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("图像处理失败：{0}")]
    Image(#[from] image::ImageError),

    #[error("不支持的文件格式：{0}")]
    UnsupportedFormat(String),

    #[error("图像 LUT 输入无效：{0}")]
    InvalidRaster(String),

    #[error(
        "缺少参考照片：请在第 1 步选择制作这张母版时使用的原照片，应用会自动重新检查；无需重新生成母版。"
    )]
    MissingPhotoMasterBaseline,

    #[error("CUBE 文件无效：{0}")]
    InvalidCube(String),

    #[error("LUT 标题不能为空")]
    EmptyTitle,

    #[error("无法写入文件 {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub type Result<T> = std::result::Result<T, LutError>;

pub(crate) fn io_error(path: impl Into<PathBuf>, source: std::io::Error) -> LutError {
    LutError::Io {
        path: path.into(),
        source,
    }
}

pub(crate) fn write_error(path: impl Into<PathBuf>, source: std::io::Error) -> LutError {
    LutError::Write {
        path: path.into(),
        source,
    }
}
