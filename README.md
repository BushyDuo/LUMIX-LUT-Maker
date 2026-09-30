# LUMIX LUT Maker

离线 macOS / Windows 桌面工具。它将照片与 64-grid 中性图拼成校准母版，导回调色后的母版后自动拟合 LUT、诊断图像一致性，并输出经过校验的 17、25、33 或 64-grid `.cube`。所有图像处理都在本机完成。

## 使用流程

1. 可点击“选择参考照片”使用自己的照片；如果跳过，点击“用内置样片生成母版”即可直接制作模板。应用自动合成左右拼接图；保存对话框只用于选择母版位置，不需要手工拼图。默认输出 PNG，也可选 TIFF；应用按照片最长边自动选择 512、2048 或 4096 方形尺寸，并把完整 Neutral64 放在右侧。未调色母版不会自动进入导出栏。
2. 在任意支持 PNG/TIFF 的调色软件中处理母版，优先使用全局颜色、曲线、曝光、对比度和 HSL 调整。
3. 将整张母版原尺寸导出，保持 sRGB、画布位置与尺寸，不裁切、不缩放、不移动图像。优先 RGB16；RGB8 或 JPEG 也可导回，但量化与压缩损失无法完全恢复。
4. 把调色后另存的拼接母版拖回应用。应用会先检查文件，从右侧提取 LUT 网格，再用左侧照片和黑白渐变留边校准；检查通过后才可选择 CUBE 保存位置。预览与导出使用同一份处理结果。
5. 使用右侧内置的“样片 / 色彩测试”，或拖入自己的 HEIC/HEIF、JPEG/JPG、PNG、TIFF 照片，以分割、切换或双图模式检查效果。

照片母版尺寸档位：最长边 ≤512 使用 512/单样本，513–2048 使用 2048/16×重复采样，>2048 使用 4096/64×重复采样。照片完整适配到左侧方形区域，保持比例且不裁切；空白留边填入黑到白的灰阶渐变，也会参与校准，帮助覆盖照片未包含的明暗输入范围。参考照片的校准基准只保存在当前会话中；重启后若要处理已有母版，可重新导入自己的参考照片，或在右侧预览中选择“参考照片”恢复内置样片基准，无须再生成母版。软件会用尺寸和图像一致性检查帮助核对，但无法仅凭调色后的文件绝对证明参考照片的身份。备用的“单独生成中性图”仍可继续使用。

一致性诊断会自动综合留出像素 RGB 映射误差与 ΔE76、亮度归一化边缘 SSIM，以及 FAST/定向 BRIEF 特征匹配和 RANSAC 仿射偏移估计。发现映射或几何疑点、或检测置信度不足时，应用会弹出确认窗并显示原图缩略图；可选择其他调色文件，或确认使用当前文件后返回主界面点击导出。诊断不会自动配准或扭曲图像，也不会改变预览与导出使用的 LUT 节点。参考拟合只是在原照片上的全局 3D LUT 最佳近似，不会把局部调色变成可在所有照片上完全重现的映射；LUT 节点始终在 sRGB 编码值域采样。

不要对母版使用裁切、几何、锐化、降噪、纹理、局部蒙版或暗角；这些空间效果不能编码进 3D LUT。若软件无法关闭颗粒，可使用抗颗粒中性图减弱随机、近似零均值颗粒，但不能修复固定纹理或其他局部空间效果。应用内的“调色与导回说明”会显示完整提示。

## 抗颗粒中性图

- 普通模式生成 `Neutral64.png`（TIFF 可选）：512×512，每个 LUT 节点 1 个样本。
- 抗颗粒 16×生成 `Neutral64_Grain16.tif/png`：完整 Neutral64 平铺为 4×4，每节点 16 个样本，是推荐抗颗粒规格。
- 抗颗粒 64×生成 `Neutral64_Grain64.tif/png`：完整 Neutral64 平铺为 8×8，每节点 64 个样本，文件更大且处理更慢。

导回抗颗粒图时，应用会按每个 512×512 区块的相同坐标读取所有 RGB16 样本，在 sRGB 编码值域用 `f64` 进行自适应稳健聚合，再复用同一份结果进行预览、Grid 重采样、裁切和 CUBE 导出。每个节点先计算中位数与 MAD：分布正常时使用精度最高的算术平均，出现异常颗粒时以 `σ ≈ 1.4826 × MAD`、`k = 1.345` 做最多 5 次 Huber 迭代，收敛阈值为 `1e-8`；MAD 为零时退回算术平均。报告会显示每节点样本数、平均/P95/最大标准差、算术与 Huber 节点数及裁切样本数。

