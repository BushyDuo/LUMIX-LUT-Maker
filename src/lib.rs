//! Color-safe LUT generation and conversion core used by the desktop app.

mod cube;
mod error;
mod raster;

pub use cube::{
    ConsistencyMode, ConsistencyReport, ConversionReport, OUTPUT_GRID_PRESETS, PreparedLut,
    ReferenceMatch, ReferenceMatchReport, SmoothingMode, SmoothingReport, ValidationReport,
    convert_cube, convert_cube_with_photo_master, convert_cube_with_reference,
    convert_cube_with_smoothing, export_prepared_cube, prepare_lut, prepare_lut_with_photo_master,
    prepare_lut_with_reference, prepare_lut_with_smoothing, validate_cube,
};
pub use error::{LutError, Result};
pub use raster::{
    ColorSpace, GrainStatistics, NeutralFormat, NeutralSampling, NeutralSpec, PhotoMasterLayout,
    PhotoMasterReport, PhotoMasterSession, PhotoReferenceBuffer, PhotoReferenceReport,
    PhotoSourceImage, generate_neutral, generate_photo_master, photo_master_layout,
    prepare_photo_reference, srgb_icc_profile,
};
