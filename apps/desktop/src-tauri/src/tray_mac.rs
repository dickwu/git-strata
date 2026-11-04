//! One cached, non-template NSImage per state. AppKit resolves each drawing
//! appearance, including its cached representations; the app theme is irrelevant.
use crate::{
    tray_assets,
    tray_model::Icon,
    tray_palette::{self, Ink},
};
use block2::RcBlock;
use objc2::{AnyThread, MainThreadMarker, rc::Retained, runtime::Bool};
use objc2_app_kit::{
    NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSBezierPath, NSColor,
    NSCompositingOperation, NSGraphicsContext, NSImage,
};
use objc2_core_graphics::CGContext;
use objc2_foundation::{NSArray, NSData, NSPoint, NSRect, NSSize, NSString};
use std::{cell::RefCell, collections::HashMap};
thread_local! { static IMAGES: RefCell<HashMap<Icon,Retained<NSImage>>> = RefCell::new(HashMap::new()); }
fn bounds() -> NSRect {
    NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(18.0, 18.0))
}
fn tinted(mask: &NSImage, ink: Ink) {
    let Some(context) = NSGraphicsContext::currentContext() else {
        return;
    };
    let cg = context.CGContext();
    context.saveGraphicsState();
    // SAFETY: current drawing CGContext remains alive for this balanced layer;
    // no auxiliary dictionary or raw pointers are supplied.
    unsafe {
        CGContext::begin_transparency_layer(Some(&cg), None);
    }
    mask.drawInRect_fromRect_operation_fraction(
        bounds(),
        NSRect::ZERO,
        NSCompositingOperation::SourceOver,
        1.0,
    );
    let [r, g, b] = ink.rgb.map(|v| f64::from(v) / 255.0);
    NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, ink.alpha).setFill();
    context.setCompositingOperation(NSCompositingOperation::SourceIn);
    NSBezierPath::fillRect(bounds());
    CGContext::end_transparency_layer(Some(&cg));
    context.restoreGraphicsState();
}
pub(crate) fn image(icon: Icon) -> Retained<NSImage> {
    assert!(
        MainThreadMarker::new().is_some(),
        "tray images belong to the main thread"
    );
    IMAGES.with_borrow_mut(|cache| {
        cache
            .entry(icon)
            .or_insert_with(|| {
                let load = |badge| {
                    let mask = NSImage::initWithData(
                        NSImage::alloc(),
                        &NSData::with_bytes(tray_assets::mask(icon, badge, true)),
                    )
                    .expect("validated tray mask");
                    mask.setSize(NSSize::new(18.0, 18.0));
                    mask
                };
                let (glyph, badge) = (load(false), load(true));
                let handler = RcBlock::new(move |_rect: NSRect| {
                    // SAFETY: AppKit's immutable appearance-name constants.
                    let names = unsafe {
                        NSArray::from_slice(&[NSAppearanceNameAqua, NSAppearanceNameDarkAqua])
                    };
                    let appearance = NSAppearance::currentDrawingAppearance()
                        .bestMatchFromAppearancesWithNames(&names);
                    let dark =
                        appearance.is_some_and(|a| unsafe { &*a == NSAppearanceNameDarkAqua });
                    tinted(&glyph, tray_palette::glyph(icon, dark));
                    if icon != Icon::Normal {
                        tinted(&badge, tray_palette::badge(icon, dark));
                    }
                    Bool::YES
                });
                let image = NSImage::imageWithSize_flipped_drawingHandler(
                    NSSize::new(18.0, 18.0),
                    false,
                    &handler,
                );
                image.setTemplate(false);
                image
            })
            .clone()
    })
}
/// Called only inside Tauri's main-thread `with_inner_tray_icon` closure.
pub fn show(status: &objc2_app_kit::NSStatusItem, icon: Icon, description: &str, changed: bool) {
    let Some(button) = status.button(MainThreadMarker::new().expect("main thread")) else {
        return;
    };
    let image = image(icon);
    image.setAccessibilityDescription(Some(&NSString::from_str(description)));
    if changed {
        button.setImage(Some(&image));
    }
}