重复聚合只适合减弱随机、近似零均值的颗粒。固定纹理、暗角、光晕、局部对比度、锐化及其他空间效果仍无法正确转换。阴影/高光中可明确识别的裁切样本会被单独剔除并报告，应用不会尝试“恢复”已裁切数据；黑白端仍可能产生偏移。抗颗粒图必须保持 sRGB 和原始尺寸，任何缩放或裁切都会被拒绝。

## 平滑降噪与低精度补偿

- `关闭`：只使用单样本直读或重复采样聚合；普通中性图及 CUBE 默认使用此档。
- `轻度拟合`：只修正明显超出噪声范围的低可信度节点；检测到抗颗粒 16×或 64×输入时自动启用。
- `中度拟合`：供颗粒特别强的文件手动选择，仍受更严格的单节点最大修正幅度限制。

平滑只参考 RGB 三维网格中的成对相邻节点，以逐节点可信度加权二阶差分；全部修正同时从原始网格计算，不做普通邻居平均或迭代替换。恒等 LUT、曝光变化和颜色矩阵等仿射映射的二阶差分为零，因此不会被平滑改变。报告会显示平滑模式、修正节点数与最大偏移。

RGB8 输入会在每个 8-bit 量化区间内估计受限的亚码值；JPEG 会使用稍宽但有上限的误差窗口减弱 DCT、色度与量化伪影。两种补偿都只接受符合局部二阶趋势的微小残差，不修改黑白裁切端，也不会假装恢复已经丢失的色深或有损压缩信息。预览和最终 CUBE 始终复用同一份补偿、聚合与平滑后的 `PreparedLut`。

## Grid 与兼容性

- 17、25、33-grid：LUMIX 相机兼容预设。
- 64-grid：通用桌面软件或存档格式，允许导出，但不能导入 LUMIX 相机。
- 源 Grid 高于目标时使用四面体插值降采样。
- 源 Grid 低于目标时允许插值放大，但不会恢复源 LUT 已经缺失的颜色细节，应用会显示强警告。

## 支持的输入

- 应用生成布局的 RGB16/RGB8、sRGB N²×N TIFF/PNG/JPEG，以及精确 2048×2048 / 4096×4096 的抗颗粒重复布局。
- 当前会话生成的 1024×512、4096×2048 或 8192×4096 照片校准拼接母版；调色后可用 RGB16/RGB8 PNG/TIFF 或 JPEG 导回，且必须保持画布尺寸不变。
- RGB16/RGB8、带 sRGB ICC 的有效方形 HALD PNG；无 ICC 的 JPEG 会明确提示并按 sRGB 解释。
- `DOMAIN_MIN 0 0 0` / `DOMAIN_MAX 1 1 1` 的纯 3D `.cube`，尺寸 2–65。

不接受灰度、CMYK、带 Alpha、非 sRGB ICC 的 LUT 图像，也不接受 1D/3D 混合 LUT、非单位输入 Domain、数据量错误或含非有限数值的 CUBE。TIFF/PNG 缺少 ICC 仍会被拒绝；JPEG 缺少 ICC 时按 sRGB 解释并显示警告。

## 照片预览

