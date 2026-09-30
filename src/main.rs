#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod platform;
mod preview;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("LUMIX LUT Maker")
            .with_icon(load_app_icon())
            .with_inner_size([1280.0, 760.0])
            .with_min_inner_size([1050.0, 650.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };

    eframe::run_native(
        "LUMIX LUT Maker",
        options,
        Box::new(|context| Ok(Box::new(app::LumixApp::new(context)))),
    )
}

fn load_app_icon() -> eframe::egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../assets/AppIcon.iconset/icon_512x512.png"))
        .expect("embedded application icon must be a valid PNG")
}

#[cfg(test)]
mod tests {
    #[test]
    fn embedded_runtime_icon_is_valid() {
        let icon = super::load_app_icon();
        assert_eq!((icon.width, icon.height), (512, 512));
        assert_eq!(icon.rgba.len(), 512 * 512 * 4);
    }
}
