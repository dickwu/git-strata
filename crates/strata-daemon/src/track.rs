//! The ToolCall path of a checkpoint (§5.4, I8): runs on the request
//! thread after the session queue was taken. The FSEvents callback only
//! ever calls `Inbox::note` (wired by `Tracker::watch`); `hint`, `flush`
//! and dropping a `Watcher` never happen on the callback thread.

use strata_overlay::Tracker;
use strata_types::RepoPath;

/// Paths given → `Inbox::hint` (reads attributes, so request thread only);
/// no path list → `Inbox::require_full`. Then `Tracker::flush`.
pub fn prepare_tool_call(tracker: &Tracker, paths: &[RepoPath]) {
    let inbox = tracker.inbox();
    if paths.is_empty() {
        inbox.require_full();
    } else {
        inbox.hint(paths);
    }
    tracker.flush();
}

/// Start watching (request thread). If the watch cannot start, the next
/// reconcile must be a full scan. Returns whether it is watching.
pub fn watch_or_full(tracker: &mut Tracker) -> bool {
    if tracker.watch().is_ok() {
        return true;
    }
    tracker.inbox().require_full();
    false
}

/// Drop a Tracker (and with it its Watcher) on the calling thread: the
/// request or shutdown thread, never the watcher callback.
pub fn drop_tracker(tracker: Tracker) {
    drop(tracker);
}
