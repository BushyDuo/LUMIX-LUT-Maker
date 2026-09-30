# Third-party notices

## Prior work

### `Skyfish1/lut-utility`

With thanks to Christian Schrinner, the author of
[`Skyfish1/lut-utility`](https://github.com/Skyfish1/lut-utility), for creating
the open-source LUT workflow that inspired this project. Its MIT-licensed
project established the practical workflow of generating a neutral LUT image,
grading it in an image editor, and converting the result to a `.cube` LUT. That
workflow was the original project base for LUMIX LUT Maker.

LUMIX LUT Maker is a separate desktop application. Its current implementation
was reorganized and rewritten around a tested library API, 16-bit raster
handling, robust sample aggregation, tetrahedral resampling, standard CUBE
validation, reference-photo calibration, cross-platform packaging, and a
native GUI. No endorsement by or affiliation with Christian Schrinner is
implied.

The upstream project is Copyright (c) 2025 Christian Schrinner and is licensed
under the MIT License. The required upstream notice is reproduced in
[`licenses/lut-utility-MIT.txt`](licenses/lut-utility-MIT.txt). The upstream
license applies to upstream material only; this repository's root `LICENSE`
covers LUMIX LUT Maker project material and does not relicense separately
identified third-party assets.

## ICC sRGB profile

`assets/color/sRGB2014.icc` is the unmodified `sRGB2014.icc` v2 profile
published by the International Color Consortium. It is embedded in this
application and copied unchanged into generated neutral TIFF/PNG files. The ICC
permits its profiles to be copied, distributed, embedded, made, used, and sold;
altered versions must not be represented as the original. The profile retains
its original creator and copyright tags.

Reviewed SHA-256: `384b832de3412066743b52a75ee906b6fb9fb8d9e09e936fc2c43223815c6e0a`.

- Profile and usage information: <https://registry.color.org/rgb-registry/srgbprofiles>
- ICC profile licensing terms: <https://registry.color.org/profile-library/>

## Noto Sans CJK SC

The unmodified `NotoSansCJKsc-Regular.otf` font is embedded as a Simplified
Chinese fallback. Font metadata states: Copyright 2014-2021 Adobe
(<http://www.adobe.com/>). Noto is a trademark of Google Inc. The font is
distributed under the SIL Open Font License 1.1. The full license is retained
at `assets/fonts/OFL.txt` in source distributions and as
`NotoSansCJK-OFL.txt` in Windows binary packages.

Reviewed SHA-256: `2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b`.

Upstream project: <https://github.com/notofonts/noto-cjk>

## ICC test profiles

The Display P3 and Adobe RGB-compatible profiles under `tests/fixtures/icc`
come from `saucecontrol/Compact-ICC-Profiles`. They are used only by automated
color-conversion tests and are dedicated to the public domain under CC0 1.0.
The accompanying `CC0-1.0.txt` is retained with the fixtures.

Upstream project: <https://github.com/saucecontrol/Compact-ICC-Profiles>

## Built-in preview photographs

`assets/default-preview/IMG_0779-preview.jpg` is a 2048-pixel sRGB JPEG
derivative of `IMG_0779.HEIC`, supplied in the project folder and included as
the application's default preview image. The project owner has confirmed
authorization to distribute this derivative in the public repository and
application. This image asset is excluded from the project's MIT license. No
permission for commercial reuse or unrelated standalone reuse is granted by
bundling it with the application. Copyright and other rights in the constituent
photographs and depicted people remain with their respective rights holders.

The JPEG contains the same image pixels as the supplied derivative, with the
unmodified sRGB2014 profile embedded so color-managed readers and automated
checks see the intended color space.

Reviewed derivative SHA-256: `edbfd436f39f74b7919f968a9056109289063d9dbcbdc7370af5f8338667ab79`.

The mathematical `LUT_Color_Test_Chart.png` diagnostic image is original
project material distributed under the project MIT License. It contains no
third-party chart artwork or proprietary measured patch data. Reviewed
SHA-256: `37b48ccd1e7d11e6def5ad934598cd0cbcdb80c1f5573cc0d01012838743f627`.

## Platform APIs and Rust dependencies

- macOS photo preview uses Apple's system CoreFoundation, CoreGraphics,
  ColorSync, and ImageIO frameworks through the MIT-licensed
  `objc2-core-foundation`, `objc2-core-graphics`, and `objc2-image-io` crates.
- Windows photo preview uses the Windows Imaging Component and COM system APIs
  through the dual MIT/Apache-2.0 licensed `windows` crate. No WIC binaries are
  redistributed.
- Windows executable resources are produced by the MIT-licensed `winresource`
  crate.
- Parallel pixel processing uses the dual MIT/Apache-2.0 licensed `rayon`
  crate.
- Image decoding and encoding uses the dual MIT/Apache-2.0 licensed `image`
  crate.

All other Rust dependencies retain their respective licenses. See `Cargo.lock`
and upstream crate metadata for the exact dependency set used by a build.

## Single-file Windows distribution

The Windows executable embeds the application fonts, color profile, icons,
manifest, project license, third-party notices, and full Noto OFL text. The
license texts remain available from the application's “许可说明” window when
the executable is distributed without sidecar files.
