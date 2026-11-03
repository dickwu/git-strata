# Unreleased: menu-bar state and freshness

The menu-bar icon now combines a neutral workspace fork with a distinct colored shape:
red slash (Down), red triangle (Alert), dim ellipsis (Checking), amber dot (Waiting),
violet diamond (Conflict), teal ring (Syncing), or the neutral glyph (Normal). There is
no animation or count beside the icon. Colors adapt to the drawing appearance independently
of the app theme; inactive-display dimming is intentionally not retained.

- The native poller continues while the window is hidden or visible. Stale/missing inputs
  show Checking; slow session/dependency reads do not imply the daemon is down.
- NotResponding requires two counted health timeouts and 20 seconds since its last Ok.
  File-handle shed replies now reach the client as Busy/too_many_open_files, not rpc_id.
- Home and the tray show long non-conflict deferrals and pending config relax requests.
  Conflicts are counted separately as agent work. Daemon shows warning variant names.
- Red alerts can persist until their cause is addressed and the daemon restarts. Waiting
  items remain available, with age captions when retained data is stale. No acknowledge/snooze.
- On older daemons without `fds`, hidden polling uses only one health read every 30 seconds.
  Deps age out after 90 seconds and the icon becomes Checking until reopening or upgrading.
  The pre-fd-fix build still leaks roughly two descriptors per minute under this gentle plan;
  upgrade the daemon to remove that limitation.
- Missing session-level upstream detail can keep Checking indefinitely; Home lists those
  sessions without guessing the cause. Existing daemon RPC params/features remain compatible.

Automated tests and offline color/grayscale previews cover the implementation. Live replicas,
VoiceOver, menu highlight and the 30-minute hidden-window test remain manual acceptance gaps;
this note does not claim those checks or a published release.
