//! gui.md §5.8 window behaviour, behind a trait so it is tested with a
//! fake: a second launch (single-instance), the Dock icon and the tray's
//! "Open Git Strata" bring the one window back and restore the Dock icon;
//! closing the window hides it and drops to an accessory (menu-bar) app.
//! Quitting the GUI never touches the daemon.

pub trait Shell {
    fn show_window(&self);
    fn unminimize_window(&self);
    fn focus_window(&self);
    fn hide_window(&self);
    /// `true`: a regular app with a Dock icon; `false`: accessory.
    fn set_regular(&self, regular: bool);
}

/// Second launch, Dock reopen, tray "Open Git Strata": never a new window.
pub fn bring_to_front(s: &impl Shell) {
    s.set_regular(true);
    s.show_window();
    s.unminimize_window();
    s.focus_window();
}

/// ⌘W or the close button: hide, keep running in the menu bar.
pub fn hide_to_tray(s: &impl Shell) {
    s.hide_window();
    s.set_regular(false);
}
