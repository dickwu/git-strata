//! Generated SVG masks are black plus alpha at both supported densities.
use crate::tray_model::Icon;
pub fn mask(icon: Icon, badge: bool, retina: bool) -> &'static [u8] {
    match (icon, badge, retina) {
        (Icon::Down, false, false) => include_bytes!("../icons/tray/glyph-down.png"),
        (Icon::Down, false, true) => include_bytes!("../icons/tray/glyph-down@2x.png"),
        (Icon::Down, true, false) => include_bytes!("../icons/tray/badge-down.png"),
        (Icon::Down, true, true) => include_bytes!("../icons/tray/badge-down@2x.png"),
        (Icon::Alert, false, false) => include_bytes!("../icons/tray/glyph-alert.png"),
        (Icon::Alert, false, true) => include_bytes!("../icons/tray/glyph-alert@2x.png"),
        (Icon::Alert, true, false) => include_bytes!("../icons/tray/badge-alert.png"),
        (Icon::Alert, true, true) => include_bytes!("../icons/tray/badge-alert@2x.png"),
        (Icon::Checking, false, false) => include_bytes!("../icons/tray/glyph-checking.png"),
        (Icon::Checking, false, true) => include_bytes!("../icons/tray/glyph-checking@2x.png"),
        (Icon::Checking, true, false) => include_bytes!("../icons/tray/badge-checking.png"),
        (Icon::Checking, true, true) => include_bytes!("../icons/tray/badge-checking@2x.png"),
        (Icon::Waiting, false, false) => include_bytes!("../icons/tray/glyph-waiting.png"),
        (Icon::Waiting, false, true) => include_bytes!("../icons/tray/glyph-waiting@2x.png"),
        (Icon::Waiting, true, false) => include_bytes!("../icons/tray/badge-waiting.png"),
        (Icon::Waiting, true, true) => include_bytes!("../icons/tray/badge-waiting@2x.png"),
        (Icon::Conflict, false, false) => include_bytes!("../icons/tray/glyph-conflict.png"),
        (Icon::Conflict, false, true) => include_bytes!("../icons/tray/glyph-conflict@2x.png"),
        (Icon::Conflict, true, false) => include_bytes!("../icons/tray/badge-conflict.png"),
        (Icon::Conflict, true, true) => include_bytes!("../icons/tray/badge-conflict@2x.png"),
        (Icon::Syncing, false, false) => include_bytes!("../icons/tray/glyph-syncing.png"),
        (Icon::Syncing, false, true) => include_bytes!("../icons/tray/glyph-syncing@2x.png"),
        (Icon::Syncing, true, false) => include_bytes!("../icons/tray/badge-syncing.png"),
        (Icon::Syncing, true, true) => include_bytes!("../icons/tray/badge-syncing@2x.png"),
        (Icon::Normal, false, false) => include_bytes!("../icons/tray/glyph-normal.png"),
        (Icon::Normal, false, true) => include_bytes!("../icons/tray/glyph-normal@2x.png"),
        (Icon::Normal, true, false) => include_bytes!("../icons/tray/badge-normal.png"),
        (Icon::Normal, true, true) => include_bytes!("../icons/tray/badge-normal@2x.png"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use tauri::image::Image;
    #[test]
    fn every_mask_has_expected_size_black_alpha_and_separate_knockout() {
        for icon in Icon::ALL {
            for retina in [false, true] {
                let glyph = Image::from_bytes(mask(icon, false, retina)).unwrap();
                let badge = Image::from_bytes(mask(icon, true, retina)).unwrap();
                let size = if retina { 36 } else { 18 };
                for image in [&glyph, &badge] {
                    assert_eq!((image.width(), image.height()), (size, size));
                    assert!(
                        image
                            .rgba()
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .all(|p| p[3] == 0 || p[..3] == [0, 0, 0])
                    );
                }
                assert!(glyph.rgba().as_chunks::<4>().0.iter().any(|p| p[3] > 0));
                assert_eq!(
                    badge.rgba().as_chunks::<4>().0.iter().any(|p| p[3] > 0),
                    icon != Icon::Normal
                );
                // Raster filters leave alpha 1 fringe at 1x; the same +/-2
                // tolerance as the reproducibility check applies here.
                assert!(
                    glyph
                        .rgba()
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .zip(badge.rgba().as_chunks::<4>().0.iter())
                        .all(|(a, b)| a[3] <= 2 || b[3] <= 2)
                );
            }
        }
    }
}
