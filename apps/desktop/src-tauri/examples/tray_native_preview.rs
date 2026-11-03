//! Exercise the actual C renderer offscreen. No app, status item or daemon launch.
#[cfg(target_os = "macos")]
use strata_desktop_lib::{tray_assets, tray_model, tray_palette};
#[cfg(target_os = "macos")]
#[allow(dead_code)] // show(status item) is deliberately not called by this offscreen probe.
#[path = "../src/tray_mac.rs"]
mod tray_mac;
#[cfg(target_os = "macos")]
fn main() {
    use block2::RcBlock;
    use objc2::AnyThread;
    use objc2_app_kit::{
        NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSBitmapImageFileType,
        NSBitmapImageRep, NSCompositingOperation, NSDeviceRGBColorSpace, NSGraphicsContext,
    };
    use objc2_foundation::{NSDictionary, NSPoint, NSRect, NSSize, NSString};
    let output = std::path::PathBuf::from(std::env::args_os().nth(1).expect("preview directory"));
    std::fs::create_dir_all(&output).unwrap();
    for icon in tray_model::Icon::ALL {
        let image = tray_mac::image(icon);
        image.setAccessibilityDescription(Some(&NSString::from_str(icon.name())));
        let mut light = None;
        let mut dark = None;
        for (pass, is_dark) in [false, true, false].into_iter().enumerate() {
            // SAFETY: null planes asks AppKit to allocate/own the complete RGBA bitmap.
            let rep=unsafe {NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(NSBitmapImageRep::alloc(),std::ptr::null_mut(),36,36,8,4,true,false,NSDeviceRGBColorSpace,0,0)}.unwrap();
            rep.setSize(NSSize::new(18.0, 18.0));
            let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep).unwrap();
            NSGraphicsContext::saveGraphicsState_class();
            NSGraphicsContext::setCurrentContext(Some(&context));
            // SAFETY: immutable AppKit appearance-name constants.
            let appearance = NSAppearance::appearanceNamed(unsafe {
                if is_dark {
                    NSAppearanceNameDarkAqua
                } else {
                    NSAppearanceNameAqua
                }
            })
            .unwrap();
            let draw = RcBlock::new(|| {
                image.drawInRect_fromRect_operation_fraction(
                    NSRect::new(NSPoint::ZERO, NSSize::new(18.0, 18.0)),
                    NSRect::ZERO,
                    NSCompositingOperation::SourceOver,
                    1.0,
                )
            });
            appearance.performAsCurrentDrawingAppearance(&draw);
            NSGraphicsContext::restoreGraphicsState_class();
            // SAFETY: the empty dictionary contains no incorrectly typed properties.
            let png = unsafe {
                rep.representationUsingType_properties(
                    NSBitmapImageFileType::PNG,
                    &NSDictionary::new(),
                )
            }
            .unwrap()
            .to_vec();
            if pass == 0 {
                light = Some(png.clone());
            } else if pass == 1 {
                dark = Some(png.clone());
            } else {
                assert_eq!(
                    light.as_ref().unwrap(),
                    &png,
                    "cached light appearance changed"
                );
            }
            if pass < 2 {
                std::fs::write(
                    output.join(format!(
                        "native-{}-{}.png",
                        icon.name(),
                        if is_dark { "dark" } else { "light" }
                    )),
                    png,
                )
                .unwrap();
            }
        }
        assert_ne!(light, dark, "{icon:?} failed to adapt");
        assert_eq!(image.size(), NSSize::new(18.0, 18.0));
        assert!(!image.isTemplate());
        assert_eq!(
            image.accessibilityDescription().unwrap().to_string(),
            icon.name()
        );
    }
    println!(
        "14 native previews passed: real renderer, light/dark/light cache, 18pt, non-template, accessibility description"
    );
}
#[cfg(not(target_os = "macos"))]
fn main() {
    println!("Native tray preview requires macOS");
}
