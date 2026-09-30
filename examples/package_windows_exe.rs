use std::path::{Path, PathBuf};

use editpe::constants::{
    IMAGE_SUBSYSTEM_WINDOWS_GUI, VS_FILE_DESCRIPTION, VS_FILE_VERSION, VS_INTERNAL_NAME,
    VS_LEGAL_COPYRIGHT, VS_ORIGINAL_FILENAME, VS_PRODUCT_NAME, VS_PRODUCT_VERSION,
};
use editpe::types::{VersionU16, VersionU32};
use editpe::{Image, VersionInfo, VersionStringTable};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let source = arguments.next().map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from("target/x86_64-pc-windows-msvc/release/lumix-33-lut-maker.exe")
    });
    let destination = arguments.next().map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(format!(
            "dist/{}/LUMIX-LUT-Maker-{}-windows-x64.exe",
            env!("CARGO_PKG_VERSION"),
            env!("CARGO_PKG_VERSION")
        ))
    });
    if arguments.next().is_some() {
        return Err("用法：package_windows_exe [源 EXE] [目标 EXE]".into());
    }
    package(&source, &destination)?;
    println!("{}", destination.display());
    Ok(())
}

fn package(source: &Path, destination: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(source, destination)?;

    let mut image = Image::parse_file(destination)?;
    let mut resources = image.resource_directory().cloned().unwrap_or_default();
    resources.set_main_icon_file("assets/windows/AppIcon.ico")?;
    resources.set_manifest(&std::fs::read_to_string("assets/windows/app.manifest")?)?;
    resources.set_version_info(&version_info())?;
    image.set_resource_directory(resources)?;
    image.write_file(destination)?;
    verify(destination)?;
    Ok(())
}

fn verify(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let image = Image::parse_file(path)?;
    if image.coff_header().machine != 0x8664 {
        return Err("最终文件不是 Windows x64 PE。".into());
    }
    if image.subsystem() != IMAGE_SUBSYSTEM_WINDOWS_GUI {
        return Err("最终文件不是无控制台窗口的 Windows GUI 程序。".into());
    }
    let resources = image
        .resource_directory()
        .ok_or("最终 EXE 缺少资源目录。")?;
    if resources.get_main_icon()?.is_none() {
        return Err("最终 EXE 缺少应用图标。".into());
    }
    let manifest = resources
        .get_manifest()?
        .ok_or("最终 EXE 缺少应用 manifest。")?;
    for required in ["asInvoker", "PerMonitorV2", "longPathAware"] {
        if !manifest.contains(required) {
            return Err(format!("最终 EXE manifest 缺少 {required}。").into());
        }
    }
    let version = resources
        .get_version_info()?
        .ok_or("最终 EXE 缺少版本信息。")?;
    let product_name_is_valid = version.strings.iter().any(|table| {
        table.strings.get(VS_PRODUCT_NAME).map(String::as_str) == Some("LUMIX LUT Maker")
    });
    if !product_name_is_valid {
        return Err("最终 EXE 的产品名称不正确。".into());
    }
    Ok(())
}

fn version_info() -> VersionInfo {
    let numbers = package_version_numbers();
    let fixed_version = VersionU32 {
        major: ((numbers[0] as u32) << 16) | numbers[1] as u32,
        minor: ((numbers[2] as u32) << 16) | numbers[3] as u32,
    };
    let display_version = format!(
        "{}.{}.{}.{}",
        numbers[0], numbers[1], numbers[2], numbers[3]
    );

    let mut strings = VersionStringTable {
        key: "040904B0".to_owned(),
        ..VersionStringTable::default()
    };
    for (key, value) in [
        (VS_FILE_DESCRIPTION, "LUMIX LUT Maker"),
        (VS_FILE_VERSION, display_version.as_str()),
        (VS_INTERNAL_NAME, "lumix-33-lut-maker.exe"),
        (
            VS_LEGAL_COPYRIGHT,
            "Copyright 2026 LUMIX LUT Maker contributors",
        ),
        (VS_ORIGINAL_FILENAME, "LUMIX LUT Maker.exe"),
        (VS_PRODUCT_NAME, "LUMIX LUT Maker"),
        (VS_PRODUCT_VERSION, display_version.as_str()),
    ] {
        strings.strings.insert(key.to_owned(), value.to_owned());
    }

    let mut info = VersionInfo::default();
    info.info.file_version = fixed_version;
    info.info.product_version = fixed_version;
    info.strings.push(strings);
    info.vars.push(VersionU16 {
        major: 0x0409,
        minor: 1200,
    });
    info
}

fn package_version_numbers() -> [u16; 4] {
    let mut numbers = env!("CARGO_PKG_VERSION")
        .split('.')
        .map(|number| number.parse::<u16>().unwrap_or(0));
    [
        numbers.next().unwrap_or(0),
        numbers.next().unwrap_or(0),
        numbers.next().unwrap_or(0),
        0,
    ]
}

#[cfg(test)]
mod tests {
    use super::{package_version_numbers, version_info};

    #[test]
    fn version_resource_matches_cargo_package() {
        let numbers = package_version_numbers();
        assert_eq!(numbers[3], 0);
        let info = version_info();
        assert_eq!(info.strings.len(), 1);
        assert_eq!(
            info.strings[0]
                .strings
                .get("ProductName")
                .map(String::as_str),
            Some("LUMIX LUT Maker")
        );
        let expected_version = format!("{}.0", env!("CARGO_PKG_VERSION"));
        assert_eq!(
            info.strings[0]
                .strings
                .get("ProductVersion")
                .map(String::as_str),
            Some(expected_version.as_str())
        );
    }
}
