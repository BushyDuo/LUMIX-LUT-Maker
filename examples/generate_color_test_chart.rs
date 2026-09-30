use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;

use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use lumix_lut::srgb_icc_profile;

const WIDTH: u32 = 2048;
const HEIGHT: u32 = 1152;

type Rgb = [u8; 3];

struct Canvas {
    pixels: Vec<u8>,
}

impl Canvas {
    fn new(color: Rgb) -> Self {
        let mut canvas = Self {
            pixels: vec![0; (WIDTH * HEIGHT * 3) as usize],
        };
        canvas.fill_rect(0, 0, WIDTH, HEIGHT, color);
        canvas
    }

    fn set(&mut self, x: u32, y: u32, color: Rgb) {
        if x >= WIDTH || y >= HEIGHT {
            return;
        }
        let index = ((y * WIDTH + x) * 3) as usize;
        self.pixels[index..index + 3].copy_from_slice(&color);
    }

    fn fill_rect(&mut self, x: u32, y: u32, width: u32, height: u32, color: Rgb) {
        for py in y..(y + height).min(HEIGHT) {
            for px in x..(x + width).min(WIDTH) {
                self.set(px, py, color);
            }
        }
    }

    fn stroke_rect(&mut self, x: u32, y: u32, width: u32, height: u32, color: Rgb) {
        self.fill_rect(x, y, width, 1, color);
        self.fill_rect(x, y + height.saturating_sub(1), width, 1, color);
        self.fill_rect(x, y, 1, height, color);
        self.fill_rect(x + width.saturating_sub(1), y, 1, height, color);
    }

    fn panel(&mut self, x: u32, y: u32, width: u32, height: u32) {
        self.fill_rect(x, y, width, height, [19, 19, 23]);
        self.stroke_rect(x, y, width, height, [43, 43, 49]);
    }

    fn horizontal_gradient<F>(&mut self, x: u32, y: u32, width: u32, height: u32, color_at: F)
    where
        F: Fn(f32) -> Rgb,
    {
        for offset in 0..width {
            let t = offset as f32 / width.saturating_sub(1).max(1) as f32;
            let color = color_at(t);
            self.fill_rect(x + offset, y, 1, height, color);
        }
    }

