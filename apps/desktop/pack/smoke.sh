#!/usr/bin/env bash
# P-pack smoke test on a fresh GitHub-hosted macOS runner (gui.md §8.1).
# NEVER run this on a working Mac: it installs into /Applications, writes
# ~/Library/Application Support/git-strata/config.toml, registers a LaunchAgent
# and uses real launchctl / SMAppService.
# usage: smoke.sh <Git-Strata_*.dmg> <out dir>
set -uo pipefail
dmg="$1"; out="$2"
mkdir -p "$out"
rec="$out/smoke-record.md"
label="io.github.dickwu.git-strata.daemon"
app="/Applications/GIT STRATA.app"
cfgdir="$HOME/Library/Application Support/git-strata"
sock="$cfgdir/daemon.sock"
uid="$(id -u)"
hard_fail=0

if [[ "${CI:-}" != "true" || "${GITHUB_ACTIONS:-}" != "true" ]]; then
  echo "refusing: smoke.sh only runs on a GitHub-hosted runner (CI=true, GITHUB_ACTIONS=true)" >&2
  exit 2
fi

log() { echo "$*" | tee -a "$rec"; }
step() { log; log "## $*"; log; }
fact() { log "- $*"; }
code() { log '```'; "$@" 2>&1 | tee -a "$rec"; local rc=${PIPESTATUS[0]}; log '```'; return "$rc"; }
must() { if ! "$@"; then fact "**FAIL**: $*"; hard_fail=1; fi; }
wait_for() { # seconds, command...
  local n="$1"; shift
  for _ in $(seq 1 "$n"); do "$@" && return 0; sleep 1; done
  return 1
}

: > "$rec"
log "# Smoke record: $(sw_vers -productName) $(sw_vers -productVersion) ($(sw_vers -buildVersion)), $(uname -m)"
fact "runner image: ${ImageOS:-?} ${ImageVersion:-?}; user $(whoami) uid $uid"
fact "GUI session: $(launchctl print "gui/$uid" > /dev/null 2>&1 && echo "gui/$uid exists" || echo "no gui/$uid domain")"
fact "console user: $(stat -f %Su /dev/console)"

step "1. Install from a quarantined DMG"
xattr -w com.apple.quarantine "0081;$(printf %x "$(date +%s)");Safari;$(uuidgen)" "$dmg"
fact "dmg quarantine: $(xattr -p com.apple.quarantine "$dmg" 2>&1)"
mnt="$(mktemp -d)"
must hdiutil attach -nobrowse -readonly -mountpoint "$mnt" "$dmg" > /dev/null
fact "mounted app quarantine: $(xattr -p com.apple.quarantine "$mnt/GIT STRATA.app" 2>&1 || true)"
log "spctl (expected: rejected, self-signed / ad-hoc is not notarized):"
code spctl --assess --type execute -vv "$mnt/GIT STRATA.app" || true
if command -v syspolicy_check > /dev/null; then
  code syspolicy_check distribution "$mnt/GIT STRATA.app" || true
fi
must ditto "$mnt/GIT STRATA.app" "$app"
hdiutil detach "$mnt" > /dev/null || true
for f in "" /Contents/MacOS/git-stratad /Contents/MacOS/git-strata; do
  fact "quarantine after ditto on \`${f:-GIT STRATA.app}\`: $(xattr -p com.apple.quarantine "$app$f" 2>&1 || true)"
done
log "Stand-in for Open Anyway (the runner cannot click it): remove quarantine from the whole bundle."
xattr -dr com.apple.quarantine "$app"
must codesign --verify --deep --strict "$app"
code codesign -d -r- "$app/Contents/MacOS/git-stratad" || true

step "2. Daemon config for the runner"
root="$HOME/strata-smoke-root"
mkdir -p "$cfgdir" "$root"
printf '[paths]\nroot = "%s"\n' "$root" > "$cfgdir/config.toml"
fact "config.toml paths.root = $root"

step "3. First launch"
exe="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$app/Contents/Info.plist")"
open "$app"
if wait_for 30 pgrep -f "$app/Contents/MacOS/$exe" > /dev/null; then
  sleep 10
  if pgrep -f "$app/Contents/MacOS/$exe" > /dev/null; then fact "app running after 10 s: pid $(pgrep -f "$app/Contents/MacOS/$exe")"
  else fact "**FAIL**: app exited within 10 s"; hard_fail=1; fi
