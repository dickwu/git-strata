# Legacy-install fixtures (gui.md §5.7 migration, §7 `legacy_*` tests)

Real samples from peilin's Mac on 2026-10-27, taken from the live legacy
daemon (pid 29858, built from c98794f, `~/.cargo/bin/git-stratad`, label
`com.gwd.git-stratad`) and from the 09:27 incident daemon (pid 91344).
They are the inputs of the P6 tests `legacy_identity_gate` and
`legacy_lsof_copy_count` and of the EMFILE incident row in §6; the tests
read these files, no test talks to a live daemon.

## Redaction (applied uniformly; shapes, counts, fds and addresses unchanged)

| original | fixture |
| --- | --- |
| `/Users/<login>` and the login name | `/Users/user`, `user` |
| `/Volumes/<vol>/<dir>` (the data root's volume and directory) | `/Volumes/DATA/work` |
| the 23 repo names (`repo_waiting` keys, path components) | `repo-01` … `repo-23`, sorted order of the originals |
| the Time Machine share in lsof's stderr warning | `NAS._smb._tcp.local./REDACTED/backup` |

Session ids, pids, fd numbers and kernel socket addresses are kept as recorded.

## Files

| file | source | what it shows | expected verdict |
| --- | --- | --- | --- |
| `daemon_status-c98794f.json` | raw reply to `{"id":"1","method":"daemon_status","params":{"verify":false}}`, 12:02 CDT | `data` keys are exactly `{holds, reconcile, repo_waiting, verify, warnings}`; no `api`, no `version`; `reconcile = {runs: 0, last: null}` | identity gate (4) passes, (3) passes |
| `cli-daemon-status-924a3f3.json` | `git-strata daemon status`, 12:01 CDT | the cli's re-assembled keys (`running / socket / holders / unknown_publishes / alerts`) — **not** usable for the gate | the gate must not read this shape |
| `lsof-daemon-idle.txt` | `lsof -nP -a -p 29858 -U -F pfdn` | 25 unix fds, 23 addresses: listener f9 + 21 leftover `conns` clones (1 each) + another client's live connection `0xe6cb5e1ba54a71a8` ×3 (f50, f67, f69) | fail (an address has 3 copies) |
| `lsof-daemon-one-conn.txt` | same, GUI connection open | 28 fds, 24 addresses: the above + GUI's `0x8e11dba69afedd1` ×3 (f83–f85) | fail |
| `lsof-client-one-conn.txt` | `lsof -nP -a -p <client> -U -F pfdn` | client fd 3 `n->0x8e11dba69afedd1` (peer = daemon-side address) | used by the "no client connection" check |
| `lsof-daemon-after-close.txt` | same, ~1 s after the GUI closed | 26 fds, 24 addresses: GUI's address down to f84 ×1; the other client still ×3 | fail (the other client) |
| `lsof-daemon-settled.txt` | same (taken as `-T fqs` run; unix sockets show no state) | 24 fds, 24 addresses, every address ×1: listener + 21 old leftovers + f67 + f84 | pass |
| `lsof-stderr-smbfs.txt` | stderr of every lsof run above (exit code 0) | the smbfs `can't stat()` warning | not a failure (exit 0) |
| `lsof-91344-emfile.txt` | `lsof -p 91344` (human format), incident, 11:51 CDT | 255 numbered fds against the 256 soft limit: 194 DIR, 25 KQUEUE, 24 unix, 10 REG (5 × `store.db`, …), 2 CHR — the watcher dir/kqueue leak (×25 per watched ancestor) and the conns leak. The 41 `txt` REG rows (`.strata/search/*/gen-*/{index,lookup}.bin`, dyld, shm) are memory maps, not fds | the "old daemon isn't answering" path (§5.7 ③); fd-budget check refuses before connecting |

All daemon-side unix fds carry the bound path and no `->`, the listener
included — which is why the copy-count rule excludes no fd (§5.7 ②).
