//! Offline previews of the actual committed masks and production palette.
use std::{fs::File, io::BufWriter, path::Path};
use strata_desktop_lib::{
    tray_assets::mask,
    tray_model::Icon,
    tray_palette::{self, Ink},
};
use tauri::image::Image;
fn composite(out: &mut [u8], mask: &[u8], ink: Ink) {
    for (pixel, m) in out
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(mask.as_chunks::<4>().0.iter())
    {
        let a = f64::from(m[3]) / 255.0 * ink.alpha;
        for (channel, color) in pixel[..3].iter_mut().zip(ink.rgb) {
            *channel = (f64::from(color) * a + f64::from(*channel) * (1.0 - a)).round() as u8;
        }
    }
}
fn main() {
    let output = std::env::args_os().nth(1).expect("preview directory");
    let output = Path::new(&output);
    std::fs::create_dir_all(output).unwrap();
    let mut count = 0;
    for icon in Icon::ALL {
        for (dark, bgs) in [(false, [255, 224, 176]), (true, [0, 38, 80])] {
            for bg in bgs {
                for retina in [false, true] {
                    let image = Image::from_bytes(mask(icon, false, retina)).unwrap();
                    let badge = Image::from_bytes(mask(icon, true, retina)).unwrap();
                    let size = image.width();
                    let mut bytes = [bg, bg, bg, 255].repeat((size * size) as usize);
                    composite(&mut bytes, image.rgba(), tray_palette::glyph(icon, dark));
                    composite(&mut bytes, badge.rgba(), tray_palette::badge(icon, dark));
                    for gray in [false, true] {
                        let mut pixels = bytes.clone();
                        if gray {
                            for p in pixels.as_chunks_mut::<4>().0.iter_mut() {
                                let y = (f64::from(p[0]) * 0.2126
                                    + f64::from(p[1]) * 0.7152
                                    + f64::from(p[2]) * 0.0722)
                                    .round() as u8;
                                p[..3].fill(y);
                            }
                        }
                        let name = format!(
                            "{}-{}-{}-{}{}.png",
                            icon.name(),
                            if dark { "dark" } else { "light" },
                            bg,
                            if retina { "2x" } else { "1x" },
                            if gray { "-gray" } else { "" }
                        );
                        let mut encoder = png::Encoder::new(
                            BufWriter::new(File::create(output.join(name)).unwrap()),
                            size,
                            size,
                        );
                        encoder.set_color(png::ColorType::Rgba);
                        encoder.set_depth(png::BitDepth::Eight);
                        encoder
                            .write_header()
                            .unwrap()
                            .write_image_data(&pixels)
                            .unwrap();
                        count += 1;
                    }
                }
            }
        }
    }
    println!("{count} previews: seven states, six bar backgrounds, 1x/2x, color/grayscale");
}
