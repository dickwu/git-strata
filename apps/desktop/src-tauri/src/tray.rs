//! gui.md §5.8: the menu-bar item, rendered from `tray_model`.

use std::sync::Mutex;
use std::time::SystemTime;

use tauri::image::Image;
use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::lifecycle;
use crate::tray_model::{Icon, Snapshot, TrayModel, model};

const TRAY_ID: &str = "main";
/// Payload: the waiting item's id (`waiting:deps:12`); the window refetches
/// and opens it (gui.md §5.8: fresh data before any dialog).
pub const OPEN_EVENT: &str = "strata://open";

fn icon(i: Icon) -> tauri::Result<Image<'static>> {
    Image::from_bytes(crate::tray_assets::mask(i, false, true))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TrayVisual {
    icon: Icon,
    tooltip: String,
}
trait TrayRenderer {
    fn show(&mut self, visual: &TrayVisual, icon_changed: bool) -> tauri::Result<()>;
    fn menu(&mut self, model: &TrayModel) -> tauri::Result<()>;
}
#[derive(Default)]
struct RenderCache {
    last: Option<TrayModel>,
}
impl RenderCache {
    fn paint(&mut self, m: TrayModel, renderer: &mut impl TrayRenderer) -> tauri::Result<()> {
        let prev = self.last.as_ref();
        let icon_changed = prev.is_none_or(|p| p.icon != m.icon);
        if icon_changed || prev.is_none_or(|p| p.tooltip != m.tooltip) {
            renderer.show(
                &TrayVisual {
                    icon: m.icon,
                    tooltip: m.tooltip.clone(),
                },
                icon_changed,
            )?;
        }
        if prev.is_none_or(|p| {
            p.status_line != m.status_line
                || p.waiting != m.waiting
                || p.more != m.more
                || p.stale != m.stale
                || p.unavailable != m.unavailable
        }) {
            renderer.menu(&m)?;
        }
        self.last = Some(m);
        Ok(())
    }
}
// Never acquired on the main thread. Covers snapshot -> model -> native paint -> last.
static RENDERER: Mutex<RenderCache> = Mutex::new(RenderCache { last: None });
struct NativeRenderer<'a, R: Runtime>(&'a AppHandle<R>);
impl<R: Runtime> TrayRenderer for NativeRenderer<'_, R> {
    fn show(&mut self, visual: &TrayVisual, icon_changed: bool) -> tauri::Result<()> {
        let Some(tray) = self.0.tray_by_id(TRAY_ID) else {
            return Ok(());
        };
        tray.set_tooltip(Some(&visual.tooltip))?;
        #[cfg(target_os = "macos")]
        {
            let icon = visual.icon;
            let description = visual
                .tooltip
                .strip_prefix("GIT STRATA: ")
                .unwrap_or(&visual.tooltip)
                .to_string();
            tray.with_inner_tray_icon(move |inner| {
                if let Some(status) = inner.ns_status_item() {
                    crate::tray_mac::show(&status, icon, &description, icon_changed);
                }
            })?;
        }
        #[cfg(not(target_os = "macos"))]
        if icon_changed {
            tray.set_icon(Some(icon(visual.icon)?))?;
        }
        Ok(())
    }
    fn menu(&mut self, m: &TrayModel) -> tauri::Result<()> {
        if let Some(tray) = self.0.tray_by_id(TRAY_ID) {
            tray.set_menu(Some(menu(self.0, m)?))?;
        }
        Ok(())
    }
}

