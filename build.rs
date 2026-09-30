fn main() {
    println!("cargo:rerun-if-changed=assets/windows/AppIcon.ico");
    println!("cargo:rerun-if-changed=assets/windows/app.manifest");
    println!("cargo:rerun-if-env-changed=LUMIX_SKIP_WINDOWS_RESOURCES");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || std::env::var_os("LUMIX_SKIP_WINDOWS_RESOURCES").is_some()
    {
        return;
    }

    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon("assets/windows/AppIcon.ico")
        .set_manifest_file("assets/windows/app.manifest")
        .set("FileDescription", "LUMIX LUT Maker")
        .set("ProductName", "LUMIX LUT Maker")
        .set("InternalName", "lumix-33-lut-maker.exe")
        .set("OriginalFilename", "LUMIX LUT Maker.exe")
        .set(
            "LegalCopyright",
            "Copyright 2026 LUMIX LUT Maker contributors",
        );
    resource
        .compile()
        .expect("Windows application resources must compile");
}