    fn text(&mut self, x: u32, y: u32, value: &str, scale: u32, color: Rgb) {
        let mut cursor = x;
        for character in value.chars() {
            let glyph = glyph(character.to_ascii_uppercase());
            for (row, bits) in glyph.iter().enumerate() {
                for column in 0..5 {
                    if bits & (1 << (4 - column)) != 0 {
                        self.fill_rect(
                            cursor + column * scale,
                            y + row as u32 * scale,
                            scale,
                            scale,
                            color,
                        );
                    }
                }
            }
            cursor += 6 * scale;
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let destination = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("LUT_Color_Test_Chart.png"));
    let mut canvas = Canvas::new([11, 11, 13]);

    canvas.text(32, 24, "LUMIX LUT DIAGNOSTIC", 5, [238, 238, 241]);
    canvas.text(
        34,
        68,
        "DETERMINISTIC SRGB DISPLAY TEST / RGB 8-BIT / 2048 X 1152",
        2,
        [145, 148, 156],
    );
    canvas.fill_rect(32, 94, 1984, 2, [232, 160, 76]);

    draw_tone_panel(&mut canvas);
    draw_hue_panel(&mut canvas);
    draw_channel_panel(&mut canvas);
    draw_diagonal_panel(&mut canvas);
    draw_reference_panel(&mut canvas);

    let file = File::create(&destination)?;
    let mut encoder = PngEncoder::new(BufWriter::new(file));
    encoder.set_icc_profile(srgb_icc_profile().to_vec())?;
    encoder.write_image(&canvas.pixels, WIDTH, HEIGHT, ExtendedColorType::Rgb8)?;
    println!("{}", destination.display());
    Ok(())
}

fn draw_tone_panel(canvas: &mut Canvas) {
    canvas.panel(32, 112, 1984, 250);
    canvas.text(52, 130, "LUMA / CLIPPING", 3, [232, 160, 76]);

    canvas.horizontal_gradient(52, 164, 1944, 64, |t| {
        let value = (t * 255.0).round() as u8;
        [value, value, value]
    });
    for (index, value) in [0, 32, 64, 96, 128, 160, 192, 224, 255].iter().enumerate() {
        let x = 52 + ((1943.0 * (*value as f32 / 255.0)).round() as u32);
        canvas.fill_rect(x, 228, 1, 8, [232, 160, 76]);
        let text_x = if index == 8 { x - 30 } else { x };
        canvas.text(text_x, 240, &value.to_string(), 2, [150, 152, 160]);
    }

    canvas.text(52, 276, "NEAR BLACK 0-31", 2, [150, 152, 160]);
    canvas.text(1072, 276, "NEAR WHITE 224-255", 2, [150, 152, 160]);
    draw_steps(canvas, 52, 298, 924, 42, 0);
    draw_steps(canvas, 1072, 298, 924, 42, 224);
}

fn draw_steps(canvas: &mut Canvas, x: u32, y: u32, width: u32, height: u32, start: u8) {
    for index in 0..32 {
        let left = x + width * index / 32;
        let right = x + width * (index + 1) / 32;
        let value = start.saturating_add(index as u8);
        canvas.fill_rect(left, y, right - left, height, [value, value, value]);
    }
    canvas.stroke_rect(x, y, width, height, [72, 72, 78]);
}

fn draw_hue_panel(canvas: &mut Canvas) {
    canvas.panel(32, 382, 980, 342);
    canvas.text(52, 400, "HUE / SATURATION", 3, [232, 160, 76]);
    let rows = [
        ("S100 V100", 1.0, 1.0),
        ("S075 V100", 0.75, 1.0),
        ("S050 V100", 0.50, 1.0),
        ("S100 V075", 1.0, 0.75),
        ("S100 V050", 1.0, 0.50),
    ];
    for (index, (label, saturation, value)) in rows.iter().enumerate() {
        let y = 444 + index as u32 * 52;
        canvas.text(52, y + 11, label, 2, [150, 152, 160]);
        canvas.horizontal_gradient(184, y, 808, 42, |t| hsv_to_rgb(t, *saturation, *value));
    }
}

fn draw_channel_panel(canvas: &mut Canvas) {
    canvas.panel(1028, 382, 988, 342);
    canvas.text(1048, 400, "RGB / CMY CHANNEL RAMPS", 3, [232, 160, 76]);
    let channels = [
        ("RED", [255, 0, 0]),
        ("GREEN", [0, 255, 0]),
        ("BLUE", [0, 0, 255]),
        ("CYAN", [0, 255, 255]),
        ("MAGENTA", [255, 0, 255]),
        ("YELLOW", [255, 255, 0]),
    ];
    for (index, (label, endpoint)) in channels.iter().enumerate() {
        let y = 442 + index as u32 * 44;
        canvas.text(1048, y + 9, label, 2, [150, 152, 160]);
        canvas.horizontal_gradient(1168, y, 828, 34, |t| lerp([0, 0, 0], *endpoint, t));
    }
}

fn draw_diagonal_panel(canvas: &mut Canvas) {
    canvas.panel(32, 740, 1210, 380);
    canvas.text(52, 758, "CUBE DIAGONALS / INTERPOLATION", 3, [232, 160, 76]);
    let ramps = [
        ("BLACK-WHITE", [0, 0, 0], [255, 255, 255], None),
        ("RED-CYAN", [255, 0, 0], [0, 255, 255], None),
        ("GREEN-MAG", [0, 255, 0], [255, 0, 255], None),
        ("BLUE-YELLOW", [0, 0, 255], [255, 255, 0], None),
        ("BLACK-R-W", [0, 0, 0], [255, 255, 255], Some([255, 0, 0])),
        ("BLACK-G-W", [0, 0, 0], [255, 255, 255], Some([0, 255, 0])),
        ("BLACK-B-W", [0, 0, 0], [255, 255, 255], Some([0, 0, 255])),
        ("WARM-COOL", [238, 126, 44], [42, 178, 196], None),
    ];
    for (index, (label, start, end, midpoint)) in ramps.iter().enumerate() {
        let column = index % 2;
        let row = index / 2;
        let x = 52 + column as u32 * 595;
        let y = 804 + row as u32 * 74;
        canvas.text(x, y + 16, label, 2, [150, 152, 160]);
        canvas.horizontal_gradient(x + 142, y, 423, 52, |t| match midpoint {
            Some(middle) if t < 0.5 => lerp(*start, *middle, t * 2.0),
            Some(middle) => lerp(*middle, *end, (t - 0.5) * 2.0),
            None => lerp(*start, *end, t),
        });
    }
}

fn draw_reference_panel(canvas: &mut Canvas) {
    canvas.panel(1258, 740, 758, 380);
    canvas.text(1278, 758, "REFERENCE / AXIS CHECK", 3, [232, 160, 76]);
    let swatches = [
        ("SKIN 1", [239, 203, 177]),
        ("SKIN 2", [190, 129, 105]),
        ("SKIN 3", [116, 77, 60]),
        ("SKY", [74, 131, 190]),
        ("LEAF", [58, 108, 69]),
        ("EARTH", [151, 96, 57]),
        ("ROSE", [151, 76, 105]),
        ("VIOLET", [91, 75, 144]),
        ("TEAL", [55, 145, 151]),
        ("GOLD", [221, 169, 61]),
        ("GRAY 16", [16, 16, 16]),
        ("GRAY 64", [64, 64, 64]),
        ("GRAY 128", [128, 128, 128]),
        ("GRAY 192", [192, 192, 192]),
        ("GRAY 240", [240, 240, 240]),
        ("R32 G64 B128", [32, 64, 128]),
        ("R64 G128 B32", [64, 128, 32]),
        ("R128 G32 B64", [128, 32, 64]),
        ("ORANGE", [220, 128, 32]),
        ("AQUA", [32, 180, 220]),
    ];
    let swatch_width = 136;
    let swatch_height = 66;
    for (index, (label, color)) in swatches.iter().enumerate() {
        let column = index % 5;
        let row = index / 5;
        let x = 1278 + column as u32 * 145;
        let y = 804 + row as u32 * 74;
        canvas.fill_rect(x, y, swatch_width, swatch_height, *color);
        let text_color = if luminance(*color) > 150.0 {
            [12, 12, 14]
        } else {
            [244, 244, 246]
        };
        canvas.fill_rect(x, y + 48, swatch_width, 18, [18, 18, 21]);
        canvas.text(x + 6, y + 52, label, 1, text_color.max([180, 180, 184]));
        canvas.stroke_rect(x, y, swatch_width, swatch_height, [72, 72, 78]);
    }
}

fn lerp(start: Rgb, end: Rgb, t: f32) -> Rgb {
    std::array::from_fn(|index| {
        (start[index] as f32 + (end[index] as f32 - start[index] as f32) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    })
}

fn hsv_to_rgb(hue: f32, saturation: f32, value: f32) -> Rgb {
    let h = (hue.fract() * 6.0).clamp(0.0, 6.0);
    let index = h.floor() as i32;
    let fraction = h - index as f32;
    let p = value * (1.0 - saturation);
    let q = value * (1.0 - saturation * fraction);
    let t = value * (1.0 - saturation * (1.0 - fraction));
    let (red, green, blue) = match index.rem_euclid(6) {
        0 => (value, t, p),
        1 => (q, value, p),
        2 => (p, value, t),
        3 => (p, q, value),
        4 => (t, p, value),
        _ => (value, p, q),
    };
    [red, green, blue].map(|channel| (channel * 255.0).round() as u8)
}

fn luminance(color: Rgb) -> f32 {
    color[0] as f32 * 0.2126 + color[1] as f32 * 0.7152 + color[2] as f32 * 0.0722
}

fn glyph(character: char) -> [u8; 7] {
    match character {
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'D' => [30, 17, 17, 17, 17, 17, 30],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'F' => [31, 16, 16, 30, 16, 16, 16],
        'G' => [14, 17, 16, 23, 17, 17, 15],
        'H' => [17, 17, 17, 31, 17, 17, 17],
        'I' => [31, 4, 4, 4, 4, 4, 31],
        'J' => [7, 2, 2, 2, 18, 18, 12],
        'K' => [17, 18, 20, 24, 20, 18, 17],
        'L' => [16, 16, 16, 16, 16, 16, 31],
        'M' => [17, 27, 21, 21, 17, 17, 17],
        'N' => [17, 25, 21, 19, 17, 17, 17],
        'O' => [14, 17, 17, 17, 17, 17, 14],
        'P' => [30, 17, 17, 30, 16, 16, 16],
        'Q' => [14, 17, 17, 17, 21, 18, 13],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'S' => [15, 16, 16, 14, 1, 1, 30],
        'T' => [31, 4, 4, 4, 4, 4, 4],
        'U' => [17, 17, 17, 17, 17, 17, 14],
        'V' => [17, 17, 17, 17, 17, 10, 4],
        'W' => [17, 17, 17, 21, 21, 21, 10],
        'X' => [17, 17, 10, 4, 10, 17, 17],
        'Y' => [17, 17, 10, 4, 4, 4, 4],
        'Z' => [31, 1, 2, 4, 8, 16, 31],
        '0' => [14, 17, 19, 21, 25, 17, 14],
        '1' => [4, 12, 4, 4, 4, 4, 14],
        '2' => [14, 17, 1, 2, 4, 8, 31],
        '3' => [30, 1, 1, 14, 1, 1, 30],
        '4' => [2, 6, 10, 18, 31, 2, 2],
        '5' => [31, 16, 16, 30, 1, 1, 30],
        '6' => [14, 16, 16, 30, 17, 17, 14],
        '7' => [31, 1, 2, 4, 8, 8, 8],
        '8' => [14, 17, 17, 14, 17, 17, 14],
        '9' => [14, 17, 17, 15, 1, 1, 14],
        '-' => [0, 0, 0, 31, 0, 0, 0],
        '/' => [1, 2, 2, 4, 8, 8, 16],
        '.' => [0, 0, 0, 0, 0, 12, 12],
        ':' => [0, 12, 12, 0, 12, 12, 0],
        _ => [0; 7],
    }
}
