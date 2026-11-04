//! Fixed tray colors, independent of the app's user-selected accent/theme.
use crate::tray_model::Icon;
#[derive(Clone, Copy, Debug)]
pub struct Ink {
    pub rgb: [u8; 3],
    pub alpha: f64,
}
pub fn glyph(icon: Icon, dark: bool) -> Ink {
    Ink {
        rgb: if dark { [255; 3] } else { [0; 3] },
        alpha: if icon == Icon::Checking {
            0.55
        } else if dark {
            0.88
        } else {
            0.85
        },
    }
}
pub fn badge(icon: Icon, dark: bool) -> Ink {
    let rgb = match (icon, dark) {
        (Icon::Down | Icon::Alert, false) => [0xB0, 0x14, 0x1A],
        (Icon::Down | Icon::Alert, true) => [0xFF, 0x7F, 0x75],
        (Icon::Waiting, false) => [0x8A, 0x4B, 0x00],
        (Icon::Waiting, true) => [0xFF, 0xB3, 0x40],
        (Icon::Conflict, false) => [0x7A, 0x35, 0xA6],
        (Icon::Conflict, true) => [0xC9, 0x9B, 0xF2],
        (Icon::Syncing, false) => [0x24, 0x5A, 0x74],
        (Icon::Syncing, true) => [0x6F, 0xB3, 0xD2],
        _ => return glyph(icon, dark),
    };
    Ink { rgb, alpha: 1.0 }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn luminance(rgb: [f64; 3]) -> f64 {
        let channels = rgb.map(|v| {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        });
        channels[0] * 0.2126 + channels[1] * 0.7152 + channels[2] * 0.0722
    }
    #[test]
    fn all_inks_pass_three_to_one_on_all_six_bar_samples() {
        for (dark, backgrounds) in [(false, [255, 224, 176]), (true, [0, 38, 80])] {
            for bg in backgrounds {
                let b = f64::from(bg) / 255.0;
                for icon in Icon::ALL {
                    for ink in [glyph(icon, dark), badge(icon, dark)] {
                        let fg = luminance(
                            ink.rgb
                                .map(|v| f64::from(v) / 255.0 * ink.alpha + b * (1.0 - ink.alpha)),
                        );
                        let bg = luminance([b; 3]);
                        let ratio = (fg.max(bg) + 0.05) / (fg.min(bg) + 0.05);
                        assert!(ratio >= 3.0, "{icon:?} dark={dark}: {ratio}");
                    }
                }
            }
        }
    }
}