- 应用内置 `IMG_0779` 的 2048 像素 sRGB 预览图作为默认样片，也可切换到原创的“色彩测试”诊断图。该照片拼图不属于项目 MIT 许可；未经各原作者许可，不得单独转载或用于商业用途。详见 [第三方声明](THIRD_PARTY_NOTICES.md#built-in-preview-photographs)。
- “色彩测试”包含灰阶与黑白位、RGB/CMY 通道渐变、色相与饱和度、立方体对角线和自然参考色块，用于发现偏色、通道交换、裁切、断阶及插值异常。生成器保留在 `examples/generate_color_test_chart.rs`，可复现根目录下的 `LUT_Color_Test_Chart.png`。
- 预览与 CUBE 导出复用同一个 `PreparedLut`：先按当前 17/25/33/64 Grid 四面体重采样并裁切，再应用到照片。
- 支持 HEIC/HEIF、JPEG/JPG、PNG、TIFF；macOS 使用 ImageIO / ColorSync，Windows 使用 WIC。两者都会应用 EXIF 方向，并把有效的嵌入 ICC 转换到 sRGB。
- Windows 解码 HEIC/HEIF 依赖系统 WIC HEIF 编解码扩展；若未安装，应用会明确提示。部分 HEVC 编码照片还需要系统 HEVC 视频扩展。JPG/PNG 不需要额外组件。
- 照片最长边在后台缩至 2048 像素，只影响屏幕预览，不修改原文件。
- 缺少 ICC 的照片按 sRGB 解释并显示提示；RGBA 图片会保留 Alpha。
- 可用滚轮缩放、拖动平移、双击或“适应”恢复窗口大小；双图模式共享缩放和平移。
- 预览照片不会影响 CUBE 的默认文件名，也不会被应用导出或写回磁盘。

## 输出保证

标准输出依次包含：

- `TITLE "…"`
- `LUT_3D_SIZE N`
- full-range `DOMAIN_MIN` / `DOMAIN_MAX`
- 恰好 N³ 行 RGB 数据，红色变化最快、六位小数、UTF-8/LF

输出不写入 `#LUMIXPHOTOSTYLE` 或其他厂商私有标签。超出 `[0,1]` 的输出值会被裁切，并在报告中显示数量和比例。

## 安装

### macOS 13+（Apple Silicon）

打开 `LUMIX LUT Maker.app`。个人构建没有 Developer ID 签名或公证；如果 macOS 阻止首次打开，可在 Finder 中按住 Control 点按应用并选择“打开”。Bundle Identifier 保持 `com.local.lumix33lutmaker`。

### Windows 10/11 x64

- `dist/0.7.4/LUMIX-LUT-Maker-0.7.4-windows-x64.exe`：单文件免安装版，下载后直接运行，不写入注册表、不创建快捷方式。
- 字体、ICC、图标、manifest 和许可文本均内嵌在 EXE 中；可从程序右上角的“许可说明”查看完整声明。
- Microsoft C 运行库采用静态链接，不要求用户另外安装 Visual C++ Redistributable。

Windows 产物目前未签名，首次运行可能显示 Microsoft Defender SmartScreen 提示。请只使用自己构建或可信发布页下载的文件。

## 开发与打包

通用检查：

```sh
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
```

macOS arm64：

```sh
cargo run
sh scripts/package-macos.sh
```

打包完成后，`.app` 与 `.dmg` 会收集到 `dist/<版本号>/`，每个版本独立存放，旧版本不会被覆盖。`target/` 仅作编译缓存，可在不需要保留中间文件时清理。内部 Rust 可执行目标名继续使用 `lumix-33-lut-maker`。

Windows 10/11 x64 需安装 Rust stable MSVC 工具链和 Visual Studio C++ Build Tools / Windows SDK。`.cargo/config.toml` 会为该目标静态链接 Microsoft C 运行库：

```powershell
cargo build --release --target x86_64-pc-windows-msvc
./scripts/package-windows.ps1 -Version 0.7.4
```

`.github/workflows/windows.yml` 会在普通推送和拉取请求上运行格式检查、测试、Clippy 和 release 编译；手动运行时生成单文件 EXE 并检查动态运行库依赖；`v0.7.4` 这类与 Cargo 版本一致的标签还会把 EXE 附加到 GitHub Release。

在 Apple Silicon macOS 上也可直接交叉生成同一单文件 EXE：先安装 `cargo-xwin`，再以 `LUMIX_SKIP_WINDOWS_RESOURCES=1 cargo xwin build --release --target x86_64-pc-windows-msvc --cross-compiler clang --bin lumix-33-lut-maker` 完成链接，最后运行 `cargo run --release --example package_windows_exe -- target/x86_64-pc-windows-msvc/release/lumix-33-lut-maker.exe dist/0.7.4/LUMIX-LUT-Maker-0.7.4-windows-x64.exe`。最后一步会跨平台写入并校验图标、manifest 和版本资源。

项目采用 MIT License。上游参考与第三方说明见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。当前版本为 0.7.4。

## 鸣谢

感谢 [Christian Schrinner](https://github.com/Skyfish1) 创作并以 MIT 许可
开源 [`lut-utility`](https://github.com/Skyfish1/lut-utility)。它提供了本项目最初的
“生成中性 LUT 图像 → 在图像软件中调色 → 转成 `.cube`”工作流基座。本项目在此思路上
独立扩展并重写了桌面应用、16-bit 图像处理、参考图校准、重采样和跨平台打包。
上游版权与许可文本及其他素材说明见
[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md)。