else
  fact "**FAIL**: app process never appeared"; hard_fail=1
fi
fact "crash reports: $(find "$HOME/Library/Logs/DiagnosticReports" -name 'git-strata*' -newer "$rec" 2>/dev/null | wc -l | tr -d ' ')"

step "4. Register the daemon (plan A: SMAppService.agent)"
probe="$app/Contents/MacOS/strata-pack-probe"
code "$probe" status || true
code "$probe" register || true
sleep 2
a_status="$("$probe" status | sed -n 's/.*"status":"\([^"]*\)".*/\1/p')"
fact "plan A status after register: **$a_status**"
code launchctl print "gui/$uid/$label" || true
plan=""
if [[ "$a_status" == enabled ]] && wait_for 30 test -S "$sock"; then
  plan=A
else
  fact "plan A did not bring the socket up; trying plan B (plain plist, absolute path into the bundle)"
  "$probe" unregister > /dev/null 2>&1 || true
  plist="$HOME/Library/LaunchAgents/$label.plist"
  mkdir -p "$(dirname "$plist")"
  sed -e "s|<key>BundleProgram</key>|<key>Program</key>|" \
      -e "s|<string>Contents/MacOS/git-stratad</string>|<string>$app/Contents/MacOS/git-stratad</string>|" \
      "$app/Contents/Library/LaunchAgents/$label.plist" > "$plist"
  code launchctl bootstrap "gui/$uid" "$plist" || true
  if wait_for 30 test -S "$sock"; then plan=B; fi
fi
fact "daemon came up via plan: **${plan:-none}**"

step "5. The daemon answers on its socket"
pid="$(pgrep -x git-stratad || true)"
fact "git-stratad pid: ${pid:-none}; path: $( [[ -n $pid ]] && ps -o comm= -p "$pid")"
[[ -n "$pid" ]] && { code codesign -dv "$pid" || true; }
reply="$("$app/Contents/MacOS/git-strata" daemon status 2>&1)"
log '```'; log "$reply"; log '```'
if grep -q '"ok":true' <<<"$reply" && grep -q '"running":true' <<<"$reply"; then fact "daemon status: ok, running"
else fact "**FAIL**: daemon status did not report a running daemon"; hard_fail=1; fi
if [[ -z "$plan" ]]; then hard_fail=1; fi
if [[ -f "$HOME/Library/Logs/git-strata/daemon.log" ]]; then
  code tail -n 40 "$HOME/Library/Logs/git-strata/daemon.log" || true
fi

step "6. Background items as the system recorded them"
code sudo sfltool dumpbtm 2>/dev/null | grep -i -B2 -A12 'dickwu' || true

step "7. Uninstall"
if [[ "$plan" == A ]]; then code "$probe" unregister || true
else launchctl bootout "gui/$uid/$label" 2>/dev/null || true; rm -f "$HOME/Library/LaunchAgents/$label.plist"; fi
if wait_for 20 bash -c '! pgrep -x git-stratad > /dev/null'; then fact "daemon exited"
else fact "**FAIL**: git-stratad still running after unregister"; hard_fail=1; fi
osascript -e 'tell application id "io.github.dickwu.git-strata" to quit' > /dev/null 2>&1 || true
if ! wait_for 10 bash -c "! pgrep -f '$app/Contents/MacOS/$exe' > /dev/null"; then
  fact "app ignored the quit Apple event; killing it"
  pkill -f "$app/Contents/MacOS/$exe" || true
fi
rm -rf "$app"
fact "launchctl knows the label afterwards: $(launchctl print "gui/$uid/$label" > /dev/null 2>&1 && echo yes || echo no)"
fact "socket left behind: $(test -e "$sock" && echo yes || echo no)"
code sudo sfltool dumpbtm 2>/dev/null | grep -i -c 'dickwu' || true

step "Not testable here (manual checklist in gui.md §8.1)"
fact "TCC Removable Volumes prompt and which name it is recorded under"
fact "Touch ID / LocalAuthentication prompt owner name"
fact "Gatekeeper first-run UX and Open Anyway"
fact "login-item launch detection"

log
log "Result: $([[ $hard_fail == 0 ]] && echo PASS || echo FAIL)"
[[ -n "${GITHUB_STEP_SUMMARY:-}" ]] && cat "$rec" >> "$GITHUB_STEP_SUMMARY"
exit "$hard_fail"
