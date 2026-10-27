//! gui.md §5.8: the menu-bar item, rendered from `tray_model`.

use std::time::SystemTime;

use tauri::image::Image;
use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Runtime};

use crate::lifecycle;
use crate::tray_model::{Icon, Snapshot, TrayModel, model};

const TRAY_ID: &str = "main";
/// Payload: the waiting item's id (`waiting:deps:12`); the window refetches
/// and opens it (gui.md §5.8: fresh data before any dialog).
pub const OPEN_EVENT: &str = "strata://open";

fn icon(i: Icon) -> tauri::Result<Image<'static>> {
    Image::from_bytes(match i {
        Icon::Normal => include_bytes!("../icons/tray/normal.png"),
        Icon::Pending => include_bytes!("../icons/tray/pending.png"),
        Icon::Down => include_bytes!("../icons/tray/down.png"),
    })
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
                format!("{} more in Git Strata", m.more),
                true,
                None::<&str>,
            )?));
        }
        items.push(Box::new(PredefinedMenuItem::separator(app)?));
    }
    items.push(Box::new(MenuItem::with_id(
        app,
        "open",
        "Open Git Strata",
        true,
        None::<&str>,
    )?));
    items.push(Box::new(PredefinedMenuItem::separator(app)?));
    items.push(Box::new(MenuItem::with_id(
        app,
        "quit",
        "Quit Git Strata",
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
        .tooltip("Git Strata")
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
    Ok(())
}

pub fn refresh<R: Runtime>(app: &AppHandle<R>, snap: &Snapshot) -> tauri::Result<()> {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return Ok(());
    };
    let m = model(snap, SystemTime::now());
    tray.set_icon(Some(icon(m.icon)?))?;
    tray.set_icon_as_template(true)?;
    tray.set_menu(Some(menu(app, &m)?))?;
    Ok(())
}