fn menu<R: Runtime>(app: &AppHandle<R>, m: &TrayModel) -> tauri::Result<Menu<R>> {
    let mut items: Vec<Box<dyn IsMenuItem<R>>> = Vec::new();
    items.push(Box::new(MenuItem::with_id(
        app,
        "status",
        &m.status_line,
        false,
        None::<&str>,
    )?));
    items.push(Box::new(PredefinedMenuItem::separator(app)?));
    if let Some(stale) = &m.stale {
        items.push(Box::new(MenuItem::with_id(
            app,
            "stale",
            stale,
            false,
            None::<&str>,
        )?));
    }
    if m.unavailable > 0 {
        items.push(Box::new(MenuItem::with_id(
            app,
            "waiting:upstream",
            format!(
                "Upstream detail unavailable for {} {}",
                m.unavailable,
                if m.unavailable == 1 {
                    "session"
                } else {
                    "sessions"
                }
            ),
            true,
            None::<&str>,
        )?));
    }
    if !m.waiting.is_empty() {
        items.push(Box::new(MenuItem::with_id(
            app,
            "waiting",
            "Waiting for you",
            false,
            None::<&str>,
        )?));
        for w in &m.waiting {
            items.push(Box::new(MenuItem::with_id(
                app,
                w.id(),
                w.label(),
                true,
                None::<&str>,
            )?));
        }
        if m.more > 0 {
            items.push(Box::new(MenuItem::with_id(
                app,
                "waiting:more",
                format!("{} more in {}", m.more, crate::APP_NAME),
                true,
                None::<&str>,
            )?));
        }
        items.push(Box::new(PredefinedMenuItem::separator(app)?));
    }
    items.push(Box::new(MenuItem::with_id(
        app,
        "open",
        format!("Open {}", crate::APP_NAME),
        true,
        None::<&str>,
    )?));
    items.push(Box::new(PredefinedMenuItem::separator(app)?));
    items.push(Box::new(MenuItem::with_id(
        app,
        "quit",
        format!("Quit {}", crate::APP_NAME),
        true,
        Some("CmdOrCtrl+Q"),
    )?));
    let refs: Vec<&dyn IsMenuItem<R>> = items.iter().map(|b| b.as_ref()).collect();
    Menu::with_items(app, &refs)
}

pub fn install<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let m = model(&Snapshot::default(), SystemTime::now());
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon(m.icon)?)
        .icon_as_template(true)
        .tooltip(&m.tooltip)
        .menu(&menu(app, &m)?)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, ev| {
            let id = ev.id().as_ref().to_string();
            match id.as_str() {
                "quit" => app.exit(0),
                "open" => lifecycle::bring_to_front(&crate::TauriShell(app.clone())),
                w if w.starts_with("waiting:") => {
                    lifecycle::bring_to_front(&crate::TauriShell(app.clone()));
                    let _ = app.emit(OPEN_EVENT, w.to_string());
                }
                _ => {}
            }
        })
        .build(app)?;
    NativeRenderer(app).show(
        &TrayVisual {
            icon: m.icon,
            tooltip: m.tooltip,
        },
        true,
    )?;
    Ok(())
}

pub fn refresh<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let mut cache = RENDERER.lock().unwrap_or_else(|p| p.into_inner());
    let snap = app.state::<crate::state::AppState>().snapshot();
    cache.paint(model(&snap, SystemTime::now()), &mut NativeRenderer(app))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Fake {
        shows: usize,
        menus: usize,
    }
    impl TrayRenderer for Fake {
        fn show(&mut self, _: &TrayVisual, _: bool) -> tauri::Result<()> {
            self.shows += 1;
            Ok(())
        }
        fn menu(&mut self, _: &TrayModel) -> tauri::Result<()> {
            self.menus += 1;
            Ok(())
        }
    }
    #[test]
    fn unchanged_snapshot_paints_once_and_tooltip_only_does_not_rebuild_menu() {
        let mut cache = RenderCache::default();
        let mut fake = Fake::default();
        let mut m = model(&Snapshot::default(), SystemTime::now());
        cache.paint(m.clone(), &mut fake).unwrap();
        cache.paint(m.clone(), &mut fake).unwrap();
        assert_eq!((fake.shows, fake.menus), (1, 1));
        m.tooltip.push_str(" changed");
        cache.paint(m, &mut fake).unwrap();
        assert_eq!((fake.shows, fake.menus), (2, 1));
    }
}
