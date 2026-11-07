# P1 addendum: auto-close after a full publish (t182u), r10

Addendum to design-r7.md for the requirement in impl/autoclose-req.md (peilin t182u: "archiving is part of merging
code; after publish merges into main, auto-archive/close the session"). It is implemented on the P1 branch
(impl/brief.md: feat/session-mark-p1, worktree git-strata-wt-mark, committed as 8d9663c per impl/RESUME.md) after the
P1 writer, so it assumes P1's state model as that branch has it (a `session_marks` row `{state: 'done', note,
by_kind, at_ns}`, no `reason` column, no `session_exports`, no `archived` state, no §5.4 head export, no §8 sidecar).
Citations are `file:line` **at 8d9663c, read from the worktree's files** (r3: r2 cited 695f3cd, which put every
`handlers.rs` line 18 too low; Claude r2 nit 9). `main` is at 695f3cd (the main checkout's `.git/refs/heads/main`)
and 8d9663c is not in it, so the P1 branch is unmerged (§4). Design only; nothing run. r1 is p1-autoclose-r1.md, r2
is p1-autoclose-r2.md, r3 is p1-autoclose-r3.md, r4 is p1-autoclose-r4.md, r5 is p1-autoclose-r5.md, r6 is p1-autoclose-r6.md, r7 is p1-autoclose-r7.md, r8 is p1-autoclose-r8.md, r9 is p1-autoclose-r9.md; r3 answered
autoclose-r2-codex.md (three blockers, two nits) and autoclose-r2-claude.md (no blockers, nine nits); r4 answered
autoclose-r3-codex.md (two blockers, three nits) and autoclose-r3-claude.md (no blockers, ten nits); r5 answered
autoclose-r4-codex.md (one blocker: an earlier unlanded publish escapes a later session-wide close, §2.1; two nits) and
autoclose-r4-claude.md (no blockers, ten nits); r6 answered autoclose-r5-codex.md (two blockers, both a writer that
holds only the repo lock acting after the close's proof, §3.2; two nits) and autoclose-r5-claude.md (no blockers, eleven
nits); r7 answered autoclose-r6-codex.md (one blocker: clause 0 is read before the queue place, so a concurrent `add`
bypasses a per-repo opt-out, §1; two nits) and autoclose-r6-claude.md (the same blocker; fourteen nits); r8 answered
autoclose-r7-codex.md (one blocker: a concurrent `assign` survives a successful close and strands a conflict path, §3.2) and
autoclose-r7-claude.md (no blockers, ten nits); r9 answered autoclose-r8-codex.md (one blocker: an untracked sibling's own deps draft escapes clause 6 and the
transaction that commits `Removing`, §3.2; one nit) and autoclose-r8-claude.md (no blockers, nine nits); **this r10 answers autoclose-r9-codex.md (one blocker: a session's own deps draft inserted after `Removing` vanishes with the close, §3.2) and autoclose-r9-claude.md (no blockers, seven nits)**. §12 is the r1 → r2
changelog, §13 the r3 changes, §14 the r4 changes, §15 the r5 changes, §16 the r6 changes, §17 the r7 changes, §18 the r8 changes, §19 the r9 changes, §20 the r10 changes. r4's
new citations (audit.rs, confirm_flow.rs, startup.rs, locks.rs, du.rs, strata-publish gate.rs and push.rs, strata-deps
proposal.rs, strata-session new/mod.rs, periodic_fetch.rs, sync_move.rs, daemon upstream.rs, strata-gitio remote.rs, the
CLI's confirm.rs and sync.rs, strata-types config.rs and its tests), r5's (strata-store rm_state.rs and records.rs,
strata-publish pending.rs, main_follow.rs and message.rs, strata-gitio replay.rs and upstream.rs, daemon resolve.rs's
replay and chain paths) and r6's (strata-store sessions.rs's lifecycle edges, deps.rs, schema.rs, txn.rs and retire.rs,
strata-publish rebase.rs's gates and seams.rs, strata-gitio refs.rs, strata-session sys.rs and rm_note.rs, daemon
catalog.rs, reload.rs and handlers.rs's `handle` and `deps_propose`, the CLI's tests/sync.rs) and r7's (strata-session new/add.rs,
seams.rs and move_workspace.rs's Phase F, strata-overlay scan.rs, compute.rs and rebase.rs's three-way arms, strata-publish
stage.rs and `assignments`, strata-store open.rs, strata-deps seams.rs, the CLI's args.rs, devports.rs, rpc.rs and
tests/support/live.rs) and r8's (daemon `Params::Assign` and `record_rm_attempt`, strata-session catalog.rs and rm_note.rs's
`attempt_summary`, the CLI's ro.rs, help.rs and tests/confirm_gate.rs, skills/git-strata/SKILL.md, docs/modules/daemon.md) and r9's (strata-store main_sync.rs and gc.rs's proposal set, strata-deps rollout.rs and drift.rs, strata-overlay checkpoint.rs,
strata-gitio index.rs, the CLI's cmds.rs `repo_for` and cwd_guard.rs, README.md's path table, skills/git-strata/reference.md, docs/modules/cli.md) and r10's (strata-store errors.rs and devports.rs, strata-deps build.rs and the same crate's rollout.rs `ensure_target`, the tests strata-store sessions.rs and deps.rs and strata-deps proposals.rs and rollout.rs, apps/desktop/src-tauri lib.rs, docs/modules/deps.md and store.md) are read from
the same worktree at 8d9663c; r8 also cites design-r7.md §8 and brief.md from these notes. `main` is still 695f3cd at r10, and the branch still 8d9663c (the main checkout's
`.git/refs/heads/main`).

**Principle, stated once: the close is today's `rm` without `--discard`, run by the daemon under the same queue,
with every refusal `rm` has today and the refusals that make it strict (§3.1): an unpublished `rm.regenerable` entry
refuses it (r2); an entry whose content only a historical publish holds refuses it, because the only discharge is a
projection onto the current landed target (r3); a kept pre-move tree that still exists refuses it, changed or not, and so
does a nested repository the cold scan warns about (r7, §3.1); and for every `rm` a kept pre-move tree is compared again
after the holders scan (r3, §3.2). A refusal marks the session done with the reason and leaves it open; a failed read or a
transient is not a refusal and marks nothing (r4, r7, r8, r9, §4). **Every decision to close rests on state read under the session's
queue place, clause 0 included (r7, §1), and every clause 6 read is final: `assign` needs that place (r8, §3.2), and proposals and
conflicts are read again in the transaction that commits `Removing` (r6, §3.2). A session's own proposals are read with no tracked filter, because `Removed` hides its
draft in any repo (r9, §3.2), and no draft of its own can be inserted once `Removing` has committed, because the insert reads the lifecycle in its own transaction (r10, §3.2).** No path in this addendum discards,
prompts, or deletes a ref other than the session's own pin, which every `rm` deletes last (remove.rs:421-430) and
which clause 4 has made redundant before the close starts, for every repo of the manifest, untracked siblings
included (r3), by proving against `origin/<default_branch>` — the branch the requirement names, and the strict judge's
only discharge anchor, so a repo that publishes to another branch is marked done, never closed, on that publish (r4) —
the workspace base, every publish this session ever made in that repo and every target its `rebase_pending` row holds,
whatever the trigger is and whether or not the overlay is empty (r5, §2.1). A strict close deletes only the
`rebase_pending` rows it has proved, and refuses if they changed before `Removing` (r5, §3.2); clause 6's proposals and
conflicts are read again in the transaction that commits `Removing`, and after that commit no writer can add a target, because each
checks the lifecycle in its own transaction (r6, §3.2).** The invariant of §1.4 (no agent-alone discard) is untouched because the daemon calls
`Sessions::remove_strict(id)` (§3; a non-discard remove with no token parameter at all) and nothing else. r2 also
changed `Sessions::remove` itself for every non-discard caller, agents' `rm` included: Phase B gained a post-scan
recheck (§3.2), which r3 extends to kept old trees. Its guarantee, stated once and used everywhere (Codex r2 nit 2):
**a write that `compute`'s walk classifies, or that lands in a kept old tree, and that finished before the first
holders scan, is either in a checkpoint or refuses the remove**; ignored paths, deps directories and `.git` are
outside it, as today, and so are a nested repository's new files, which the walk never lists (scan.rs:124-139): a strict
close refuses on the scan's warning instead (r7, §3.1).

## 1. Where the hook goes

| Trigger | Where | Which sessions | Why there |
|---|---|---|---|
| `publish` | The handler's success path, handlers.rs:2031-2143: after the per-repo loop (:2074-2132) and the comment "`publish_bound` has finished its post-publish group rebase here" (:2133), **before** the reply is built (:2138-2142). The hook replaces the `upstream_best_effort` map of :2134-2137 for the sessions it closes (§6). | **Two sets, judged in this order, each session under its own queue place.** (a) Every session of the group, `upstream_sessions` (:2042-2043), in id order (a `BTreeSet`), one after another; a session's outcome never changes another's. **r8: the handler reads each group session's manifest here, at entry, as the baseline of clause 0b (§2).** (b) r3 (Claude r2 nit 4): for every repo R this call pushed — `published[R].push` is `Pushed` (:2111; push.rs:666-668) **or, r4 (Claude r3 nit 2), `already_published[R].push` is `Pushed` (:2539 copies the row's `push`; r6 refreshes a `Refused` that has landed since, §6): the push of an earlier run that then failed on a later repo and took the error path, whose hook never ran; the re-run is the first success path that sees it** — **and R publishes its default branch (`publish.branch == default_branch`; r4, §2 clause 4: a push of a split-branch repo moves `refs/strata/remotes/origin/<publish.branch>` only (push.rs:289-303), not the ref the proof reads, so nothing a re-judge could find has changed)**: **the union over those repos (r8, Claude r7 nit 10), so a session holding two pushed repos is judged once, of** every live session **marked `done`** (any `by_kind`) that has R in its manifest and is not in the group: exactly the `push` trigger's set, judged the same way; these sessions never enter the reply's `closed` (§6). **r5 (Codex r4 nit 1): the set is an enumeration read with no lock; each member's `done` mark is read again under its queue place, and one no longer marked is not judged (the per-session sequence, below).** Such a session may be waiting on this very push (`not_pushed`, clause 4) and nothing else would re-judge it: a later `git-strata push R` answers `NoCommit` once the chain no longer leads (push.rs:292-297). | Every publish row of the group is `Done` (strata-publish push.rs:892-915) and every session's post-publish rebase has returned (:852-863: a failure there makes the row `Unknown` and the call an error, :877-891), so the workspace is on the publish commit (`Moved`) or still on its old base with the landed entries in its overlay (`Deferred`/`Waiting`/`OpenFiles`, rebase.rs:37-66); both are what `rm` judges today, and both have the built base the strict judge projects onto (§3.1). **The handler holds no queue and no repo lock here**: the rebases took and released theirs one session at a time (push.rs:848-851), `with_publish_ctx` (:2094-2102) has returned, and the only thing the request still holds is its catalog admission, under which `with_queue_not_publishing` nests (resolve.rs:98 `core.admit()`). |
| `publish` error path | None. :2127-2130 returns the decorated error; nothing is judged, nothing is marked. The agent re-runs the same command and reaches the success path (`already_published`, :2115-2126). | — | A half-landed group is the agent's to finish, not the daemon's to mark. |
| `push`, both forms | **After the repo lock is released, never inside it** (r2; r1 had it inside `core.locks.with_repo`, today's :2225-2260, §12 item 1). Confirmed form (handlers.rs:2225-2260, taken when a token was issued or `rc.needs_human_confirm()`, :2222-2224): the closure returns `(PushOutcome, tip)` instead of the reply JSON of :2259; after :2260, **on `Ok`**, the handler runs the hook, then builds the same JSON plus the §6 fields. **r10 (Claude r9 nit 6): r9 said "when the outcome is `Pushed`", as if the outcome were still to be tested. `push_to` answers `Ok` for `Pushed` only, and `Refused` and `Unknown` arrive as `Err` (strata-publish push.rs:328-333; the `r?` of :2259).** An `Err` (including `Unknown`, :2253-2255) propagates as today and runs no hook. Headless form (`push_headless`, :2271-2339): its lock closure is :2283-2288; the hook runs after `rebase_replayed` (:2305-2307) and `main_sync::reconcile` (:2315-2317), right before the reply (:2319-2326), when `r` is `Ok(tip)`. | Only live sessions **marked `done`** (any `by_kind`; store read `mark`, crates/strata-store/src/marks.rs:18-57) that have the pushed repo in their manifest, **and only when the pushed repo publishes its default branch (r4): a `push` of a split-branch repo lands nothing on `origin/<default_branch>` (§2 clause 4), so the hook does not run and the reply has no `closed`/`auto_close`**. A session without a mark is work in progress; a publish is its "done" signal, a push is not. **r5 (Codex r4 nit 1): the mark is read again under the session's queue place (the per-session sequence, below).** | A `push = false` repo (PublishTarget, strata-types model.rs:97-103; push.rs:351 `push_false_leaves_chain_local`) lands only when a human pushes; without this the §2 clause `not_pushed` would hold such sessions open for ever. Note (Claude r1): the confirmed form runs no rebase and no main-follow; the sessions are in whatever state the publish left them, and the judge takes both (§2 clause 6's note). The recording push moves `refs/strata/remotes/origin/<branch>` in the push transaction (`send`, push.rs:289-303 → gitio `push_published`), so clause 4 passes for a session on the chain the moment the lock closure returns. |

Why the lock matters (both r1 blockers): `remove` passes Phase A and Phase B without a repo lock (the BeforeRemove
checkpoint goes through `DaemonBefore`, startup.rs:232-238, which takes nothing), sets `Removing`, retires the root
and the shadow repos into the trash (remove.rs:384-416), **then** unpins under `with_cleanup_lock` (:421-430).
That lock is `Locks::with_cleanup` → `repo_take` (locks.rs:748-754 → :416-442), which answers `Busy reason:
"reentrant"` when this request already holds the repo (:435-442: the repo is in the request's `CTX.repos` until the
`with_repo` closure returns, :453-468). `with_cleanup_lock` retries every `Busy` that is not `pins_tightened` with no
deadline (strata-session lib.rs:294-315, :305-307). A hook inside the closure would therefore spin for ever holding
the repo lock and the session's queue place, with the row `Removing`, the root in the trash and the pin still
present, and every later operation on the repo `Busy lock_wait` until a restart. After the closure returns, the
repo has left `CTX.repos` (the `Held` drop at :458-468) and the cleanup lock is takeable. Lock order is then the one
the rest of the daemon keeps, session queue → repo lock: `publish`'s rebases (push.rs:848-851), `rm` (handlers.rs:1246
→ remove.rs:424), and now the hook. `reentrant_repo_take_is_refused_not_deadlocked` (crates/strata-daemon/tests/
locks.rs:528-532) is the unit test of the hazard; §10 row 2 adds the handler-level test.

Serialization is the `Rm` arm's, verbatim (handlers.rs:1241-1298): `with_queue_not_publishing(core, s, …)`
(:1246; resolve.rs:97-100: wait out the session's `publishing` flag, :41-47, holding nothing, then its queue place,
the flag checked again under it, `Busy reason: "publishing"`, :59-62), then `with_du_hold` (:1252; fn :1907-1911),
`upstream_best_effort` read **before** anything moves (:1253; fn :961-980, panics caught), the `before` row (:1254),
`trackers.drop_session` (:1255), `writing()` (:1256), the remove (:1257), `forget_old_trees` (:1258),
`record_rm_attempt` on error (:1259-1261; fn :184-199), `du.on_remove` (:1275). Factor :1252-1296 into one
`Server::remove_session(&self, s, call: RemoveCall) -> (Result<RemoveReport>, Option<Upstream>)` with
`enum RemoveCall { Agent { discard: bool, token: Option<ConfirmToken> }, AutoClose { upstream: Upstream } }`, used by `Rm` (`Agent`) and
by the hook (`AutoClose` → `sessions.remove_strict(s)`, §3.1), so the two paths read alike (§8's rule) and the hook
cannot grow a `discard` argument. **r9 (Claude r8 nit 9): clause 2 has already read `upstream_best_effort`, a `ReadMode::Full` read (:965), under the same queue place, so `AutoClose` carries that read
and :1253 runs for `Agent` only. A close reads upstream once.** A static test asserts the hook module (`crates/strata-daemon/src/auto_close.rs`)
never names `RemoveCall::Agent`, `discard`, `RemoveDiscard`, `require_validated`, `ConfirmToken` or `remove(`.
Implementation note: `publish_calls_publish_not_stage` (crates/strata-daemon/tests/resolve.rs:851-861) scans the
text of `handlers.rs` for `strata_publish::stage`, `stage(` and `Staged`, so the helper and the hook's call sites in
`handlers.rs` must not spell those (the judge's own code lives in `auto_close.rs` and strata-session, which it does
not scan).

**The per-session sequence (r5; Codex r4 nit 1, Claude r4 nit 3).** The sets of the table are enumerations:
`store.sessions()` and `store.mark(s)` read once, in id order, holding nothing. They name candidates, and for set (b) and `push` the enumerated
row is clause 0b's baseline (r8). Nothing is decided from them: step 1's early `disabled` (r7: r6 said "nothing" while deciding that, which hid the blocker traced below)
now reads its own row (r8). For each candidate, one
after another:

1. **`store.session(s)` read at this step, and for an `Active` row only (r8, Claude r7 nit 10: any other row goes on to step 3, which names it, except a `Removed` one, which ends here (r10, below), so a group session
   `Removed` by now with an opted-out repo reads `gone`, not `disabled`):** clause 0 over that row's manifest, from the hook's config (§5),
   holding nothing: **an early exit, not the
   decision (r7)**. A `false` of the pinned snapshot stays true for the whole run: a repo leaves a manifest only when gc deletes its workspace row
   (`workspaces_delete_gc_only`, schema.rs:390), and the snapshot is the request's own (§5). A `false` of the current catalog may be undone by a
   later reload, and refusing on it is still safe (r8, §5). So `disabled` can end here, before
   any wait, and an opted-out candidate costs set (b) no lock wait and no `busy` line. A `true` decides nothing. The seam
   `auto_close.before_judge` sits after this step and before step 2, holding nothing (§10 rows 16, 20).
   **r10 (Claude r9 nit 3): a `Removed` row ends the sequence here, with no queue take.** That state is final, and the take would not always say so: `check_session` fences a row of
   any lifecycle (locks.rs:345-353, :547), so under a tightening reload a group session that had already gone read `busy` instead of `gone`. A group session gets `gone` (§6) and its audit line
   (step 6). One of set (b) or `push` is dropped, as at step 3. A session removed after this read, in that same reload's window, still reads `busy`: nothing is judged or marked, and
   the agent's next command answers `NoSuchSession`, so no second read is added.
2. `catch_unwind(AssertUnwindSafe(|| with_queue_not_publishing(core, s, …)))` (r6, below). Everything in steps 3-5 runs
   inside that closure. No publish of `s` can be in flight or start: `record_publish` runs under the session queue (strata-publish push.rs:770-778) and the `publishing`
   flag of a running one is checked under the place (resolve.rs:115-118). `mark`, `rm`, `sync` and **`add` (r7)** of `s` are
   excluded too (handlers.rs:1126, :1246, :1496; sync_move.rs:434), so the manifest step 3 reads is the one clause 7 removes. **So is
   `assign` naming `s` (r8, §3.2), so no assignment can reach `s` once clause 6 has read.**
3. **The reread.** `store.session(s)` (clause 1) and, **for set (b) and the `push` trigger, `store.mark(s)`, which must
   still be `Some(Done)`, any `by_kind`.** A `mark active` that ran between the enumeration and this queue place deleted
   the row under this same queue (marks.rs:89-92; handlers.rs:1126). The session has left the set: no judge, no mark (the
   hook must not bring back a row the agent just deleted), no audit line, no entry in `sessions` or `rejudged`, one
   `tracing::debug!`. A session found `Removed` here is dropped from those two sets the same way: nobody is waiting on
   its answer. Set (a) reads no mark, since a publish is its own signal; a `Removed` group session is reported as §6 says
   (`gone`, read as closed). A session marked `done` after the enumeration is not judged in this run; the next trigger
   judges it, as for a mark written just after the push. **Then, for an `Active` session, clause 0 again (r7, Codex r6
   blocker, Claude r6 blocker 1): over the reread `rec.manifest.repos`, with the pinned snapshot and, r8 (Claude r7 nit 3), the
   current catalog. Any `false` in either, or a repo either
   lacks, gives `disabled {repo}`: no mark, no audit line, no judge, nothing touched (the helper's Tracker drop and
   `writing()` are in step 5). This is the read the close rests on.** **Then clause 0b (r8, Claude r7 nit 4; §2): a repo in the reread
   manifest that the baseline lacks gives `extended {repos}`: no mark, an audit line, no judge, nothing touched.**
4. Clauses 4, 2, 3, 5, 6 (§2), all read fresh under the place. **One `store.publishes()`, read here, serves clauses 4 and 6 (r8,
   Claude r7 nit 9; §2.1, §3.2). The `Upstream` clause 2 reads goes on to step 5 (r9, Claude r8 nit 9).** A failing clause gives `reasons` and, unless §4 says it marks
   nothing, the mark of §4, and the judge does not run. A new `test-seams` hit, `auto_close.after_clauses`, sits at the end of this step (§10 row 22).
5. `remove_session(s, RemoveCall::AutoClose { upstream })` (clause 7). A refusal gives its code and the mark of §4. **A `Dirty` that only a failed
   read explains is no refusal: `error`, no mark (r8, Claude r7 nit 2; §4).** **On any `Err`, the hook
   reads the lifecycle again before it reads the code (r7, Claude r6 nit 4; §2 clause 7):** not `Active` is `unknown`; `Active`
   goes by the code, and a code that is no refusal is `busy` or `error`, which mark nothing (§4).
6. After the closure, **with the queue place released and no lock held (r6, Codex r5 nit 2)**, the audit line (§7). That is
   how a `busy` from step 2, which never held a place, gets its line, and so does a `gone` from step 1 (r10).

The reread and the mark's write sit in one closure under one queue place, so `mark active` is ordered before both or
after both. §4's note-ownership read is in the same closure.

**Why clause 0 is read again (r7, Codex r6 blocker, Claude r6 blocker 1).** r6 ran it only in step 1, and step 3's reread covered
clause 1 and the mark. `add` changes the manifest under the same queue place the hook is waiting for:

| Step | What happens | Source |
|---|---|---|
| 1 | S holds `tool`. `front` has `publish = { auto_close = false }`. S's publish of `tool` lands, and its hook passes step 1 over `{tool}`, holding nothing. | §1; handlers.rs:2133 |
| 2 | `add` of `front` holds S's queue across all of `Sessions::add`: it builds the workspace, then commits the row while S stays Active. It needs no `publishing` check (`with_sessions`, not `with_queue_not_publishing`). | handlers.rs:1494-1499; strata-session new/add.rs:119, :163-168; strata-store sessions.rs:577-608 |
| 3 | The hook takes the place. Its reread now holds `{front, tool}`. `front` is clean, with a head from `resolve_heads`, so clauses 4, 2, 3, 5, 6 pass. | new/add.rs:108-109; §2 |
| 4 | Clause 7 retires the whole session root and unpins every repo of the manifest. | remove.rs:138, :384-416, :421-430 |

No work is lost, but §5's "one `false` disables the hook" is false, and the agent that just added `front` gets `NoSuchSession` on
its next command. If `front`'s head sits on an unpushed chain, clause 4 fails instead and the hook writes a mark §5 forbids for an
opted-out session. Set (b) widens the window, because candidates are enumerated once and judged one after another. Step 3's second
read settles the opt-out: `add` needs the place the hook then holds through clause 7, so the manifest cannot change after that read.

**A session that grew during the run is not closed (r8, Claude r7 nit 4).** r7 said that read "closes all of it", which overclaimed. With
`front` at `auto_close = true`, the same steps end in a close: `front` is clean, every clause passes, and the agent that just ran `add` gets
`NoSuchSession` on a session it had just extended. Nothing is lost, because an edit in the new workspace would refuse as `Dirty`, but the
close overrides the session's latest explicit signal. `add` also leaves a daemon mark in place (handlers.rs:1494-1515), so a later push can do
the same. Two small rules cover both:

- **Clause 0b (§2):** a repo in the reread manifest that the baseline lacks gives `extended`, with no judge and no mark (§4). The baseline is the
  manifest at the handler's entry for set (a), so a long publish's whole run is covered, and the enumerated row for set (b) and `push`.
- **`add` clears a daemon-kind mark under the place it holds (§4).** A session `add` has extended is work in progress again, so the next push
  does not judge it. An `add` that races the enumeration drops out at step 3's mark reread.

Two cases stay as they are, on purpose (§11 item 47). A clean repo added before the baseline was read is part of the session the publish
judges. An agent's, cli's or gui's own mark survives an `add`, and §8 tells that writer to run `mark active`.

**The three ways out of step 2 (r6; Claude r5 nits 4 and 5).** The closure never returns `Err`: every failure inside it,
its own reads' and the judge's, becomes an outcome in steps 3-5. A refusal (a failing clause, `Dirty`, `OpenFiles`) marks; a
`Code::Busy` is `busy` and anything else is `error`, and those two do not (§4, r7). So the result of the call says whether the
closure produced one:

| Result of step 2 | Meaning | Reply and mark |
|---|---|---|
| `Ok(Ok(outcome))` | The closure ran. | What steps 3-5 decided; any mark was written under the place. |
| `Ok(Err(e))` | The wait or the take failed and the closure never ran: `Busy` (§4), `ReadOnly shutting_down` (locks.rs:385-386), or a failed read of the `publishing` flag (resolve.rs:114, :116). | `busy`, or `error {code}`. **No mark**: no place is held, so the write could race `mark active` (handlers.rs:1126). `mark: {written: false, skipped: "busy" \| "error"}`. |
| `Err(panic)` | A panic, before or inside the closure. The lock guards released the place as the stack unwound, the same path a panic takes to `handle`'s `catch_unwind` today (handlers.rs:1050). | `error {code: "handler_panicked"}`, `skipped: "error"`, no mark, a `tracing::error!`, the audit line. |

Why the hook catches its own panics: `handle` answers `Unknown handler_panicked` once `wrote` is set (handlers.rs:1050-1062),
and `publish` has set it (:2092), so a panicking hook would report a landed publish as `Unknown`.
`upstream_best_effort` catches for the same reason (:969). The enumeration and the assembly of the reply sit under a second
`catch_unwind`; a panic or an `Err` there gives `auto_close: {closed: false, reasons: ["error"], rejudged: [], sessions: {}}`
and the publish's own reply stands. A panic after `Removing` leaves the row `Removing` for `recover`, as a panic in `Rm` does
today (remove.rs:457-458).

**How long the hook can wait (r3; Claude r2 nit 2, checked against the code).** `with_queue_not_publishing` is
bounded by one lock wait end to end, inside a publish or push request too. The flag wait (resolve.rs:64-72, via
:51-57 and :114) runs against one absolute deadline; the queue take goes through `with_sessions_until`
(locks.rs:499-501), whose bound is the remaining deadline **as long as the request has made no visible write**
(:536-542, `Some(d) if !is_visible()`); and the post-take check (resolve.rs:119-122) refuses a late start with
`Busy lock_wait deadline: true`. `is_visible()` reads `Ctx.visible` (locks.rs:43-47, :97-100), which only
`mark_visible()` sets (:87-95), and only `new` (handlers.rs:1477) and `add` (:1502) call it; publish and push never
do. `writing()` (handlers.rs:47-49) sets the separate `WRITING` cell that `handle` folds into the reply's `wrote`
(:1049-1051) and is not visibility, so the nit's premise ("the publish and push have already called `writing()`, so
the deadline is ignored") does not hold. What the request's earlier lock takes do change is `Ctx.took`
(:108-110), which only drops the *first-take* bound of `take_bound`, and `with_sessions_until` does not use that
bound while the deadline applies. The hook holds nothing while it waits, so the bound is latency, not deadlock
safety; §9 row 1 words the outcome (`busy`, no mark, §4).

**That bound covers the queue and nothing after it (r7, Claude r6 nit 8).** `take_bound` bounds only a request's first take
(locks.rs:104-113), and a publish or push has taken its locks by then. So inside the closure `collect_session_leftovers`'s
`with_lock` (remove.rs:700) and the unpin's `with_cleanup_lock` (remove.rs:424; strata-session lib.rs:294-315) wait for as long as
the repo's holder runs, and so does every set (b) session judged on another session's reply. That wait is latency on the reply, not
a deadlock: the hook holds one session's place and no repo lock when it starts it, lock order stays session queue → repo lock, and
the CLI's `publish` and `push` have no reply deadline (rpc.rs:130-146 reads until the line arrives or a signal; neither command
takes `--timeout`, args.rs:160-179).

## 2. The landed predicate

Judged per session over **every repo of its manifest** (`rec.manifest.repos`, tracked and untracked), not only the
repos the publish touched: a sibling repo with its own unpublished edit, or a base nothing durable holds, keeps the
session open. Clauses are numbered by topic (the numbers are the reviews' anchors) and **evaluated in the order 0, 1, 4, 2, 3, 5, 6,
7** (r4, §11 item 24: clause 4 reads git and the store only, and its codes are the specific ones — a base or commit
off the default branch is `not_pushed` or `not_merged` there, where clause 3 would only say `ahead`); the first that
fails names the outcome. The judge runs only when the cheap part passed (r5 says so; r4's wording read as both), so
`reasons` is either every failing cheap clause in that order or the judge's refusal. The cheap clauses read the store,
git and `upstream_best_effort`; the judge is `remove_strict` itself. Clauses 1 to 7 run under the session's queue place
(§1, the per-session sequence). **Clause 0 runs twice (r7): over the enumerated manifest holding nothing, as an early exit, and
over the manifest reread under the place, right after clause 1, which is the decision.** **Clause 0b (r8) runs right after that second read.**

| # | Clause | Source | Reason code / `detail` |
|---|---|---|---|
| 0 | Every repo of the manifest resolves `publish.auto_close = true` (§5). **The manifest is the one reread under the queue place; the earlier read over the enumerated manifest is only an early exit (r7, §1).** | One `core.snapshot()?.config`: the catalog generation this request pinned on entry (handlers.rs:1046; startup.rs:436-438; r6, Claude r5 nit 10, and r7, Claude r6 nit 14: r6 called it "current"; §5), **and, r8 (Claude r7 nit 3), `core.catalog.current().config` (the read of periodic_fetch.rs:91 and main_sync.rs:208), so a `false` set while the publish ran is honoured**; a repo either generation lacks counts as `false`. | `disabled` `{repo}`. No mark, no audit line, no judge; the reply still carries `sessions[<sid>]` with it so the skill can branch on it (§8, r4). |
| 0b | **r8 (Claude r7 nit 4).** The manifest reread under the place holds no repo the baseline lacks. The baseline: for set (a), the manifest read at the handler's entry (next to handlers.rs:2042-2043; an unreadable row reads as empty, so the comparison fails closed); for set (b) and `push`, the enumerated row. | `add` commits its row under the queue place the hook waits for (handlers.rs:1496-1499; new/add.rs:163-168), and a repo leaves a manifest only at gc (schema.rs:390), so a difference can only be an `add` during this run. Nothing was judged, so nothing is marked (§4, §1's paragraph on a session that grew). | `extended` `{repos}`. No mark, an audit line, no judge; the reply carries it under `sessions[<sid>]` and `mark.skipped: "extended"` (§6). |
| 1 | The session row is `Active`. | `remove` refuses the rest itself (remove.rs:138-154: Removed → `NoSuchSession`, Creating/Removing → `InvalidInput lifecycle`; **r8, own correction: r7 listed `Unknown` here too, but `Agent` lets it through to `dirty_check`, :143, :524-526; strict refuses it, §3.1**), but the hook reads the row first, under the queue place (r5; r4 said before the queue, which left the read stale): an `Unknown` session is never auto-marked (its scene is the record, `remove_unknown` :459-470). | `gone` (Removed meanwhile; reported as closed for a group session, §6, r5) / `lifecycle` `{state}` |
| 2 | `upstream_best_effort(s)` is `Some`. | handlers.rs:961-980 (`None` on any error or panic). | `upstream_unavailable` (a failed read, not a refusal: no mark, r7, §4) |
| 3 | For every repo in `upstream.repos`: `ahead == 0` and not `unchecked`. | `RepoUpstream.ahead` (strata-types upstream.rs:19-20) is the `base_only` count of `ws.head` against the tip (daemon upstream.rs:131-134, :367); `unchecked` is a graph that could not be computed (:126, :371). `upstream.repos` holds only tracked workspaces whose health is `Ok` and whose scene is not live (:328-340: health :329-331, scene :332-337, **untracked dropped at :338-340**); an unhealthy workspace it drops is refused by the judge (`workspace_missing`/`workspace_unknown`, remove.rs:528-533), and an untracked one by clause 4 and clause 7 (untracked edits). **r10 (Claude r9 nit 2): r9 said every dropped workspace is refused, and one is not. A `Health::Ok` workspace with a live scene (`workspace_scene_live`, strata-store main_sync.rs:108-115) passes `unhealthy`, which tests only `Health::Unknown` and a missing directory or shadow (remove.rs:488-505), and clauses 3 and 6 (a), which read `upstream.repos`, say nothing about it. There is no hole: the judge's cold reconcile still reads it (:550-558), clause 4 proves its commits, and line 3 of §3.2 reads its `conflicts` and proposals with no health filter.** The tip `ahead` is counted against is `main_tip` (:77-78): the chain top under `pin_unpushed` for a repo that publishes its default branch, else `origin/<default_branch>` (proposal.rs:386-396), so a `Moved` split-branch session (base = its release commit) is `ahead` here too; clause 4, evaluated first (r4), names that `not_merged`. | `ahead` `{repo, ahead, base_only}` / `unchecked` `{repo}` |
| 4 | For **every repo of the manifest, tracked or not** (r3, Codex r2 blocker 1; r2 said "tracked repos of the manifest"): the workspace base is on **the default branch**: `git.is_ancestor(ws.head, L)`, where `ws.head` is the `WorkspaceState.head` every workspace has, tracked or not (strata-types model.rs:127-132), and `L` = `refs/strata/remotes/origin/<default_branch>` (`remote_ref("origin", &cfg.default_branch)`, strata-gitio remote.rs:10-13; the ref `main_tip` falls back to, proposal.rs:395) — **r4 (Codex r3 blocker 1, Claude r3 nit 1): r3 tested against `origin/<publish.branch>`, D57's anchor (remove.rs:981), which is the same ref only when `publish.branch == default_branch`**. Equal counts (`is_ancestor` of itself). **r5 (Codex r4 blocker, §2.1): the same anchor for every commit the close would abandon, read from the store and not from this call's reply: every publish row naming this session and repo (its commit, or that commit's replayed copy), and the repo's `rebase_pending` row (`head` and `planned_head`, whatever the `source`), independently of the trigger and of whether the overlay is empty. r4 tested `published[R].commit`/`already_published[R].commit` of this call, which missed earlier calls (the blocker) and also tested commits of another session's batch (Claude r4 nit 2).** An unreadable ref or `ws.head` fails the clause. The code names the cause by the repo's branch pair: `publish.branch == default_branch` (the default form, and the only one a repo with `deps_dirs` may have, config.rs:578; tests/config.rs:359-382) → `not_pushed`, because L is the ref the recording push moves (push.rs:289-303 → gitio `push_published`), so the next push lands the failing commit **if the chain holds it (`detail.on_chain`, r5, Claude r4 nit 1; §2.1)**; `publish.branch != default_branch` (a split-branch repo; supported: tests/config.rs:383-391 `release` with `pin_unpushed = false`, :760-761; strata-publish tests/chain.rs:198-228; strata-session tests/new_layout.rs:540-551) → `not_merged`, because the commit is on `origin/<publish.branch>` and no push puts it on L. `what` says which test failed (`base`, `publish`, `pending`, `planned_replay`, `planned_publish`; §2.1), `head`/`commit` the oid. | Why untracked siblings too: `new` gives every member a workspace, tracked or not (`members` vs `tracked`, new/validate.rs:17-21, D51; new/mod.rs:148-153, `tracked: v.tracked.contains(r)`), with `head` from `resolve_heads` → `head_at` → `main_tip` (:573), which under `pin_unpushed` (default `true`, config.rs:164-165, :355-357) is the publish chain's top while it leads origin (proposal.rs:386-396). An untouched untracked sibling passes `dirty_check` on its empty overlay (remove.rs:560-561, :634-639), is absent from `upstream.repos` (:338-340, so clause 3 never sees it), and its pin goes with the close like every manifest repo's (:421-430). r2 therefore closed a session whose sibling's base sat on an unpushed chain that drop/replay rewrite (D57; r7 §5.4 X2), leaving nothing durable to hold it: the session was not "fully landed" (autoclose-req.md: every repo). Clause 3 does not imply this clause for tracked repos either: `ahead` is counted against `main_tip`, the chain top under `pin_unpushed`. With this clause `base_rev_unreferenced` (handlers.rs:1282-1294; tracked repos only, :1284) cannot fire for an auto-close, which is what makes the close no-loss without §5.4's head export (not in P1). **Why L and not `origin/<publish.branch>` (r4, Codex r3 blocker 1; Claude r3 nit 1).** The requirement is "fully landed on main". For a repo that publishes its default branch the two refs are one ref. For a split-branch repo they are two lineages: the publish commit's parent is the publish branch's chain top or `origin/<publish.branch>` (`merge_base`, gate.rs:60; chain.rs:213-223: "B is origin/release, not origin/main"), while the session's base comes from L (`main_tip`, proposal.rs:387-395: the chain rule applies only when the branches are equal; `new`, new/mod.rs:573 and :581-583; `new_pins_default_branch_when_publish_branch_differs`, new_layout.rs:540-551; config.md:127). Codex's scene: `default_branch = main`, `publish = { branch = "release" }`, release descending from main's tip; the publish lands C on release and pushes it, the post-publish rebase defers (`recent_writes`), the workspace stays on main's base B with the published entries in its overlay, `ahead == 0` against `main_tip` = L; r3's clause 4 passed (B and C are ancestors of `origin/release`) and `remove_strict`'s candidates were origin/release-reachable (:981 with `cfg.publish.branch`, :563) with C descending from B, so the projection discharged the overlay and the session closed while main never received C. Claude's scene is the other lineage: release not descending from B, so `newer` (:575-577) never holds, every deferred session is `not_pushed` and the note says "push REPO", which answers `NoCommit` (push.rs:292-297). Under r4 both are `not_merged`. A `Moved` split-branch session has its base on release too: the post-publish rebase moves a session onto the publish commit, split or not (`after_publish(ctx, s, &landed.staged.tree, &landed.commit)`, push.rs:858), so the base test fails with `what: "base"`. L is refreshed for a split-branch repo only by `new` (new/mod.rs:561-572, unless fresh) and by a publish whose cached check needed it (`refresh_refs`, gate.rs:73-79, via :82-94); periodic fetch skips split repos (periodic_fetch.rs:48), `sync` skips non-following repos (sync_move.rs:205-208; `follow_main` defaults to `false` for them, config.rs:324-326), and the publish's own fetch is of `publish.branch` (push.rs:551-563). A stale L only fails the proof: the tracking ref moves by fast-forward (periodic_fetch.rs:75-78); a rewound origin is the exposure every D57 judgement has today, not new here. **Consequence, stated plainly: a publish that touches a split-branch repo never closes its session in the same call, nor (r5) in any later call until C is on L, whatever else that call publishes** — C cannot be on L before a human has merged `release` into `main` and L has been fetched — so for such a repo the hook is the requirement's fallback, `mark done` with `not_merged` (§4, §8), and the session ends by the agent's own `rm` (D57 against `origin/<publish.branch>`, unchanged, §11 item 7) or, after the merge and a fetch of L, by any later judge of the session (set (b) skips split-branch repos, §1, so in practice the agent's `rm`, or a push of a same-branch sibling of the session; §11 items 19 and 20). An **untouched** split-branch sibling of a same-branch repo passes: its base is L's tip or an ancestor of it (new/mod.rs:573) and nothing was published to it, so it does not hold the session open (§10 row 1's sibling variant). | `not_pushed` `{repo, what, head or commit, publish, phase, stopped, ref, tracked, on_chain, chain, copy, pending, outstanding, more}` / `not_merged` `{repo, what, head or commit, publish, phase, ref, publish_branch, default_branch, tracked, pending, outstanding, more}` (§2.1) |
| 5 | For every `already_published` entry of this call whose repo has a batch of this session (r5, Claude r4 nit 2): `push` is `Pushed` or null (`push = false`: clause 4 decides). A refused entry fails clause 4 first, so `reasons` reads `["not_pushed", "push_refused"]` (r5 corrects r4's test, §10 row 1). | r2 wording (Claude r1 note): a `published` entry can only carry `Pushed` or null, because push.rs:666-683 turns `Refused` and `Unknown` into errors and the handler then takes the error path (:2127-2130, no hook). Only `already_published` can carry `Refused`: a row closed `Done` with `stopped: PushRefused` (push.rs:416-430) is what `landed_before` copies (:2525-2542, `push` at :2539; `Unknown` rows are not `Done`, :2528, so they are never `already_published`). The CLI turns a refused **`published`** push into `ok:false` (confirm.rs:874-889) but reads nothing of `already_published`: `publish_envelope` loops `published` only (:861) and passes `already` into `data` unexamined (:899) — r3's "already" was wrong (Codex r3 nit 1). r4 plans the same `push.kind` match over the `already` entries before the envelope is built: `Refused` → the `PushRefused` error of :879-882 (an `Unknown` cannot occur there: only `Done` rows are copied, :2528), so a re-run after a refused push answers `ok:false` like the first run did, carrying `closed: false` and `push_refused` (§6; §10 row 9). **r5 (Claude r4 nit 9): that error's detail carries `fix: "git-strata push REPO"`: a re-run cannot clear a `Done` row with `stopped: PushRefused` (push.rs:417-430), so an agent must not loop on `publish`.** A small CLI behaviour change (today the re-run is `ok:true`), §11 item 22. **r6 (Claude r5 nit 1): the entry is refreshed before this clause reads it: a `Refused` whose commit or copy has reached `origin/<publish.branch>` since then reads `Pushed` with `landed_later: true` (§6). So a `Refused` here names a commit that is still unpushed, this clause's `["not_pushed", "push_refused"]` holds, and the `fix` above is true.** | `push_refused` `{repo, push}` |
| 6 | No conflicts and no pending deps proposal, in four reads whose scopes differ on purpose (r9). **(a) Every tracked repo:** `conflicts` empty (the workspace row's, daemon upstream.rs:402; strata-store lib.rs:210), `auto.reason != "conflicts"` (the Stop hook's reading, cli sync.rs:163-164; `RebaseReport::Deferred` reasons, rebase.rs:52-59), and `deps` is `None` (a `Pending` proposal whose `to` is not the workspace's deps set, daemon upstream.rs:437-443). **(b) r9 (Codex r8 blocker, §3.2): every repo, tracked or not: no open proposal of origin `Session{this}`. One `strata_deps::open_proposals(store, None)` (proposal.rs:146-153; the CLI's `waiting` list makes the same call per repo, confirm.rs:945-955), filtered on `origin`, with no manifest or tracked filter. r8 read this per tracked repo.** **(c) r9 (Claude r8 nit 1): every repo of the manifest: no saved `sync` conflict report, `store.sync_report(s, repo)` (strata-store main_sync.rs:311) is empty.** **(d) r7 (Claude r6 nit 2), r8: no path `assign` named for this session, in any repo: `strata_publish::assigned_to(&rows, s)` (§3.2; re-exported next to `assign`, lib.rs:31-32) is empty.** | These are the brief's clauses; a deferred post-publish rebase without conflicts (`open_files`, `recent_writes`, `busy`, `state`) is **not** a clause: the workspace then sits on its old base with the published entries in its overlay, and the judge's projection lets exactly those go (§3.1). **r6 (Codex r5 blocker 2): this read is the early refusal, not the guarantee. Proposals and `conflicts` deferrals can be written without S's queue place, so the proposal and conflict half of this clause is read again in the transaction that commits `Removing` (§3.2), and a refusal there carries the same code. r10 (Codex r9 blocker): an own draft that comes after that transaction is refused in its own insert, so `closed: true` means S has none (§3.2).** **r7: why `assigned`.** A path named by `assign` goes only with the named session (stage.rs:525-542), and the assignment ends only when a later publish lands the path (rebase.rs:940-948), whatever the assignee's lifecycle. So after a close every other session's publish of it answers `Conflict assigned` until someone re-runs `assign` (:907), and the requirement says "no conflicts". It is read once, here, under the place, **and r8 makes that read final: `assign` now needs the assignee's queue place (Codex r7 blocker, §3.2).** **r7 (Claude r6 nit 11): a stale reason refuses too.** Main-follow keeps `reason = 'conflicts'` across a retarget (main_follow.rs:221-224), so a target that no longer conflicts can still refuse the close: a false negative until T2's next attempt rewrites the reason, with a mark that names it. **r9: why (a) keeps its tracked filter, and (b) has none (§3.2).** A `Pending` main proposal stays open after S goes (deps.rs:571 has no origin test for `pending`) and a rollout skips an untracked workspace (`SkipReason::Untracked`, rollout.rs:349-351), so one in an untracked sibling strands nothing and does not concern S. **r10 (Claude r9 nit 7): r9 said only an own proposal can vanish with S, and that is true of `awaiting_publish` only, the one state `Removed` hides (deps.rs:571-572). An own `pending` proposal stays visible after `Removed` (deps.rs:571), so (b) refuses it for the requirement's "no pending deps proposals", because S owns it, and not because anything would be lost.** **r9 (Claude r8 nit 1): why (c).** `sync` saves its conflict in `sync_report` and leaves the workspace unmoved (sync_move.rs:488, :615-616), so neither `workspaces.conflicts` nor `upstream` holds it (upstream.rs:402); the agent was told to resolve it, and `Removed` would delete it (sessions.rs:683). That read is final, because its only writer holds S's queue place (sync_move.rs:434, :616), and a move clears a report (sessions.rs:955, :1016). A report whose target has since moved on still refuses: the same accepted false negative as a stale reason. No report can exist for an untracked repo (`not_tracked`, sync_move.rs:39-45, checked for every requested repo at :130-133), so reading every repo of the manifest refuses nothing more and leaves no filter to reason about. | `conflicts` `{repo, conflicts, auto_reason, report}` (r9: `report: true` when a saved report is what refuses, with its paths in `conflicts`) / `deps_pending` `{repo, proposal, state, own, tracked, proposals}` (r9: the flat keys name the first item; `own`: the origin is this session; `tracked: false` occurs on an own proposal only; `proposals` lists every item); both gain `after: "checkpoint"` when the transaction found them (r6) / `assigned` `{repo, paths, assigned}` (r7; r8: any repo, and this read is final; r9, Claude r8 nit 6: the flat keys name the first repo and `assigned` is `assigned_to`'s whole map, so an agent that clears one repo is not refused again by surprise for the next) |
| 7 | **The judge**: `remove_strict(s)` passes (§3). Phase A (remove.rs:136-182): `refuse_open_files` (:175, :334-360: every same-uid process with an fd, a mapped file or its **cwd** in the session root or its shadow repos, strata-deps open_files.rs:15-34; a dev server started with `session exec` is such a holder while its cwd or a file stays inside, §11 items 43 and 58, and its `dev_ports` lease goes only with `Removed`, strata-store sessions.rs:676-678); `dirty_check` (:178-182, :511-685): unresolved scenes (:524-543), a cold reconcile of every healthy workspace against its base (:550-558), then per tracked repo the **strict landed rule (§3.1): an entry is let go only when the §1.9 T3 projection onto the current landed target discharges it (:578-595, under the `newer` guard :573-577); the D57 per-entry content match (`split_landed`, :1061-1106) discharges nothing in strict mode (r3, Codex r2 blocker 3), and `rm.regenerable` exempts nothing (r2; :622-633 off)**; untracked repos' edits (:634-639); every done move's old tree still on disk (:641-657; **strict: changed or not, r7, §3.1**); and **any nested repository the cold scan warns about (strict, r7, §3.1)**. Phase B: BeforeRemove checkpoints (:188-265) and the first recheck (:249-259 → `changed_during_remove`, :266-286; strict: no D57 for the new entries either, §3.1); the R4 holders scan (:304-305); **the post-scan recheck of every workspace and every kept old tree, then the second holders scan (§3.2)**; the fence (:310-311); teardown (:314). | This is the F1 clause of the brief ("corrected F1 unpublished = false"), read from disk rather than from the Tracker: right after the post-publish move the Tracker's base differs from `ws.base`, so `Trackers::live` returns `Default` and `unpublished` reads `true` (handlers.rs:347-363, the base check :359; the reset at `take`, :280-286; §6.2's transient case). F1's own clause (A) is "the overlay against the current base is non-empty" **with no regenerable exemption** (:368 counts every entry), so the judge counts them too (Codex r1 blocker 2, §3.1). F1's comment also says which history is not work: "edits reverted or already published on a newer base" (:365-367), i.e. the base's own history, never an older publish's content: a fresh revert to what an **older** publish held is live overlay by F1, and r3's judge refuses it (Codex r2 blocker 3, §3.1). The judge's reconcile is that definition computed cold, and it is also what makes the kept-tree and holder clauses exact, since `rm` runs them (:175, :641-657). The reply then reports `unpublished: false` for a closed session's repos exactly as `Rm` does for `!discard` (:1279), and truthfully: nothing unpublished was let go. One asymmetry is inherited from F1 and named here (r4, Claude r3 nit 8): on a `Deferred` session a post-publish revert to the **base's own** content leaves no overlay entry (the workspace equals the base), so F1 (:364-368) and the judge's cold reconcile read nothing unpublished and the close proceeds — the publish it undoes is on main and the base's content is on main, nothing is lost, but the agent's last intent was not published; the same workspace content on a `Moved` session (base = the publish) is an entry and refuses. Today's `rm` reads it the same way; this addendum does not change it. | `open_files` `{pids, open}` (:342-359) / `unpublished` `{repos, regenerable, landing, landed}` (:664-673 `repos`; `regenerable` :680-682 names the subset that matches the repo's rules; `landing` r3, §3.1: per repo the projection target, `anchor` (r4: the default branch's tracking ref the candidates must be on), `why_none` ∈ `no_newer_publish`, `no_built_base`, `base_tree`, `no_landed_publish` (r4), `published` (r5: the session has a publish row with a commit for this repo; it tells the two meanings of `no_landed_publish` apart, §3.1), and `historical`, the paths a historical publish holds, computed only when the projection left something (r4); `landed` :670 keeps today's summary of what the projection discharged) / `untracked_unpublished` `{untracked_repos}` / `kept_trees` `{old_trees}` (r7: each item says `changed`, §3.1) / `embedded_repo` `{embedded_repos}` (r7, §3.1) / `unresolved` `{unresolved}` / `changed_during_remove` `{repos, old_trees, outstanding, pending_changed, after}` (:279-285; `after` ∈ `dirty_check`, `checkpoint`; `old_trees` r3, `outstanding` and `pending_changed` r5, §3.2; r6: the same `Dirty` with a `reopened` list is reported under its items' codes, `conflicts` or `deps_pending`, not under this one, §3.2) / `busy` `{reason}` (**every `Code::Busy`**, r4, Claude r3 nits 3 and 6: `publishing` (resolve.rs:59-62, :64-72), `lock_wait` (:119-122), `pins_tightened` (`fenced_error`, locks.rs:812-816, raised at the queue take by `check_session` (:547, :589-590 → :345-353) and by the fence inside the judge, remove.rs:310-311), `du_cancel_timeout` (`hold_sessions`, du.rs:726-753, under `with_du_hold`, handlers.rs:1907-1911); **no mark**, §4; r3 sent the last two to `error`, which marked then) / `unknown` `{state, code}` (no mark; **r7, Claude r6 nit 4: decided by reading the lifecycle again after a failed `remove_strict`, never from `Code::Unknown`, which failures before `Removing` also answer: a failed holder scan, strata-deps open_files.rs:100-103; a failed git command, strata-gitio refs.rs:119-135. `Unknown` or `Removing` (:450-454; :459: a scene that could not be recorded leaves the row `Removing` for `recover`) → `unknown`. Still `Active` → by code: `Dirty`, `OpenFiles`, `Busy`, else `error`; `set_mark` accepts an `unknown` row, marks.rs:83, so the order matters**) / `error` `{code}` for anything else that is not `Busy` (**no mark, r7, Claude r6 nit 5, §4**; r6 marked it when the closure ran), **and, r8 (Claude r7 nit 2), a `Dirty` that only a failed read explains: `error` `{code: "judge_read", errors}`, §4** |

A `Dirty` with several sections lists every applicable code (`unpublished`, `kept_trees`, `embedded_repo`, `unresolved`) in
`reasons`, with the whole Dirty `detail` (minus `upstream`) under `detail`. **A repo whose entries only a failed read kept is not
`unpublished` (r8, §4).**

### 2.1 Clause 4's proof: every commit the close would abandon (r5, Codex r4 blocker)

**The hole, traced.** r4 tested the bases and the commits of this call's reply. Session S holds `front` (`push = false`)
and `tool`:

| Step | What the code does | Source |
|---|---|---|
| S publishes `front` within the quiet window. | Commit C goes on the chain and the row becomes `{head: C, source: publish}`. No push. The post-publish rebase defers, and the workspace stays on base B, which is on L. The hook answers `not_pushed` and marks done, which is right. | strata-publish push.rs:643 → pending.rs:98-115 → rm_state.rs:437-442; push.rs:684-686; rebase.rs:758-801, :504-512 |
| S restores `front`'s files to B. | The overlay is empty. Nothing else changes. | remove.rs:556-561 |
| S publishes `tool`, or an unrelated session pushes `tool` (set (b)). | The call has no `front` entry, because `rerun_plan` walks the requested batches only. `front`'s `ahead` measures B. `dirty_check` finds no entry to discharge. r4's clause 4 sees a base on L and no commit of `front`, so S closes with `closed: true`. | handlers.rs:2408; daemon upstream.rs:131-134; remove.rs:560-561 |
| `Removed`. | The row is deleted in the transition's transaction. C stays on the chain ref, and nothing records that S's work never reached main. | sessions.rs:676-684 |

An earlier release-only publish takes the same path (a split branch, `not_merged` in the first call), and so does every
set (b) or `push` re-judge.

**Why the row alone is not the proof.** The row (`RebasePending.head`, rm_state.rs:79-80) is where a deferred target
lives, and it is the place Codex's fix starts from. But it steers one workspace, it holds one target per (session, repo)
(rm_state.rs:621), and it can leave while the commit is still unlanded:

| Path | What happens to the row | Source |
|---|---|---|
| `sync` moves S to `main_tip`. Under a `Diverged` chain that is origin's tip, which lacks C, with the default `pin_unpushed`; under `pin_unpushed = false` it always is. | Consumed. `sync` passes the row's current rev whatever its head is, so `attempted` holds, `head` is cleared and the row is dropped. | proposal.rs:386-396; sync_move.rs:499; rm_state.rs:376-386, :313-325 |
| T2 finds the target stale: `stale_base` after any other move, `stale_target` once a chain drop or replay took C off both refs. `validate` does the same under every `reconstruct`. | Discarded. | pending.rs:372-385, :502-505, :83-91 |
| A publish whose commit does not descend from the kept target. | Never promoted (`RebaseTargetConflict`); the row keeps the older target. | pending.rs:104-111 |
| Main-follow retargets a `publish`-source row to a newer main tip. | `head` changes and `source` stays. | main_follow.rs:211-235; rm_state.rs:631-646 |

The durable record of "S published C in R" is the publish row. `publish_items` names the session and the repo
(records.rs:160-168), `outcome.commit` names the commit (push.rs:870; :417 for a refused push), nothing deletes a row
(no `DELETE FROM publish` in the store), and `candidates` already reads it that way (remove.rs:984-989). So the proof
reads both.

**The rule.** `strata_session::outstanding(store, git_of, cfg_of, rec, &pending, &rows) -> Result<Vec<Outstanding>>`, in a new
module crates/strata-session/src/outstanding.rs. It is read-only and takes no lock. `pending` is
`store.rebase_pending(Some(id))`, passed in so that step 5.5 can guard exactly the rows it proved (§3.2); `rows` is
`store.publishes()`, read after this call's own rows closed and **passed in too (r8, Claude r7 nit 9): the hook reads it once under the queue
place, for this clause and for clause 6's `assigned`, and step 5.5 reads its own.** L is the tip of
`remote_ref("origin", &cfg.default_branch)`. **For every repo R of `rec.manifest.repos`, tracked or not:**

| `what` | Commit under test | Passes when | Source |
|---|---|---|---|
| `base` | `ws.head`. | `is_ancestor(ws.head, L)`. | §2 clause 4 |
| `publish` | Every row with a batch `(S, R)` (the filter of remove.rs:988). Phase `Done` with a non-null `outcome.commit`: a successful publish, or a refused push's (`stopped: "PushRefused"`). Phase `Unknown`: `outcome.commit`, else the applied chain-ref change (a row closed by startup has no outcome). **That is the reading of `row_commit` (push.rs:471-482), which `outstanding` reimplements over `store.ref_changes`: it is `pub(crate)` in strata-publish, and strata-publish depends on strata-session, not the other way (Cargo.toml:23; r7, Claude r6 nit 10).** A `Done` row with a null commit stopped before writing anything and is skipped. | The commit is on L, or its replayed copy is (below). An `Unknown` row with no commit found fails closed (`commit: null`), and so does a `Running` row, which the queue place excludes anyway (§1). | records.rs:160-168, :225-227; push.rs:417-447, :870 |
| `pending` | The row's `head`, **any `source`**: `publish`, `replay` or `main`. | `is_ancestor(head, L)`. | rm_state.rs:79-80, :437-442, :590-595, :620-622 |
| `planned_replay` | The row's `planned_head`: a replay between its plan and its adoption, or a crash there. | `is_ancestor(planned_head, L)`. | rm_state.rs:82-84, :558-566; pending.rs:75-82 |
| `planned_publish` | The row's `planned_publish` id: a publish whose commit is not known yet. | The id's publish row exists and is not `Running`, so the `publish` line decides it. Fails closed otherwise. | rm_state.rs:85-86, :334-353 |

Three things it deliberately does not do:

- **It has no cap.** D57's `LANDED_MAX_COMMITS` (remove.rs:723) only makes it discharge less; a cap here would be a hole.
- **It reads no overlay.** That is the requirement "independent of overlay emptiness".
- **It reads no reply.** This call's rows are closed `Done` before the hook runs (push.rs:892; handlers.rs:2133), so they
  are in `rows`; another session's are not, so Claude r4 nit 2's over-refusal goes away. With the store and git as its
  only inputs, the trigger cannot change the answer: an own publish, set (b) and `push` get the same list.

Each distinct commit is reported once, the first `what` in the table's order winning. A `pending` head equal to a failing
item's commit or copy is folded into that item as `pending: {rev, source, reason, unpushed}`; a `what: "pending"` item
carries the same object. The git reads go through a small `Ancestry` trait so that §10 row 3 can drive the rule with a
fake.

**The replayed copy, and why `publish` needs it.** A replay rewrites C into C′ (`replay_commits`, strata-gitio
replay.rs:50-100, from `push_replaying`, resolve.rs:821, or `chain --repo REPO --accept-origin replay`, :648). Nothing rewrites the
publish row. That is the normal way a `push = false` chain lands once origin has moved
(`headless_push_replays_a_diverged_chain_then_pushes`, tests/resolve.rs:1035-1082), so without a copy rule such a session
could never close: a false negative, never a false close, but on the very path the push trigger exists for. The copy
carries the identity. `replay_commits` re-uses the raw message (:97-98), every publish message ends in `sessions: <ids>` and
`Strata-Publish: <id> (<repo>)` (message.rs:112-116), and a chosen message cannot forge either (both keys are reserved,
message.rs:40-43, :46-59). **The publish id alone is not an identity (r6, Claude r5 nit 2):** it is `MAX(id)+1` per store
(records.rs:157-158), and `tip` is origin's history, which holds other developers' commits, so another store's `41 (front)`
would satisfy r5's lookup and, with the row gone and the base on L, give a false `closed: true`. So
`landed_copy(git, id, repo, session, c, tip)` takes `m = merge_base(c, tip)` and looks on `m..tip`, first parent, at most 500
commits, for the commit whose trailers name both `<id> (<repo>)` and `session`. `commits_between_trailers` (gitio
upstream.rs:96-116) gains `PublishCommit.sessions`, parsed as `history_between` parses it (:52, :75-80). A session id is 40
random bits (strata-session sys.rs:86), so a false match needs that to collide as well. r5 also accepted the subject
`strata publish <id> (<repo>)` for commits older than the trailer; such a commit names no session and proves nothing here, so
the shared `find_publish` r5 planned is dropped and `commit_of` keeps its own lookup, fallback included (pending.rs:191-195).

**Pruned commits (r6, Claude r5 nit 6).** `is_ancestor` and `merge_base` answer `Err(NoSuchCommit)` for a commit that is gone
(gitio refs.rs:96-117, :121), not `false`. `Ancestry` maps exactly that code, `is_ancestor` to `false` and `merge_base` to
`None`, so a pruned C (handlers.rs:2500-2505) has no merge base, no copy is provable and the item stands with its `on_chain`.
A plain `?` would have given `error {NoSuchCommit}` and no `on_chain`. Any other git error stays an `Err`, which is `error
{code}`. A squash merge that dropped the trailers, or a copy beyond 500 commits, also leaves the item standing. Those are
false negatives, and `on_chain: false` below names the way out.

**Codes and `detail`.** The code goes by branch pair, as in r4: same branch `not_pushed`, split `not_merged`. `reasons`
lists each code once, the first failing item's first. `detail` keeps r4's keys for the first item (`repo`, `what`, `head`
or `commit`, `ref`, `tracked`, and `publish_branch`, `default_branch` for `not_merged`) and adds `publish` (the row id)
and `phase` (`done`, `unknown`) on a `publish` item, `stopped` when the row says `PushRefused`, `copy`, `pending`, and
`outstanding: [every item, at most 20]` with `more: bool`, so the note and the agent see a second repo too.

**`on_chain` and `chain` (`not_pushed` only; r5, Claude r4 nit 1).** r4's note said "push REPO" for every `not_pushed`.
That is wrong whenever the next push cannot land the commit: a `new --base <rev>` base that is a local-only descendant of
`main_tip` (new/mod.rs:577), a commit a chain drop orphaned (resolve.rs:638-642), a base left behind when `rebase_replayed`
failed (handlers.rs:2305-2307). So each `not_pushed` item carries `chain`, the state of `publish_chain(git, &branch)` in
lower case (proposal.rs:352-382: `leads`, `diverged`, `rewound`, `equal`, `behind`, `absent`), and `on_chain`, true when
the chain's top reaches the commit (`is_ancestor(c, top)`, or a `landed_copy` against the top, reported as `copy`) **and**
`chain` is `leads` or `diverged`:

| `on_chain`, `chain` | What lands it | Note and skill (§4, §8) |
|---|---|---|
| `true`, `leads` | The next push of R; `send` needs `Leads` (push.rs:291-298). | "ask the human to push REPO; the next push closes this session" |
| `true`, `diverged` | A headless push replays first (resolve.rs:813-826). A repo that needs a confirmation answers `NoCommit` until the human's `chain --repo REPO --accept-origin replay` (`confirmed_repo_push_never_replays`, tests/resolve.rs:1179). | "ask the human to run `git-strata push REPO` (a repo that needs a confirmation: `git-strata chain --repo REPO --accept-origin replay` first)" (r7, Codex r6 nit 2: `repo` is a long option, args.rs:256-257; §10 row 14) |
| `false` | Nothing the daemon waits for: the commit is off the chain, or the chain is rewound, which is the human's. | "no push lands COMMIT; tell the human if it was meant to land; run `git-strata rm --session S` yourself when done" |

**Flows checked against the rule.**

| Flow | Result |
|---|---|
| `push = true`, one or many publishes, `Moved` or `Deferred`. | Every commit is pushed in its publish's lock and L moves in the push transaction (push.rs:289-303). Passes. A `Deferred` session's row names a commit on L, so dropping it loses nothing. |
| `push = false`, `Moved`. | `base` fails, `not_pushed`. After the push everything is on L and it closes. |
| `push = false`, `Deferred`, overlay restored, then a sibling's publish or a re-judge. | `publish` fails, with `pending` folded in. **The blocker.** |
| The same, after a `sync` consumed the row. | `publish` fails, no `pending`. |
| A refused push, re-run. | `publish` with `stopped: "PushRefused"`; clause 5 adds `push_refused`. |
| An `Unknown` publish of a sibling, then a publish of another repo (`refuse_open` is per repo, push.rs:362-372). | `publish` with `phase: "unknown"`, until a push lands C. |
| A replay that landed, on a session still on its old base, with no rebase row or one naming the copy. | The publish row's C is off L and its copy is on L, so it passes. A row still `Deferred` names the copy, also on L. |
| A replay that landed while the rebase row still names the old C (r6, Claude r5 nit 8): `chain --repo REPO --accept-origin replay` writes no plan (resolve.rs:643-652), and a confirmed push runs no reconstruct (handlers.rs:2225-2260). | `pending` fails on C, because only `publish` has a copy rule: `not_pushed`, `on_chain: false`. A false negative until T2's `reconstruct` discards the row as `stale_target` (pending.rs:83-91). Nothing re-judges then, so the note's fallback, the agent's `rm`, is the way out. |
| A replay that landed on a `Moved` session whose move onto the copy deferred (r6, Claude r5 nit 8). | `base` fails on the old head, which also has no copy rule: `not_pushed`, `on_chain: false`, the same note. A false negative until T1 or T2 moves the session. No copy rule is added for `base` or `pending`: both name a commit the close would drop with no record that its content landed under another id, and refusing costs a done mark. |
| A replay whose recording push failed. | `publish` with `copy` on the chain, `pending.source == "replay"`, `unpushed: true`, `on_chain: true`. |
| An earlier release-only publish. | `publish` fails, `not_merged`, whatever the overlay holds. |
| A split-branch repo merged into main with L fetched. | Passes when the merge kept C or its trailer. |
| Main-follow enrolled S's tracked sibling on an unlanded chain top: another session's `push = false` publish (main_follow.rs:238-261). | `pending`, `source: "main"`, `not_pushed`. T2 gives the same answer by moving S onto it, when the base test fails, so the outcome does not depend on T2's timing. It closes after the push. |
| An untracked sibling under the same chain top. | Not enrolled (main_follow.rs:177-178). Its base is on L, so it passes: S holds nothing of that commit (Claude r4 nit 2). |
| A chain dropped by a human. | `publish` fails, `on_chain: false`; the note names the agent's `rm`, which does not read the §2.1 items (§11 item 27). |

Cost: one `store.publishes()`, the read `publish` already does at handlers.rs:2044 and `candidates` does per repo, plus
one `is_ancestor` per manifest repo and distinct commit of this session. The chain and copy reads run only for a commit
off L. A close runs the proof twice (clause 4, step 5.5), and so reads `rows` twice; clause 6 reads none of its own (r8).

**Never dropping a record.** Clause 4 runs under the queue place, which excludes S's publishes. It does not exclude writers
of S's row that hold only the repo lock: main-follow's `enroll_main` and `retarget_pending` (main_follow.rs:225-261) and
replay's `plan_replay` and `settle_planned` (resolve.rs:861-863). §3.2 therefore repeats the proof at step 5.5, makes
`Active → Removing` conditional on the rows being the ones it proved, and (r6, Codex r5 blocker 1) has each of those writers
refuse, in its own transaction, a session that has left for removal, so what `Removed` deletes (sessions.rs:682) has every
target on L. The agent's own `rm` answers as before (§11 item 27).

## 3. The close (never a discard)

### 3.1 `Sessions::remove_strict`: the agent's non-discard `rm` without the exemption and without the historical match

**The regenerable exemption (r2).** Today a non-discard `rm` of a healthy tracked repo does not count the unpublished
entries that match `rm.regenerable` (rm-followups §1.8): `dirty_check` partitions them out of `remaining`
(remove.rs:622-633), lists them under `detail.regenerable` only when something else blocks (:680-682), Phase B lets
them go with the landed entries (:222-224) and skips them in the first recheck (:248, :258, :1122, :1130), and the
report lists them as `regenerable` (:36-40, :317-321; `regenerable_does_not_block_rm_and_is_checkpointed`,
crates/strata-session/tests/rm_regenerable.rs:37-72). That is the human's rule for an explicit `rm`: the patterns
are pinned like `publish.confirm` (strata-types config.rs:229-232; confirm_flow.rs:97-98, :107-113) and name build
output the human is willing to lose from a session they remove by hand. An unattended close is not that `rm`: the
brief asks for `unpublished = false`, F1 counts every entry (handlers.rs:368), and a session whose `dist/` edit never
landed is not "fully landed on main". Codex r1 blocker 2's scene (publish one path while a regenerable file stays
edited; r1 closed the session and reported `unpublished: false`) is therefore refused since r2.

**The historical content match (r3, Codex r2 blocker 3).** Today a non-discard `rm` also lets an entry go when its
content equals the content at the same path in **any** Done publish of this session for the repo whose commit
origin's tracking ref has: `candidates` (:980-1023; Done rows of this session and repo :984-989, origin-reachable
:1008-1010, newest first, at most `LANDED_MAX_COMMITS` = 32, :723) and `split_landed` (:1061-1106), which tries
every candidate in turn (:1074-1101) with D57's per-kind content rule `landed_at` (:1049-1057). Nothing in that match
asks whether the candidate is **newer than the base**; the code guards the projection against exactly this ("Only a
publish newer than the base: onto an older tree, a revert to its content would look discharged", :573-577) but not
the per-entry match. Codex's scene: the session publishes `src/p.ts` = v1 (C1) and moves onto it, publishes v2 (C2)
and moves onto it, edits `src/p.ts` back to v1 and publishes an unrelated path (C3). Against the base (P = v2) the
overlay holds the fresh revert P = v1; `split_landed` matches it at C1 → `landed` (:1090-1092); Phase B lets it go
(:222-223), `recheck` treats it as judged (:1130), and the r2 judge closed the session with `unpublished: false`
while main's current `src/p.ts` is v2 and F1's live overlay is non-empty (handlers.rs:364-368). The revert survives
in the BeforeRemove checkpoint until gc, so nothing is destroyed, but the close's claim is false and the requirement
("fully landed on main") is not met.

**r3's rule for strict mode, the current-target landing proof.** A tracked repo's entry is let go **only** when the
§1.9 T3 projection (`project`, :943-975) of the whole overlay onto the **current landed target** discharges it. The
target is the newest D57 candidate that strictly descends from the workspace base (`sp.newest`, :1073, under the
`newer` guard :575-577: `c.commit != ws.head && is_ancestor(ws.head, c.commit)`). `project` rebases the overlay from
the base onto the target's tree (`rebase_with`, :964; `PreviewBlobs`, :963, reads the session's blobs without
building anything) and blocks every path of the projected overlay (both rename endpoints), every conflict, every
`.lww` path and every unstable path (:965-973); an entry is discharged iff it touches none of them (:974). For a
close this is exactly "an **empty** (at every path the entries touch), **conflict-free**, **lossless** (no LWW
resolution, nothing unstable) projection onto the current landed target". Why it is sound where the content match
is not: the target descends from the base and is on origin (:1008-1010), so a change the projection discharges is one
the base → target history already made on origin — on `origin/<default_branch>`, since r4 anchors the strict
candidates there (below); a revert to an **older** publish's content is carried by the
rebase (base v2, target v2, entry v1 → the projected overlay still has the path → blocked); and when the base
already is the newest publish there is no target and nothing is discharged. The scene, both ways: `Moved` onto C3 →
`newest` = C3 = `ws.head` → no projection → P remains → `unpublished`; `Deferred` on C2 → project {P = v1, Q} onto
tree(C3): Q discharged, P carried → `unpublished`. The target is the newest publish rather than origin's current
tip because every candidate is an ancestor of the tip anyway (:1008-1010) and because another session's later
publish of the same path would otherwise make a landed edit look unlanded.

**The built base the projection needs.** `project` returns `None` without a built base for the target tree
(:957-959; `built_base`, strata-session base.rs:127-147: the base's own deps set first, else any built set under
`bases/<repo>/<tree>`), and `rm` builds none (`rm_t3_without_a_built_base_stays_dirty_and_builds_none`,
tests/remove_landed.rs:830-840). After a publish the base exists: `rebase_one_with` builds the target base before
anything can defer (`base_for` :471 → `ensure_base_for`, base.rs:67-73, ahead of the auto gates `policy_gates`
:504-512 — `recent_writes` :322-342, :350-352, `conflicts` :359-362 — and of the locked move :515-598), so a
`Moved`, `Deferred`, `Waiting` (:592-597) or `OpenFiles` (:583-588) session has the base its projection needs; the
common agent flow (edit, checkpoint, publish within the quiet window, `default_quiet_secs` = 120, config.rs:277-279
→ `Deferred recent_writes`) therefore closes. Where none exists anyway (a base gc'd long after a publish, the `c2`
move-source path :465-466, and, r5, a publish whose push was refused: its row closes `Done` before step 8, strata-publish
push.rs:669-674, :838-863, so nothing builds that tree's base until a `sync`, T2 or a `new` at that tip does,
base.rs:59-63), strict mode refuses `unpublished` with `landing.why_none: "no_built_base"`, the note says
so and names the agent's own `rm` as the way (r4, Claude r3 nit 4: the paths are published, "publish them" was the
wrong advice), and that `rm` still has D57 (§11 item 7); the judge never builds a base (§11 item 13).

**The anchor of the strict judge is the default branch (r4, Codex r3 blocker 1).** `dirty_check` passes
`&cfg.publish.branch` to `split_landed` (:563), which passes it to `candidates` (:1072); `candidates` reads
`refs/strata/remotes/origin/{branch}` (:980-983) and keeps only the session's Done publishes whose commit is an ancestor
of it (:1008-1010); `recheck` takes the same `branch` (:1118) for its own `split_landed` (:1135). That anchor is D57's
and is right for the agent's `rm`: "landed" there means "on the branch this repo publishes to". For the close it is
not: the requirement says main, and for a split-branch repo `origin/<publish.branch>` is another lineage (§2 clause
4). Strict mode therefore passes **`&cfg.default_branch`** at both sites (`dirty_check`'s :563 and the `recheck` call
of :249-259), so `sp.newest` (:1073) — the projection's target under the `newer` guard — is the newest own publish
**on `origin/<default_branch>`** that strictly descends from the base, and the historical diagnostic reads the same
set. For a repo that publishes its default branch nothing changes (one ref). For a split-branch repo there is no
candidate until the human's merge has reached L and L has been fetched: `landing.why_none: "no_landed_publish"` (new
value) with `landing.anchor` naming the ref, every entry remains, the refusal is `unpublished` with the paths and the
note says what that means (§4, §8). When the merge has been fetched, the projection discharges as for any repo if the
merged commit descends from the base (`newer`); when `release` branched before the base it does not, `why_none:
"no_newer_publish"`, and the agent's `rm` passes by D57 — a false negative, never a false close. The agent's `rm`
(`RemoveMode::Agent`) keeps `cfg.publish.branch` at both sites: D57 unchanged (§11 item 7).

**What `no_landed_publish` means under r5.** Clause 4 now refuses every unlanded own publish first, so at the hook this
value has two meanings, and `landing.published` tells them apart. `false`: this session has no publish of the repo, so
the paths are plain unpublished edits and the answer is "publish them". `true`: its publishes landed only as replayed
copies, and the overlay still holds their entries; `candidates` reads each row's own commit, which is off L (:1008-1010),
so nothing discharges. That false negative is accepted: T1 and T2 move the session onto the copy (handlers.rs:2353-2361;
pending.rs:292-325), after which the overlay is empty and the agent's `rm` passes. r4's note and skill text read this value
as "published where the repo publishes, run `rm`", which sent a never-published edit to an `rm` that refuses it as `Dirty`;
§4 and §8 are corrected. `remove_strict` called directly on a split repo still answers it (§10 row 6).

**What the projection refuses, and what it does not (r7, Codex r6 nit 1).** An entry whose blob and mode equal the target's at
that path is discharged, whatever happened to the path in between: the first arm of `rebase_with`'s per-path judgement is
`if *o == t { (t, From::Base) }` (strata-overlay rebase.rs:549-550), ahead of every conflict arm, so the path reaches neither the
projected overlay nor `conflicts`, and `project` does not block it (:965-974). r4 claimed a conflict for exactly that case (this
session publishes `p` = v1 at C1, another session publishes `p` = v2, this session writes v3 and publishes it at C3; the overlay's
v3 against tree(C3)'s v3) and proposed a second discharge rule for it. Both are withdrawn: the case passes and the rule could never
fire (§11 item 18). The false negative that does exist has sides that differ: this session publishes `p` = v1 at C1 and stays
`Deferred` with `p` = v1 in its overlay, another session publishes `p` = v2, this session publishes `q` at C3 on top of that.
Projected onto tree(C3), the entry's v1 differs from v2 and `t` differs from the base, so `rebase_with` carries it (merged or not)
or reports a conflict, `p` is blocked, and the judge answers `unpublished` with `landing.historical` naming `p`, although v1 did
land before another session replaced it. To the judge this state is the fresh revert of Codex r2 blocker 3, and that is why it
refuses. The note's "an older publish has this content; main's current state does not" is true of both, and the agent's own `rm`
passes by D57 (C1 is a candidate with equal content).

**Kept pre-move trees: the requirement read literally (r7, Claude r6 nit 6).** autoclose-req.md says "no kept old trees". `dirty_check`
lists only a tree that changed (:641-657), and r6 passed an unchanged one with no decision on record. Strict mode refuses every
done move's tree that still exists, changed or not, and says which: `kept_trees` with `changed` on each item. It costs the common
flow nothing, because a tree outlives its move only when it had a holder, a late or unjudged write, or ignored entries the move did
not carry (Phase F, move_workspace.rs:764-775); with none of those it is retired on the spot, unless that retire fails (:765-769), and
an `Auto` move defers on a holder before it starts. An unchanged kept tree may still hold those uncarried ignored files, which is one more reason not to
let it go unattended. The agent's `rm` keeps today's rule, so the note can name it for `changed: false` (§8), and the background pass
may retire such a tree before the next judge (old_tree.rs:258-276). §11 item 42.

**Nested repositories (r7, Claude r6 nit 1).** The walk never lists a nested repository's new files: a directory holding `.git`
restricts everything below it (scan.rs:124-139, :160-167; `embedded_repo_is_not_entered`, strata-overlay tests/classify.rs:576), the
only trace is the warning `EmbeddedRepo` (compute.rs:166-171), and `dirty_check` reads only `Unstable` from the warnings (:557;
:742-750). So a nested clone with unpushed commits goes to the trash, gc deletes it after 14 days (`GC_KEEP`, below), and the close would
report `closed: true` with nobody having chosen that. Strict mode collects the warning from the same cold scan
(`bad.is_none() && Active`, :550-558) for every repo of the manifest, tracked or not, and refuses: `embedded_repo` with
`detail.embedded_repos[repo]` in the `list_json` shape (:736-739). A nested repository under an ignored directory is not warned
about (the walk does not enter it, scan.rs:217-226) and leaves with the ignored paths, as named below. The agent's `rm` is unchanged and
still takes a nested repository to the trash; that is its explicit act, and §8 tells the agent so. §11 item 40.

The API: `remove(id, discard, confirm)` keeps its signature and behaviour for every existing caller; both it and the
new `remove_strict(id) -> Result<RemoveReport>` call one private `remove_with(id, RemoveMode)`,
`enum RemoveMode { Agent { discard: bool, confirm: Option<ConfirmToken> }, Strict }`. `Strict` is
non-discard with no token and no `discard_action` read (:155-173 is skipped as for `discard == false`) and differs
from `Agent { discard: false, .. }` in exactly these places, all keyed on one `strict: bool` passed to
`dirty_check(&rec, &intents, strict)` and `recheck(…, strict)`:

- :143 (r8, own correction): strict accepts `Active` only. `Unknown`, which `Agent` lets through to `dirty_check` (:524-526), answers
  `InvalidInput lifecycle` as Creating and Removing do. The hook never gets here (clause 1), and it leaves :912's `--discard` sentence unreachable
  in strict mode.
- :563 and :249-259 → :1118 (r4): the `branch` handed to `split_landed`/`candidates`/`recheck` is `cfg.default_branch`
  in strict mode, `cfg.publish.branch` for `Agent` (the anchor paragraph above); `landing.anchor` records the ref.
- :560-621 (r3; r4 reorders the reads, Claude r3 nit 9): strict calls `candidates` first (the target, `sp.newest`,
  needs no blob), projects the **whole** entry set (no `landed` taken out, no regenerable filter, the r2 change at
  :568-570) onto `sp.newest` under the `newer` guard, and only when something remains runs the D57 content match
  (`split_landed`'s loop :1074-1101, blob reads over up to 32 candidates) over the **remaining** entries to fill
  `landing.historical`; a close that discharges everything reads no blobs, and a refusal pays the diagnostic once.
  `remaining` is everything the projection did not discharge plus the unstable entries (`never`, :1070, :1103). `landed_all` is empty for strict; `projected_all`
  carries `(tree, discharged)` as today for Phase B. `detail.landing[repo]` = `{target: {publish, commit} | null,
  anchor: "refs/strata/remotes/origin/<default_branch>" (r4), why_none: "no_newer_publish" | "no_built_base" |
  "base_tree" (:954-955) | "no_landed_publish" (r4: no candidate on the anchor), published: bool (r5: a publish row
  with a commit exists for this repo), historical: [the paths the content
  match would have let go; omitted when the projection left nothing (r4)]}`, and `detail.landed[repo]` keeps today's summary (:596-609, `rebased`/`rebased_onto`) so a strict
  refusal shows what the projection did discharge. `landed_errors` and `projection_errors` (:591-593, :616-619)
  stay conservative: every entry remains. **r8 (Claude r7 nit 2): the hook does not read such a repo as a refusal (§4). In strict mode
  `newer`'s `unwrap_or(false)` (:576) no longer hides a failed read: an `Err` other than `NoSuchCommit` goes to `projection_errors`. A failure of the
  content match, which only fills `historical`, leaves the refusal standing and adds `historical_error` to `landing`, because the projection has
  already left those entries.**
- :622-633 (r2): the partition is not applied; every `remaining` entry stays and blocks. `regen_json` is still filled
  with the matching subset so `detail.regenerable` names which of the listed paths are regenerable (the agent's own
  `rm` would let exactly those go; §8 words it).
- :568-570 (r2): the projection input no longer filters regenerable entries out.
- :222-224: `judged.landed` and `judged.regenerable` are empty by construction, so `let_go` is only what the
  projection recomputed from the checkpoint still discharges (:225-247): the same proof over what the checkpoint
  holds (`rm_t3_phase_b_recomputes_the_projection_from_the_checkpoint`, remove_landed.rs:859). **r9 (Claude r8 nit 2): strict splits the `.ok().flatten().unwrap_or_default()` of :232-245.
  An `Err` from that projection goes up with `?`, because a failed read is no refusal (§4). `Ok(None)` stays "nothing discharged", because a base gc'd since Phase A is a real change, and refuses as
  `changed_during_remove`. The session is Active at this point. r10 (Claude r9 nit 4): r9 said nothing visible had happened, which overclaims. Besides an extra BeforeRemove checkpoint, step 5.1's `collect_session_leftovers` (remove.rs:186, :690-719) has already abandoned `Preparing` move intents and retired stale stagings. Both are the internal clean-up every refused Phase B leaves today, and neither touches the workspace or the lifecycle. `Agent` keeps the line.**
- :249-259 and `recheck` (:1113-1142) (r3): the regenerable predicate is `|_| false` (r2, :258), and in strict mode
  `recheck` does **not** call `split_landed` for the new entries (:1134-1140): every new entry is `left`. Without
  this, a revert written between Phase A and the checkpoint would be discharged by the historical match the strict
  judge has just refused (Codex r2 blocker 3's "apply the same policy during Phase B"). Non-strict is unchanged.
- :317-321: `RemoveReport.regenerable` is empty for a strict remove.
- :845-934, :269-277 and :474-482 (r8, Claude r7 nit 1) and :345-352 (r9, Claude r8 nit 4): strict has its own message lines (the paragraph after this list). `refused_prefix` and `refuse_open_files` take `strict`, and so does `dirty_message`.
- :641-657 (r7, above): the old-tree loop computes `changed = !old_tree_unchanged(i, cfg).unwrap_or(false)` as today and
  lists the tree when `strict || changed`, with `changed` on the item in strict mode. Only that loop's filter differs.
- :556-558 and :659 (r7, above): strict mode keeps the `EmbeddedRepo` paths of `scan.warnings`, and any of them makes the result
  `Dirty`, with `embedded_repos` in its detail.
- Step 5.5 (c) and `teardown`'s first write (r5, r6, §3.2): strict mode repeats the outstanding proof of §2.1 and makes
  `Active → Removing` conditional on the `rebase_pending` rows it proved and on clause 6's proposals and conflicts. `Agent` does neither
  (§11 item 27). The target writers' own lifecycle checks (r6, §3.2) are not a mode: they hold whatever removes the session.

**What a strict refusal says (r8, Claude r7 nit 1).** The daemon's note is built from codes (§4), but `last_rm` keeps `e.message`
(handlers.rs:194). So the message must not advise a discard in the daemon's name, and must not say an unchanged tree changed. r7 banned the sentences at :917
and :928 only; :931 is one too, and :892-904 prints "changed old trees" for strict's `changed: false` items. Strict gets its own lines behind the same
`strict: bool`. `Agent`'s are untouched, and the unit tests at :1183-1189 stay.

| Section | `Agent`, as today | Strict |
|---|---|---|
| Prefix | "rm refused: nothing was removed; session S is still Active and listed" (:474-482) | "auto-close refused: nothing was removed; session S is still Active and listed (`git-strata ls`)." |
| `repos` | `repo_line`, then "Publish them, or run `git-strata rm --discard`…" (:917-926) | `repo_line`, then "N of them match rm.regenerable; a `git-strata rm` of your own lets those go" and, when `landing.historical` is non-empty, "N of them have the content of an older publish, not main's current content". |
| `old_trees` | "changed old trees: tool #3" (:892-904); late-write lines (:875-891), then the `--discard` sentence (:930-932) | "tool #3: pre-move tree kept at PATH (changed)" or "(unchanged)", from the item's `changed`; the late-write lines without :931. |
| `embedded_repos` | No such section; it would fall into :927-929 | "tool: nested git repository vendor/x; no publish covers it". |
| `unresolved` | "unresolved: …", then :928 | "unresolved: …", without :928. |
| Recheck | "It changed while it was being removed (…); run the same command again." (:269-277) | "It changed while it was being closed (…); the next publish or push of its repos judges again." No command of the agent's is there to re-run. |
| `OpenFiles` (r9, Claude r8 nit 4) | "Processes 4121 hold files in it; close them and run the same command again." (:345-352) | "Processes 4121 hold files in it; close them, then run `git-strata rm --session S` yourself." The same line at step 3 and at both holders scans of Phase B (:175, :305, step 5.6). |

`attempt_summary` (rm_note.rs:238-240), which `status` prints as `last_rm`, says "changed old trees" for any non-empty list and does not know
`embedded_repos`. It learns both: "kept old trees" when every item has `changed: false` (an `Agent` item has no such key and stays "changed old trees"),
and "nested git repositories" when `embedded_repos` is non-empty.

Everything else is `remove`'s: Phase A (:136-182), the checkpoints (:190-219: after a full publish the overlay is
empty, so they are empty rows that pin nothing new), Removing → retire the session root and its shadow repositories
into the store trash (:384-416, `retire_dir`, strata-store retire.rs:90) → unpin `refs/strata/pins/<sid>` per repo
(:421-430) → Removed (:435-447), which deletes `dev_ports`, `rm_attempts`, `rebase_pending` and `sync_report` in the
same transaction (sessions.rs:676-684). What the close throws away of what the walk classifies is nothing: every entry was
discharged by a projection onto a publish on `origin/<default_branch>` or the overlay was empty, every base is on
`origin/<default_branch>` (clause 4), so is every publish of the session and every target of the `rebase_pending` rows
`Removed` deletes (r5, §2.1 and §3.2), no pre-move tree is kept (clause 7, r7), no nested repository was warned about (r7), and the
root sits in the trash until trash gc (remove.rs:931; `GC_KEEP` 14 d, strata-daemon lib.rs:104) exactly as after an agent's `rm`.
**What goes to that trash unjudged, as with every `rm` (r7, Claude r6 nit 1: r6 said "nothing"):** ignored paths, deps directories,
`.git`, a nested repository under an ignored directory, and anything directly in the session root outside a workspace, whose
listing no step reads. `rm.trash` in the reply names where they went (§6). **Whether that meets the requirement is open for its owner (r10, §11 item 59).** The `session_marks` row (if any) stays with the Removed
row until the session's gc (crates/strata-store/src/gc.rs:284).

**Where the root went (r6, Claude r5 nit 11; both modes).** `retire_dir` returns the trash path (retire.rs:134-142) and
`teardown` drops it (:411-412), so nothing could fill r5's `rm.trash`. `teardown` returns `Torn { not_owned, trash }` in place of
`Vec<PathBuf>` (callers: :314; new/mod.rs:260, which reads `not_owned`), and `RemoveReport` gains `trash: Vec<PathBuf>`,
`#[serde(default, skip_serializing_if = "Vec::is_empty")]`: the destinations, in retire order. It is additive on the wire, so
an agent's `rm` reply carries it too.

### 3.2 Phase B step 5.5: the post-scan recheck of workspaces and kept old trees (every non-discard `rm`)

Codex r1 blocker 3 (r2) and Codex r2 blocker 2 (r3), both verified. Phase B checkpoints the repos one after another
(:191-264); a writer that edits and **closes** a file of repo A after A's checkpoint (:216-218) while repo B is
being checkpointed is seen by nothing: the first recheck compares A against A's own checkpoint (:249-259), the R4
holders scan (:304-305) sees no holder because the file is closed, and teardown retires the root with the write in
it (:398-416). The same holds for a **kept pre-move tree**: Phase A compares every done move's old tree once
(:641-657, `old_tree_unchanged`, move_workspace.rs:884-894, failing closed through `unwrap_or(false)` at :650); a
write and close into that tree afterwards, at `remove.before_removing` (:304) say, is seen by neither holders scan
(the descriptor is closed) nor by r2's recheck (which read workspaces only), and teardown retires the whole session
root, which is where old trees live (`root.join(".strata-tmp-move-<repo>-<hex>")`, move_workspace.rs:525-526 with
lib.rs:83 `MOVE_STAGING_PREFIX`; `old.parent() == session_root`, tests/move_workspace.rs:801-802). Both writes finish
before the first final holders scan, outside the R4 window of :302-303. r3 closes both inside `remove`, for the
agent's `rm` as much as for the hook:

| Step | What | Source |
|---|---|---|
| 5.4 | The R4 holders scan, as today. | :304-305 (`remove.before_removing` seam at :304 stays before it) |
| 5.5 (new) | **Post-scan recheck**, read-only, two parts. **(a) Workspaces** (r2): for every repo of the manifest of an Active session — a tracked repo that step 5.2 found healthy, and **every untracked repo, whose health step 5.5 reads itself with `unhealthy()` (r4, Claude r3 nit 5: the Phase B loop skips untracked repos at :192-194 before the health check at :195), treating `Some(why)` as a change in the :195-203 shape (`workspace: why`)** — a cold `strata_overlay::compute` against the same base (as `dirty_check`'s :555-556: fresh `Tracker::new`, `dirty: None`, so a full walk). A tracked repo's entries must equal its BeforeRemove checkpoint's `entries` (`cp.checkpoint.entries`, :219's `cp`: the entries `put_blobs` returned after any raced re-read, checkpoint.rs:388-398, :407; not `Checkpointed.changed`, Claude r2 nit 7) as a set; an untracked repo's overlay must be empty (what step 4 required of it, :634-639). The comparison is `classify::changed` (strata-overlay classify.rs:94-122, path-indexed entry equality, rename sources included), today `pub(crate)` (:95): make it `pub`, or re-export a `pub fn changed_paths` from strata-overlay's lib.rs. `OverlayEntry` equality is content equality (`EntryKind` carries the blob id, strata-types model.rs:43-58). Any `Unstable` warning of the scan counts as a difference, with or without an entry (the rule of :1108-1111; `unstable()`, :742-750). **In strict mode so does an `EmbeddedRepo` warning, the nested directory listed among the repo's paths (r7, §3.1).** A workspace found unhealthy now is a change too, as :195-203 words it (`workspace: why`). **(b) Kept old trees** (r3): for every repo and every `Done` intent whose tree exists (`old_path`, old_tree.rs:118-120; the `exists` check of :646-649), `old_tree_unchanged(i, cfg)` again, `unwrap_or(false)`: `auto_check` against the move's activity baseline (old_tree.rs:287-314; `Late` or `Skipped` reads as changed, :889-891), then `old_tree_matches` (move_workspace.rs:897-919: `reconcile_full` of the tree against its rollout checkpoint's base and entry equality, :850-867, :903; deps directories and carried ignored entries through `untouched`, :906-918). An error, a missing rollout checkpoint (:899-901) or an unreadable baseline reads as changed, as :650 and status.rs:478-480 already read it. `auto_check` records a late-write notice when it first finds a change (:297, :305, :310); a second call sees it and answers `Late` (:293-294), so the two reads agree. **In strict mode (r7, §3.1) Phase A has already refused every tree that existed, and a new one needs a move, which needs the queue place the close holds, so (b) finds nothing there; it stays one code path.** **(c) Outstanding targets, strict only** (r5, §2.1): `let pending = store.rebase_pending(Some(id))`, then `outstanding(…, &pending, &store.publishes()?)`; any item is a difference. `pending` reduced to `PendingKey {repo, rev, head, planned_head, planned_publish}` is kept for the guard on `Removing` below. Any difference in (a), (b) or (c) → `Dirty changed_during_remove` with the workspace paths per repo in the shape of :266-286, `old_trees: [{repo, n, path}]` (the :651 shape) for (b), `outstanding: […]` (the §2.1 items) for (c), and `"after": "checkpoint"` (the first recheck's refusal gets `"after": "dirty_check"`, additive); nothing is removed, the session stays Active, and the next `rm` checkpoints the write (a workspace) or finds the tree changed in Phase A (an old tree, `kept_trees`), and the next judge meets an outstanding item at clause 4. Non-discard only, like the first recheck (:221); `--discard` is unchanged (§11 item 9). New seam `remove.after_recheck` after it. | new, between :305 and :310 |
| 5.6 (new) | **The R4 holders scan again**, the same batched call over the session root and the shadow repos (:336-338; old trees are inside the root, so they are covered), so that the sentence of :302-303 ("only the lifecycle and phase writes sit between it and the first retire") stays true and teardown's R4 note (:362-365) is unchanged. | `refuse_open_files` (:334-360) |
| then | The fence (:310-311) and Phase C (:313-329) as today. **Strict: `teardown`'s first write is conditional (r5, r6, below).** | |

**The guard on `Removing`, strict only (r5, r6).** Between step 5.5 (c) and the first visible write, a writer that holds only
the repo lock can still change the row (§2.1), and clause 6 was read long before. `teardown` gains
`pending: Option<&[PendingKey]>`, `None` for `Agent` and for `new`'s rollback (:369-374). With `Some`, its first write
(:384-386) is `Store::set_removing_if_unchanged(id, cur, keys, now) -> Result<IfUnchanged>`, one `BEGIN IMMEDIATE`
transaction (txn.rs:37-48) that does, in this order:

| # | In the transaction | When it fails |
|---|---|---|
| 1 | `session_lifecycle(c, id)` equals `cur`. | `LifecycleMoved`: today's `lifecycle_changed` (:388-392). |
| 2 | `read_rebase_pending(c, Some(id))` (rm_state.rs:208) reduces to `keys`. | `PendingMoved` → `Dirty changed_during_remove`, `pending_changed: true`, `after: "checkpoint"`. |
| 3 | **Clause 6's proposals and conflicts (r6; `assigned` is read at clause 6 only, r7, below), in the scopes of clause 6 (r9).** **(a)** The tracked workspaces, `SELECT w.repo, g.deps, w.conflicts FROM workspaces w JOIN base_gens g ON g.repo = w.repo AND g.id = w.base_gen WHERE w.session = ?1 AND w.tracked = 1` (the join of sessions.rs:350), and per repo `SELECT id, to_set, state FROM deps_proposals WHERE repo = ?1 AND state = 'pending'` (at most one, schema.rs:66). An item: non-empty `conflicts` (upstream.rs:402); that `pending` proposal when its `to_set` differs from `g.deps` (upstream.rs:437-443). **(b) r9 (Codex r8 blocker, below): the session's own proposals, joined to no required workspace: `SELECT p.id, p.repo, p.state, w.tracked FROM deps_proposals p LEFT JOIN workspaces w ON w.session = p.origin_session AND w.repo = p.repo WHERE p.origin_session = ?1 AND p.state IN ('pending', 'awaiting_publish')` (the index of schema.rs:69). Every row is an item, `own: true`, and `tracked` is false unless `w.tracked = 1`. A proposal (a) also found is reported once.** **(c)** Any row line 2 read with `reason = 'conflicts'` (the `auto.reason` of sync.rs:163). **(d) r9 (Claude r8 nit 1): `SELECT repo, path FROM sync_report WHERE session = ?1`, one `conflicts` item per repo, with `report: true`.** | `Reopened(items)` → `Dirty changed_during_remove`, `reopened: [{repo, code, …}]`, `after: "checkpoint"`. |
| 4 | The `UPDATE` of `set_lifecycle` (sessions.rs:666-674), then commit: `IfUnchanged::Removing`. | |

A failure rolls back: nothing is retired, the session stays Active, the row and the proposal stay as they are. Nothing in the
transaction reads git or waits on a lock but the store's own, so `:302-303`'s sentence still holds: only the lifecycle and phase
writes sit between the last holders scan and the first retire. `Agent` keeps today's `set_lifecycle`.

The keys catch every change of target: `rev` moves with each one (rm_state.rs:438, :486, :591, :638, :654), `enroll_main`
adds a row, `drop_pending` and a consume remove or clear one, and `plan_replay` and `plan_publish` set a planned column.
`defer_pending` keeps `rev` (:508-514), so a T2 deferral during a close does not refuse it unless its reason is `conflicts`
(line 3).

**Why line 3 is in the transaction (r6, Codex r5 blocker 2).** r5 read clause 6 once, before the judge, under the queue place. The
queue place does not stop what the clause reads from changing:

| Step | What happens | Source |
|---|---|---|
| 1 | Clause 6 passes for S. | upstream.rs:437-443 |
| 2 | Another publish or a fetch reaches `observe_main`, which holds only the repo lock, and proposes main's new deps set for S's tracked sibling. Main-follow enrolls S on that commit, already on L. | proposal.rs:415, :487; main_follow.rs:165, :253 |
| 3 | Step 5.5 (c) finds the commit on L and takes the new row into its keys. | §2.1 |
| 4 | r5's guard finds the keys equal and commits `Removing`. S closes with a pending proposal, which `upstream.deps` would report and the requirement forbids. | autoclose-req.md |

Re-reading at step 5.5 would only move the window, so the read shares the transaction that commits. It covers every proposal and
conflict the clause reads, because a proposal from main is not the only such writer (`assigned` is the one exception, below):

| Writer | What it holds | Source |
|---|---|---|
| `observe_main` → `propose`, and `ensure_owner` → `propose` (`Main` origin) | The repo lock. | proposal.rs:415, :487; main_follow.rs:279 |
| `deps_propose` (S's own `awaiting_publish` draft, **in any repo of S, tracked or not, r9**) | The repo lock at the write; it takes the queue only later, and only for `switch`. **r10: its insert reads S's lifecycle in its own transaction, so it cannot land after `Removing` (below).** | handlers.rs:202-207, :1763-1777, :1786 |
| `defer_pending` with `reason: "conflicts"`, from a T2 attempt | Nothing: `policy_gates` runs outside every lock and `note_outcome` records its answer. | rebase.rs:504-512; pending.rs:518-533 |

`workspaces.conflicts` gains paths only from a move or its resolution (sessions.rs:945, :1022; resolve.rs:148), both under the queue
place, so it cannot change here; it is read anyway, so that nobody has to reason about which half of the conflict read line 3 left out. **`sync_report` (r9) is the same case: its only writer holds the queue place
(sync_move.rs:434, :616), and it is read here for the same reason.** The transactions are serialised (txn.rs:39, :48), so each
writer is ordered before this one or after it. Before: the close refuses. After, by writer: a main proposal is recorded, concerns the sessions that stay and stays listed, because
`Removed` deletes no proposal and `open_proposals` has no origin test for `pending` (deps.rs:570-573); a `conflicts` deferral goes with its row at `Removed` (sessions.rs:682); **and S's own draft
is refused in its own insert (r10, Codex r9 blocker, below). r6 treated the draft like the main proposal, which it is not: `open_proposals` hides a draft once S is `Removed`, and gc reclaims it (gc.rs:942-943).**

**The session's own proposals are read with no tracked filter (r9, Codex r8 blocker).** r6 and r8 read them per tracked repo, at clause 6 and at line 3 (`w.tracked = 1`). Nothing ties a draft to a tracked repo:

| Step | What happens | Source |
|---|---|---|
| 1 | S = `new {repos: ["tool"]}` holds `tool` tracked and `front` untracked: every member gets a workspace. | new/validate.rs:17-21; new/mod.rs:148-153; rpc.rs:95-97 |
| 2 | The agent edits `front`'s `package.json` and runs `git-strata deps propose --session S --repo front`. Neither the CLI nor the daemon asks whether `front` is tracked; `drift` and `build` read the workspace's inputs, and `propose` records `awaiting_publish` with `origin_session = S`. | cmds.rs:73-75, :1135; handlers.rs:202-207, :1756, :1761, :1763-1777; proposal.rs:249-257 |
| 3 | The agent restores the file. The overlay is empty and the draft stays: no command cancels one, because `deps` has only `propose`, `approve` and `list`, and `awaiting_publish` leaves only for `pending` or `superseded`. | args.rs:340-375; schema.rs:364; store deps.rs:394, :435-437 |
| 4 | S publishes `tool`. Both bases are on L (clause 4), and `front` reads as an untouched sibling (clause 7). r8's clause 6 and line 3 do not read `front`, so S closes with `closed: true`. | remove.rs:560-561, :634-639; line 3 above |
| 5 | `Removed` hides the draft from every live view, and gc reclaims it. The requirement's "no pending deps proposals" is false, and nothing says so. | deps.rs:570-573; proposal.rs:146-153; gc.rs:942-943; autoclose-req.md:3 |

**The fix** is the reads above: clause 6 (b) under the place, and line 3 (b) in the transaction, each with no filter on the workspace. Both are one query. The hook reports them as `deps_pending`, with `own: true`, and S stays open and is marked done. **The rest of
the proposal read keeps its tracked filter on purpose:** a `Pending` proposal of another origin stays visible after S goes (deps.rs:571) and a rollout skips an untracked workspace (rollout.rs:349-351), so it strands nothing and does not concern S (§11 item 53). No conflict read needs widening: an untracked workspace never moves (sessions.rs:794), the trigger refuses a change of its `conflicts` (schema.rs:552-565), and `sync` refuses it (sync_move.rs:39-45).

**The way out of a draft is the agent's own `rm`.** A draft on a tracked repo lands when its inputs are published (`promote_draft`, rebase.rs:604-639). One whose inputs were restored never can, and a draft on an untracked repo cannot be published at all: `checkpoint` refuses an untracked workspace (strata-overlay checkpoint.rs:75-84), and `add` leaves a repo the manifest already holds as it is (new/add.rs:70-88). Short of another session promoting the same set (store deps.rs:435-437), the draft ends only with S. So the note and the skill name `git-strata rm --session S` (§4, §8), which refuses on real work and does not read proposals: an explicit act, as for every other `rm` (§11 item 27). A new `deps cancel` would be a new command and a new schema edge for this one case, so it is not proposed (§11 item 52).

**A session's own draft cannot appear after `Removing` (r10, Codex r9 blocker).** r6 ordered every proposal writer against the transaction that commits `Removing` and accepted the ones that land after it, because such a proposal "concerns the sessions that stay". That holds for a main proposal. It does not hold for S's own draft, and r9's line 3 (b) sees only drafts inserted before the commit:

| Step | What happens | Source |
|---|---|---|
| 1 | `deps propose --session S --repo R` finds drift, finishes `build` outside every lock, and stops before it takes the repo lock. | handlers.rs:1756, :1761, :1763 |
| 2 | The agent restores the install inputs, so R's overlay is empty. S has no draft yet. | remove.rs:560-561 |
| 3 | S's publish of another path reaches the close. Every clause passes, line 3 (b) finds no own proposal, `Removing` commits, and the close runs on towards the unpin, holding no repo lock until :424. | §1 steps 4-5; remove.rs:384-386, :420, :424 |
| 4 | The writer takes the free repo lock. `propose` finds no occupant, and `record_proposal` inserts `awaiting_publish`, because nothing in its transaction reads the session. | handlers.rs:1763-1777; proposal.rs:249-257; store deps.rs:372-418 |
| 5 | `Removed` commits and the reply says `closed: true`. `open_proposals` hides the row and gc reclaims it. The writer was told it has a draft, and the requirement's "no pending deps proposals" is false with nothing saying so. | sessions.rs:676-684; deps.rs:570-573; gc.rs:942-943; autoclose-req.md:3 |

**The fix.** When the row it is about to insert has no target (`!target`, store deps.rs:364, so `origin` is `Some`, :365-367), `record_proposal` first reads the origin's lifecycle, inside its own transaction (:372), with `session_lifecycle` (sessions.rs:68-70):

| Origin's lifecycle | Result | Why |
|---|---|---|
| `removing` | `InvalidInput`, `{reason: "lifecycle", session, lifecycle}`, the shape of `set_mark` (marks.rs:84-87). | S is leaving, live under a close or an `rm`, or torn and waiting for `recover`. A draft can land in neither. |
| `removed` | `NoSuchSession` (errors.rs:74-76). | What `assign` answers (below). |
| `active`, `creating`, `unknown` | As today. | Nothing hides a draft of these, and an `unknown` session is never auto-closed (clause 1). |
| No row | As today. | In production only a writer that outlived `Removed` and gc's 14 days (`GC_KEEP`, strata-daemon lib.rs:104) gets here; `proposals` hides the row and gc reclaims it (deps.rs:571-572; gc.rs:942-943). The store's own tests use such origins (tests/deps.rs:226-230; tests/sessions.rs:564). |

An `Err` rolls the transaction back (txn.rs:56-59), so a draft it would have replaced stays as it was. `propose` returns at once (the `?` of proposal.rs:244 and :290); answering `None` instead would mean "retry" and end in `Busy proposal_race` (:194-203). `deps_propose` returns before its `switch` step (handlers.rs:1777), and the set it built is one nothing pins, for `gc_repo` as usual. The two transactions are serialised (txn.rs:39, :48):

| Which commits first | Result |
|---|---|
| The insert | Line 3 (b) reads the draft: `deps_pending`, `own: true`, `after: "checkpoint"`, mark done, S Active, the draft listed. |
| `Removing` | The insert refuses. The writer gets `InvalidInput lifecycle`, or `NoSuchSession` once `Removed` has committed. No row appears, and S's `closed: true` is true. |

**Why drafts only.** Codex's fix reads "reject session-origin proposal insertion", which taken literally breaks two writers. A `Pending` proposal outlives its session (deps.rs:571), and these two insert a `pending` row that keeps its origin, which by then can be `Removed`, because an agent's `rm` reads no proposals (§11 item 27):

| Writer | What it inserts | Reached from | Source |
|---|---|---|---|
| `same_to` | The `Pending` with a new tree and head, `origin: c.origin`. | `observe_main` following main, and any main-origin `propose` with the same `to`. | proposal.rs:258-274, :315-318, :495-500; deps.md:828 |
| `ensure_target` | The replacement with the sealed `to`, keeping P's origin. | `deps approve`. | strata-deps rollout.rs:140-161; handlers.rs:1844 |

Refusing those would make main-follow and a human's approval fail once the proposer has gone, for no gain, since `Removed` hides only `awaiting_publish`. A `pending` row of origin S cannot appear after `Removing` either: `promote_draft` runs under S's queue place (rebase.rs:601-602, :626-638), which the close holds, and a copy needs an own `pending`, which line 3 (b) has already refused. So **once a strict close has committed `Removing`, no open row of origin S can appear**. (An agent's `rm` runs no line 3, so a `same_to` copy of an own `pending` can still appear under it: it stays listed, and nothing claims otherwise.) Originless main proposals have no origin to read, and they carry a target, so they are untouched, as Codex asks.

**It is not a mode.** Like the target writers' checks below, this holds for any removal: an agent's `rm` gains no refusal, and a stale `deps propose` during it is told the truth rather than writing a row nobody will see. It costs one primary-key read in one insert's transaction. No lock or queue is added, so `deps propose` keeps its contract that building comes first and the insert takes only the repo lock (daemon.md:473). store.md's `record_proposal` contract, deps.md's draft bullet (:817) and daemon.md:473 gain the rule **[line numbers at implementation time]**. Tests: §10 row 23. §11 item 57.

**The same state gets the same code.** The hook reports a non-empty `reopened` under its items' codes, in clause 6's shape plus
`after: "checkpoint"`: `reasons: ["deps_pending"]`, `detail: {repo, proposal, state, own, tracked, proposals, after}` (or `reasons: ["conflicts"]`, `detail: {repo, conflicts, report, after}`, r9), the mark and the skill row of §4 and
§8. Which side of clause 6 the writer landed on does not change what the agent is told. `record_rm_attempt` stores it as any
`changed_during_remove`, and `attempt_summary`'s arm needs nothing new (rm_note.rs:226).

**`assign` needs the assignee's queue place, so clause 6's read is final (r8, Codex r7 blocker).** r7 read `assigned` once, at clause 6, and
accepted a late `assign` as a bounded cost. Against the requirement's "no conflicts" it is a violation:

| Step | What happens | Source |
|---|---|---|
| 1 | Clause 6 passes for S. The close runs on to `remove.after_recheck`, holding S's queue place, with S `Active`. | §1; step 5.5 above |
| 2 | A human runs `git-strata assign --file P --session S` for a publish conflict on P. The daemon takes only the repo lock, which the close does not hold. `assign` refuses only `Removed`, so it writes `outcome.assigned` for an `Active` or a `Removing` S alike. | handlers.rs:1226-1240, :1235; rebase.rs:884-887, :909-911 |
| 3 | Nothing reads an assignment again. S becomes `Removed` and the reply says `closed: true`. | line 3 above; sessions.rs:676-684 |
| 4 | Every live session's publish of P answers `Conflict assigned`: `stage` does not ask whether the assignee is alive, and an assignment ends only when a publish lands P, which only the assignee may do. Only a new `assign` frees the path. | stage.rs:525-542; rebase.rs:907, :940-948 |

**The fix.** The `Assign` handler (handlers.rs:1226-1240) takes the assignee's queue place first and the repo lock inside it. The repo lookup of
:1227-1229 stays ahead of both, and the branch with no such publish runs under the place too:

```rust
core.locks.with_sessions(std::slice::from_ref(&p.session), || {
    match repo {
        Some(repo) => core.locks.with_repo(&repo, run)?,
        None => run()?,
    }
    Ok(json!({}))
})
```

`strata_publish::assign` (rebase.rs:884-887) also refuses a `Removing` assignee, with `InvalidInput reason: "lifecycle"` in the shape of remove.rs:143-154;
`Removed` stays `NoSuchSession`. Every live `rm` and close holds the place from before that write to `Removed` (handlers.rs:1246; §1), so under the place `Removing` can only be a torn
remove waiting for `recover` (remove.rs:450-454, :459) or a failed `new` rolling back (:369-374). Neither can resolve a conflict.

**Why this closes it.** The hook holds S's place from before clause 6 through `Removed` (§1 steps 2-5), so the two are ordered:

| Who gets the place first | Result |
|---|---|
| `assign` | Clause 6 reads the assignment: `assigned`, mark done, S open. |
| The hook, and S closes | `assign` waits, then answers `NoSuchSession` and writes nothing. |
| The hook, and S is refused | `assign` waits, then writes. S is open, and the next judge reads it. |
| The hook, and `assign`'s first-take bound runs out (locks.rs:104-113, :535-545) | `Busy lock_wait`, nothing written. A re-run gets one of the answers above. |

So no second read is needed, and line 3 stays free of parsing publish rows under the write lock, which is what r7 objected to (rebase.rs:921-950). Only
assignments naming S matter here. Re-pointing a path from S to T takes T's place and can only remove an assignment of S's, which at worst makes S's refusal stale.

**The lock order is the daemon's own, session queue → repo lock:** `publish` (push.rs:770-784), the post-publish rebase (rebase.rs:143-145), `deps propose --switch`
(handlers.rs:1786-1788), `deps approve` (:1877-1879), `sync` (sync_move.rs:434-435). Every call site of the waiting takes (`with_sessions`, `with_queue_not_publishing`) takes the
queue first. T2's takes go through `PublishLocksNoWait`, whose `try_sessions` never waits (handlers.rs:2555-2571; locks.rs:527-529). So nothing holding that repo lock is waiting for
S's place, and `assign` cannot close a cycle. Three costs, all small for a command a human runs rarely:

- The repo take is no longer the request's first, so it waits without bound (locks.rs:104-113) with S's place held, like `deps propose --switch`. Under a long publish of
  that repo, S's other requests answer `Busy lock_wait` meanwhile.
- `check_session` at the queue take can answer `Busy pins_tightened` (locks.rs:547, :345-353), which is retryable. It fences a row of any lifecycle, so an `assign` naming a session that has already closed can read that too (r10, Claude r9 nit 3). Nothing is written, and the re-run, pinned to the new generation, answers `NoSuchSession`.
- The documented contract changes: docs/modules/daemon.md:483 says `assign` takes the repo's write lock and "不占队列". It becomes "先占被点名会话的队列，再拿该 repo 的写锁".

`assign` needs no `publishing` wait: it writes one publish row's outcome and touches no workspace. `strata_publish::assign` itself stays lock-free, so
`assign_names_resolver_and_does_not_push`'s "no repo lock" assertion (strata-publish tests/rebase.rs:515) stays green.

An assignment made before an agent's `rm` still strands the path, as it does today, because `rm` reads no assignment. That is `Agent`'s explicit act, and §11 item 41 keeps the flag for
rm-followups.

**Every repo, one read (r8, Claude r7 nit 9; own finding).** r7's clause called `assignments(store, repo)` once per manifest repo, and each call reads every publish row
(rebase.rs:922). It also missed a case with no race in it: `assign` does not require S to hold the repo. The row's repo comes from the group's first batch when S is not in
the group (handlers.rs:1227-1229), the assignment is keyed on that row's `outcome.repo` (rebase.rs:923; push.rs:397-398), and `assign` reads only the lifecycle. So S could close
with a path of another repo assigned to it, and strand it just the same. The body of `assignments` (:923-949) becomes `assignments_in(rows: &[PublishRecord], repo)`, and
`assignments(store, repo)` stays as the wrapper `stage` calls (stage.rs:525). A new `pub fn assigned_to(rows: &[PublishRecord], s: &SessionId) -> BTreeMap<RepoName, Vec<String>>`
runs `assignments_in` over the distinct `outcome.repo` of the rows whose outcome has an `assigned` list and keeps the paths naming `s`. With no assignment anywhere, which is the
usual case, it is one pass over rows the hook already holds.

**After `Removing`: the writers check for themselves (r6, Codex r5 blocker 1; Claude r5 nit 7).** r5 said nothing adds a target once
`Removing` is committed, because main-follow skips a session that is not Active. That test reads a snapshot, and the two store
writes behind it test no lifecycle:

| Step | U's main-follow, holding `front`'s repo lock | S's strict close | Source |
|---|---|---|---|
| 1 | Reads S as Active, then pauses before the enroll. | | main_follow.rs:169, :177, :253 |
| 2 | | Proves its rows and commits `Removing`. It holds no repo lock yet. | remove.rs:384-386 |
| 3 | Inserts an unpushed target C and releases the lock. | Waits for that lock to unpin. | rm_state.rs:618-625; remove.rs:424 |
| 4 | | `Removed` deletes C's row, which nothing proved. | sessions.rs:682 |

So every writer that can set a target and holds no queue place reads `session_lifecycle(c, session)` (rm_state.rs:17) inside its own
write transaction, atomically with the write:

| Writer | Sets | r6 rule, in its transaction | Why this rule |
|---|---|---|---|
| `enroll_main` (rm_state.rs:613-626) | A row with `head`. | Needs `active`; otherwise `Rollback(false)`, no row. | Main-follow is the only caller and has already required `Active` (main_follow.rs:177). This makes that test atomic. |
| `retarget_pending` (rm_state.rs:631-646) | `head`. | The same, next to its `rev` test. | The same. |
| `plan_replay` (rm_state.rs:542-573; resolve.rs:861 → pending.rs:274) | `planned_head`. | Skips a session that is `removing` or `removed`; the other plans stand. | `session_heads` serves every session not `Removed` (resolve.rs:877-885). A `Creating` session becomes Active later and must keep its replay target, so `active` would change today's behaviour (sessions.rs:211-217). A strict close only ever goes through `removing`. |
| `settle_planned` with `adopt: true` (rm_state.rs:579-606; pending.rs:77, :283) | `head`. | `Rollback(false)` on `removing` or `removed`. `adopt: false` only clears, and is unchanged. | The same. |
| `promote_pending` with a target (rm_state.rs:410-447; `reconstruct`, pending.rs:112, :223) | `head`. | `Rollback(false)` on `removing` or `removed`. Without a target it only clears, and is unchanged. | `reconstruct` walks every row of the repo under the repo lock alone (pending.rs:199-205). |

The rest need no check. `plan_publish` and a running publish's promotes hold S's queue place or its `publishing` flag, which the close
excludes through `Removed` (push.rs:770-778; resolve.rs:115-118). `set_pending_to_deps`, `defer_pending` and `set_pending_reason` set no
commit. `drop_pending`, `discard_pending_target`, `clear_planned_publish` and `consume_on_move` only clear. Before `Removing`, any of them
that moves a key refuses the close (line 2); after it, none can leave an unproved commit in a row. Every caller already treats `false` as
"moved on": main-follow ignores the bool (main_follow.rs:225-234, :253-261), `promote_one` and `adopt_replay` log only errors
(pending.rs:112, :283), and a skipped plan leaves `settle_planned` nothing to match. So what `Removing → Removed` deletes at
sessions.rs:682 is exactly what step 5.5 proved on L. These checks apply to any removal: an agent's `rm` refuses nothing new, and
simply has no late row to delete (§11 item 27).

What this gives, stated exactly (the one sentence the principle also uses): **every write that `compute`'s walk
classifies (a path that is not ignored, not a deps directory, not `.git`) or that lands in a kept old tree, and that
finished before the first holders scan (step 5.4), is either in a checkpoint or refuses the remove**, since it is on
disk when step 5.5 reads. Ignored paths, deps directories and `.git` are outside both comparisons and leave with the
root into the trash as today (Claude r2 nit 6); so do a nested repository's new files under an agent's `rm`, while a strict close
refuses on the nested directory's warning (r7, §3.1). A write that finishes after step 5.5's read of its tree is lost to the
trash only if its writer opened the file after the first scan (an earlier opener is a holder the first scan refused),
finished after that read, and closed before the second scan (or opened after it: the R4 window as acknowledged today,
:302-303). That residual is the class today's `rm` already accepts at :302-305, no longer preceded by the
completed-write gaps; a filesystem that is not frozen cannot do better without retiring first (§11 item 8). Cost per
non-discard remove: one more cold scan per healthy workspace (the third, after `dirty_check`'s and the checkpoint's),
one `reconcile_full` plus one activity walk per kept old tree (the second of each), and one more `open_files_many`
(the third; its mapped-region pass is the expensive part, open_files.rs:39-41 notes ~0.3 s). Nothing in step 5.5
waits on a lock. `rm`'s message for the new refusal is :269-277's ("It changed while it was being removed; run the
same command again"), which is the right advice for both rechecks.

After the close the agent's shell cwd is a trash path: `resolve_in` returns `None` for a Removed session
(strata-session resolve.rs:52-77), `stale_in` says nothing (cwd.rs:146-148), `select_in` answers `NoSuchSession` for
the id or ticket (resolve.rs:79-89). The reply (§6) tells the agent to `cd ~`; a stray command from the old cwd gets
`NoSuchSession`, whose skill row already says "cd into your workspace or pass --session" (SKILL.md:163; r8 corrects r7's :160), and
`rm --session S` on a closed session is `NoSuchSession` too (remove.rs:138-141), not a failure to retry. **So is a `publish --from S:…` retried after a lost
reply, on a non-TTY too (r8, Claude r7 nit 6, §6).**

## 4. Fallback: `mark done` with the reasons

When any clause fails except `disabled`, `gone`, `lifecycle`, `unknown`, **`busy` (r3)**, **`error` and `upstream_unavailable` (r7)**,
**`extended` (r8)**, the hook writes the P1 mark under the queue place it holds: `store.set_mark(s, MarkState::Done, Some(note), MarkByKind::Daemon, now)`
(marks.rs:61-112; it checks the lifecycle in the same transaction, :81-88).

- **`error` and `upstream_unavailable` write no mark, wherever they arise (r7, Claude r6 nit 5).** r6 stopped at "outside the
  closure, because no place is held" (§1, the three ways out of step 2) and still marked an `error` from inside it. That contradicted the next
  bullet's rule: a transient git or store failure leaves a `done` mark on a session nothing refused. A mark records a refusal, so a
  failed read writes none. The reply says `mark: {written: false, skipped: "error"}`, the audit line is written, the session stays open
  and the next judge decides. `upstream_unavailable` is the same case under its own code: `upstream_best_effort` answers `None` on any
  error or panic (handlers.rs:961-980). `unchecked` (clause 3) still marks: a graph that cannot be computed is the repo's state, not this
  read's failure. A real refusal beside one of these (`["not_pushed", "upstream_unavailable"]`) still marks, and the note lists the
  refusals only. §11 item 44.
- **A failed read inside the judge writes no mark either (r8, Claude r7 nit 2).** Strict `dirty_check` keeps every entry of a repo whose
  `candidates` or `project` read failed (`landed_errors`, `projection_errors`; remove.rs:591-593, :616-619). That is the right answer for a remove,
  but a transient git failure then looks like `unpublished`, and r7's note would say "not published" about work that may be published. So the hook
  reads the `detail` first: let `E` be the keys of those two maps, and drop `E` from `detail.repos`.
  - With nothing left in `repos`, `untracked_repos`, `unresolved`, `old_trees` or `embedded_repos`, the outcome is `error {code: "judge_read",
    errors: {repo: message}}`: no mark, `mark.skipped: "error"`, an audit line, S open, and the next judge decides.
  - With something left, those sections are real refusals: their codes and the mark, with `"error"` added to `reasons`. The note lists the refusals only, as
    for `["not_pushed", "upstream_unavailable"]` above.

  `record_rm_attempt` stores the `Dirty` as the remove answered it, because the remove did refuse. §11 item 49.

  **r9 (Claude r8 nit 2): Phase B's projection is the same case.** `.ok().flatten().unwrap_or_default()` (remove.rs:232-245) turned its failure into `changed_during_remove`, whose note says something wrote during the close, when
  nothing did. Strict returns that `Err` (§3.1), so §1 step 5 answers by its code: `busy` or `error`, no mark, S open. A base gc'd since Phase A is no failed read and still refuses.
- **`extended` writes no mark (r8, Claude r7 nit 4).** Nothing was judged, and an `add` during this run says more work is coming. A done mark would put the
  session it just extended back in line for closure on the next push. `mark: {written: false, skipped: "extended"}`, an audit line, S Active.
- **`add` clears the daemon's own mark (r8, Claude r7 nit 4).** Under the place it already holds (handlers.rs:1496-1499), after `Sessions::add` returns with
  `added` non-empty: when `store.mark(s)` has `by.kind == Daemon`, `set_mark(s, MarkState::Active, None, MarkByKind::Daemon, now)` deletes the row (marks.rs:89-92). A failed read or
  write is logged and does not change `add`'s reply. An `add` whose repos were all present already adds nothing and clears nothing. An agent's, cli's or gui's mark
  stays, for the reason of note ownership below: it is its writer's to undo (§8, §11 item 47).

- **`busy` writes no mark, and `busy` is every `Code::Busy`** (r3, Claude r2 nit 1; r4, Claude r3 nits 3 and 6).
  `Busy publishing` comes from the flag wait (resolve.rs:64-72) and `Busy lock_wait deadline: true` from the post-take
  check (:119-122): the guarded closure never ran, the hook holds no queue place, and a mark written without one would
  race an agent's `mark active` (handlers.rs:1126-1141, under `with_sessions`). `Busy pins_tightened` comes from
  `check_session` at the queue take (locks.rs:547 and :589-590 → :345-353 → `fenced_error` :812-816: a stricter config
  was applied to one of the session's repos) or from the same fence inside the judge after Phase B (remove.rs:310-311,
  holding the place); `Busy du_cancel_timeout` from `with_du_hold` inside the queue closure (handlers.rs:1907-1911;
  du.rs:726-753). r3 keyed `busy` on two reason strings and sent the other two to `error`, which marked then. The rule is
  the code, and the reason not to mark is the same everywhere and is not the queue place: **nothing was judged** — a
  transient stopped the hook before or inside the judge, the session's state is unknown to it, and a `done` mark would
  record a refusal that did not happen. `detail.reason` carries the daemon's reason string. The reply says `reasons:
  ["busy"]` with `mark: {written: false, skipped: "busy"}`, and the audit line (§7) is written (a file append needs no
  queue). The other operation's own outcome, the other publish's hook, the agent's `rm`, or the agent's re-run after
  the transient, is the record; nothing is lost by not marking.
- **`MarkByKind::Daemon`** joins the enum (crates/strata-types/src/mark.rs:17-23, serialised `"daemon"`), the store's
  two match arms (marks.rs:42-47, :94-98) and the CHECK `by_kind IN ('agent', 'cli', 'gui', 'daemon')`
  (crates/strata-store/src/schema.rs:612; r7 §1.3 already lists `'daemon'`). **The v7 migration is amended in
  place** (r3, decided): `main` is at 695f3cd and 8d9663c is not in it (impl/RESUME.md; the main checkout's
  `.git/refs/heads/main`), so no store at v7 has shipped; the CHECK text changes inside the v7 migration and
  `migrate_v7.rs:118-121`'s probe of `by_kind='daemon'` flips from rejected to accepted. The only stores carrying the
  old v7 text are developer stores created from the branch before the amendment (the scratch fixture HOMEs of
  impl/RESUME.md:9). `set_mark` never meets one (r7, Claude r6 nit 3; r3 described a failing CHECK reported in three places, a path
  that does not exist): `check_schema` compares the store's schema with the pinned DDL row for row at open (open.rs:284-301;
  reader :473, writer :693), so the daemon and the CLI answer `Corrupt reason: "schema"` before anything runs, and such a store is
  recreated. A v8 rebuild of the small table is the answer only if a v7 store ships before this
  lands **[re-check `main` at implementation time]**. The daemon kind is informational like the others (§2.1:
  `by_kind` is bookkeeping, not authorization); the `mark` RPC never accepts it (`MarkParams.agent` only
  distinguishes agent/cli, handlers.rs:1127-1136).
- **On the wire** (Claude r1 note, checked): the P1 CLI reads `mark.state` as loose JSON (`ls --table`,
  crates/git-strata/src/cmds.rs:434-449, :445) and never deserialises `SessionMark` (no `MarkBy` use in
  crates/git-strata/src; the Stop hook's sync.rs does not read `mark`); the P1 GUI reads `state` and `note` only
  (apps/desktop/src/lib/view.ts:441-442) behind a compile-time union `'agent' | 'cli' | 'gui'` (lib/types.ts:156)
  that rejects nothing at runtime. So an older P1 CLI or GUI shows a daemon mark correctly; `'daemon'` is added to the
  union for the tooltip text. The one strict reader is the store: a daemon **older** than this change fails `mark()`
  with `bad_row` on a `'daemon'` row (marks.rs:46), which would break its `ls`/`status`. config.md:136's ordering
  rule (new binaries first) covers the upgrade. A downgrade cannot be fixed by clearing marks: the older binary
  cannot open this change's v7 store at all, because its schema check rejects the amended `session_marks.by_kind`
  CHECK as `Corrupt` / `reason: "schema"` before any mark is read (open.rs:293). To roll back, stop the daemon,
  restore a compatible `store/backups/store-v<from>-<ns>.db` as store.md describes, install the matching older CLI and
  daemon (and a config they accept), then start it; keep the displaced database and its sidecars (config.md:147, §8).
- **Note ownership** (r3, Claude r2 nit 3): the hook writes when `mark(s)` is `None` **or the existing row's
  `by.kind` is `Daemon`**; an agent's, cli's or gui's `done` row is kept. `set_mark`'s upsert replaces `note`,
  `by_kind` and `at_ns` (marks.rs:99-104), which would overwrite an agent's note with the daemon's, so the hook reads
  first, under the queue place it holds. Refreshing its own row means a second refusal for another reason shows the
  current reasons in `ls` and the GUI, not the first refusal's. The reasons also reach the agent in the reply and the
  human in the audit line (§7). If P1 later gains §1.3's `reason` column, the reasons move there and this rule stays
  for `note`.
- **An unchanged note is not written (r6, Claude r5 nit 9).** `set_mark`'s upsert always sets `at_ns` (marks.rs:99-104), and §8's
  once-per-mark reason is keyed on `mark.at`, so a re-judge with the same reasons would fire that reason again on every push. When
  the existing row's `by.kind` is `Daemon` and its note equals the new one, the hook writes nothing and reports `mark: {written:
  false, skipped: "unchanged", at: <the row's at>}`. For the comparison to mean anything the note names no trigger (next item).
- **Note text** (the only place the reasons are stored in P1): `auto-close did not close this
  session: <code>[, <code>]: <one line per reason from detail, e.g. "tool: src/a.ts not published", "tool: dist/
  bundle.js not published (rm.regenerable)", "tool: deps proposal 12 is pending", "tool: deps proposal 12 is this session's own and is pending; the human approves it", "tool: deps proposal 12 is this session's own draft and awaits a publish of its inputs; no command cancels it, so git-strata rm --session S yourself if you dropped that change", "front: deps proposal 12 is this session's own draft on an untracked repo, which nothing publishes; git-strata rm --session S yourself when done", "tool: git-strata sync left a conflict on src/p.ts; git-strata sync --session S --repo tool reports it again", "tool: src/p.ts not published (an older publish has this content; main's
  current state does not)", "tool: pre-move tree .strata-tmp-move-tool-1a2b… changed during the close", "tool: pre-move tree
  .strata-tmp-move-tool-1a2b… is still kept (unchanged; your own rm lets it go)", "tool: nested git repository vendor/x is covered by
  no publish", "tool: src/a.ts is assigned to this session", "pids 4121,
  4188 hold files", "front: base 3f9c… is not on origin/main (untracked sibling; push front)", "front: publish 41 (commit
  9c1e…) is not on origin/main (on the publish chain; push front)", "front: publish 41 (commit 9c1e…) is not on origin/main
  and no push lands it (off the publish chain; git-strata rm --session S yourself when done)", "front: commit 9c1e… is on
  origin/release, not origin/main (publish.branch = release: the merge into main is the human's; the daemon never closes
  this session; run git-strata rm --session S yourself when done)", "tool: src/a.ts not published against origin/main
  (the base of this session's newest publish is not built; your own rm judges by D57)", "tool: src/a.ts is in a publish that
  landed as a replayed copy; git-strata sync --session S, then your own rm">`. Every §2.1 item is listed, not only the
  first (r5). A `no_landed_publish` with `published: false` is the plain "not published" line (r5 corrects r4's wording,
  §3.1). r5's text began `auto-close after publish <group id>`, which differs on every trigger and is wrong for a push; the
  trigger stays in the audit line (§7) and in `last_rm.detail` (below). The note is
  cut to 2000 UTF-8 bytes on
  a char boundary, control characters other than tab/newline replaced by a space (`validate_mark_note`,
  mark.rs:42-53). `ls --table`'s MARK column prints only the state; the note is `mark.note` in `ls` and `status` and the GUI badge tooltip (P1 §3.1), and is overwritten only
  by the daemon's own next judgement.
- `record_rm_attempt` (handlers.rs:184-199) stays in the shared helper: a `Dirty`/`OpenFiles` refusal becomes the
  session's `last_rm` (rm_state.rs:116-145; rm_note.rs:262-298). Its `detail` object (:194) gains
  `"by": "auto_close", "publish": <group id> | null, "repo": <pushed repo> | null` so `status` says whose attempt
  it was. **r8 (Claude r7 nit 5): `Code::Unknown` (:187) no longer means `Failed` by itself. `RmOutcome::Failed` says "after teardown began"
  (rm_state.rs:41-42), but a failed holder scan or git read answers `Unknown` on a session nothing touched (§2 clause 7), and the hook makes that path far more common. The
  helper reads the lifecycle: `Active` → `Refused` ("the session is as it was", rm_state.rs:38-39; `status` then says "refused: Unknown; nothing removed",
  rm_note.rs:267-275); anything else, or a failed read, → `Failed`, today's answer. It serves the agent's `rm` as well, and only that record changes. The comment at :179-183 changes with it.**
- A failed `set_mark` is logged and reported in the reply as `mark: {written: false, error: {code, message}}`; it
  never changes the publish's outcome (the rule of daemon.md:103 for enrichment).

## 5. Config: `publish.auto_close`

| Level | Key | Type / default | Where |
|---|---|---|---|
| Per repo | `[repo.X] publish = { branch, push, confirm, auto_close }` | `auto_close: Option<bool>`, `#[serde(default, skip_serializing_if = "Option::is_none")]`; absent = inherit | `PublishTarget` (strata-types model.rs:97-103; `deny_unknown_fields`); the inline-table convention of config.md:42 |
| Global | `[publish] auto_close = true` | `#[serde(deny_unknown_fields)] PublishDefaults { #[serde(default = "PublishDefaults::default_auto_close")] auto_close: bool }` (r4, Claude r3 nit 10: `deny_unknown_fields` like `StatusConfig`, config.rs:27-28, so a misspelt key under `[publish]` is refused, not ignored) with `default_auto_close() -> bool { true }`, **a hand-written `impl Default` returning `auto_close: true` (r5, Claude r4 nit 5: `#[serde(default)]` on `Config.publish` calls `Default::default()` for an absent table, and a derived impl reads `false` whatever the inner field's serde default says)**, `is_default`; on `Config`: `#[serde(default, skip_serializing_if = "PublishDefaults::is_default")] pub publish: PublishDefaults` (r3, Codex r2 nit 1: the inner field's own default is what makes an **empty** `[publish]` table read `true`; a bare `#[serde(default)]` on the field would read `false`) | `Config` (config.rs:9-24), next to `status` (:18-19) and `dev_ports` (:22-23); config.md:12-14's "only `[paths]` and `[repo.<name>]`" sentence names it as it should already name `[status]`/`[dev_ports]`; §2.2's table (config.md:54) gains the field; Appendix A is unchanged (defaults) |

Resolution per repo: the repo's `Some(v)`, else the global value, else `true`. Per session: **every repo of the
manifest must resolve `true`** (clause 0); one `false` disables the hook for that session with
`reasons: ["disabled"]` and `detail.repo`, and then nothing is marked either (the agent's own `mark done` of P1 §2.2
remains its signal). **The manifest it is judged over is the one read under the session's queue place, so a repo `add` brought in
while the hook waited counts (r7, §1).** The flags are read once per hook run from one `core.snapshot()?.config` (r6, Claude r5 nit 10): that
is the `Config` of the catalog generation this request pinned on entry (`handle`, handlers.rs:1046; `snapshot` → `catalog.pinned()`,
startup.rs:436-438; catalog.rs:15; upstream.rs:78), and its `publish` is the global value (r7, Claude r6 nit 14: r6 said "current", and
the behaviour it stated was right). `repo_cfg`
(handlers.rs:69-74) reads the same source but returns only a `RepoConfig`, and `Core.config` (startup.rs:246) is the startup copy,
which a reload does not touch, so neither can supply the global flag. **r8 (Claude r7 nit 3): the snapshot is not the only source. `core.catalog.current()`
(strata-session catalog.rs:84-86) is what periodic_fetch.rs:91 and main_sync.rs:208 already read for this kind of stale-config check, so clause 0 takes both: a repo resolves `true` only when
the pinned generation and the current one both say so, and a repo either lacks counts as `false`. A human who opts out while checks run is therefore honoured for every session not yet past step 3. A `true`
still waits for the next request, because only a refusal may use the newer config. A reload after step 3 does not stop a judge already running; what that judge may do is still
strict and non-discarding.** Neither `[publish]` nor a repo's `publish.auto_close` is a restart field (reload.rs:129-155). **Flipping a repo's key is not
quite inert, though it is harmless (r7, Claude r6 nit 13):** the repo's serialised config changes, so `cfg_hash` changes
(startup.rs:430-435), that repo's Trackers reset at their next `take` (handlers.rs:279-286), and an install in flight for it drops its
result (main_sync.rs:207-210) and is redone. The global `[publish]` is not in `config.repos`, so flipping it does none of this. `auto_close` is **not a pinned flag**: `PinnedFlags`
(strata-daemon lib.rs:174-189; `tighten`, confirm_flow.rs:70-82; `apply`, :107-113) guard against loosening a safety
gate, and no value of `auto_close` can lose anything (a close is a refusable, non-discarding, strict `rm`), so it
needs no pin and no `config relax`. `rm.regenerable` stays pinned as it is (:111) and does not reach the strict judge
at all. Old binaries reject the key (V11, config.md:136's ordering rule: install the new `git-strata`/`git-stratad`
first, then add the key); the new binary reads a config without it. Serialisation: an omitted table and an empty
`[publish]` both read `true` and write back nothing (`is_default`); an explicit `false` is not the default and
round-trips (§10 row 12). Implementation ripple (compile-time, found by the daemon-test audit): every struct literal
of `PublishTarget` and `Config` in the tree gains the new field (`PublishTarget`: strata-types tests
error_and_model.rs:206 and config.rs:248, :447; the test supports of strata-search :297, strata-publish :298,
strata-session :297, strata-deps common :296, :649; strata-daemon tests/support/mod.rs:299-303. `Config`:
strata-daemon tests/support/mod.rs:377; strata-search support :527; strata-session support :546, new_refusals.rs:195,
:218, :333, src/lock_tests.rs:46; strata-publish support :543; strata-types tests/config.rs:118;
git-strata/src/confirm.rs:968) **[re-grep at implementation time]**. **The CLI's live fixture (r7, Claude r6 nit 9).**
crates/git-strata/tests/support/live.rs:118 writes TOML by hand with no `[publish]`, so the product default `true` applies; its guarded
repos push and `quiet_secs = 0` (:127), and tests such as confirm_gate.rs:367-382 go on using a session after publishing it. So
`write_config_guarded` emits `[publish]\nauto_close = false\n` after `[paths]` (:118), the same default as the typed fixture (§10 row
1; §11 items 16, 45). cli.rs's rewrites of that text (`.replace("\npush = false", …)`, :975, :1004, :1037-1039) do not touch the table.
`Home::write_config` (support/mod.rs:151-165) stays as it is: its tests run against `FakeDaemon` or no daemon (timeouts.rs:17;
confirm_gate.rs:25), and only `Live::options` (live.rs:134) serves a real one **[re-check at implementation time]**.

**Split-branch repos** (r4). `auto_close` means the same for a repo whose `publish.branch` differs from
`default_branch` (config.md:68-70, :127: such a repo does not follow main and pins `origin/<default_branch>`), but the
proof of §2 clause 4 cannot pass on the publish itself: the commit is on `origin/<publish.branch>`. A session whose
publish touched such a repo is therefore marked done `not_merged` and stays open, in that call and in every later judge
until the commit is on L (r5, §2.1); an untouched split-branch sibling
does not hold a session open. `auto_close = false` on the repo turns the mark off too (`disabled`). config.md §2.2's
`publish` row gains: "`auto_close` closes a session only when its work is on `origin/<default_branch>`; a repo whose
`publish.branch` differs is marked done instead and is removed by hand". No new key: a per-repo `false` is the opt-out.

## 6. Reply fields

**Daemon, `publish`** (handlers.rs:2138-2142), additive:

```json
{ "published": [...], "already_published": [...],
  "upstream": { "<sid>": {...} },
  "closed": true,
  "auto_close": {
    "closed": true, "reasons": [],
    "rejudged": [],
    "sessions": { "<sid>": { "closed": true, "in_group": true, "reasons": [], "detail": {},
                             "rm": { "session", "discarded": false, "checkpoints": {"tool": 17}, "trash": ["…"], "orphans": [], "warnings": [] },
                             "mark": { "written": false } } } } }
```

- `closed` = every session **of the group** closed (set (a) of §1) — r4 (Codex r3 nit 2): the done-marked sessions
  re-judged because this call pushed their repo (set (b)) are not the caller's, so they never enter `closed` or
  `reasons`; r3 folded them in, and a caller whose own session was removed could read `closed: false` because a
  stranger's session stayed open, which §8's skill text misread as "yours is still open". They appear under
  `auto_close.sessions[<sid>]` with `"in_group": false` and are listed in `auto_close.rejudged`. `auto_close.closed`/
  `reasons` are the group's single session's when the group has one (the CLI's usual case); with several, `closed` is
  the conjunction over the group and `reasons` the union, each prefixed `<sid>:`, and `sessions` carries the
  per-session truth (`in_group: true`). The skill text branches on `data.auto_close.sessions[<its id>]` whenever the
  group has several sessions, and on `data.closed` otherwise (§8). For the `push` reply there is no caller's session:
  `closed` is the conjunction over every judged session, `disabled` and, r8, `extended` ones excluded because nothing judged them (r5, Claude
  r4 nit 8: otherwise one opted-out done-marked session makes every push of the repo read `closed: false`), and `rejudged`
  lists the judged ones; a `disabled` or `extended` one still appears under `sessions`. **With no judged session (every candidate `disabled` or `extended`,
  which step 3 can now cause on a session step 1 passed, r7, r8), `auto_close.closed` is `false` and the top-level `closed`
  is omitted, so an empty conjunction never reads as a close.** `reasons` are codes (§2), never
  sentences: the skill's rule is "branch on codes, never parse `message`" (SKILL.md:140).
- **`gone` on a group session reads as closed (r5, Claude r4 nit 3).** Between a group session's row going `Done`
  (push.rs:892) and its hook taking its queue place, another publish's set (b) can close it (it is done-marked by an earlier
  refusal), or the agent's `rm` can remove it. Its hook then finds `Removed`. The caller's question is "is my session
  gone?", so its entry reads `{closed: true, in_group: true, by: "other", reasons: [], detail: {}, rm: null, mark:
  {written: false}}`, it counts as closed in `closed`, and `upstream[<sid>]` is omitted. r4 answered `closed: false,
  reasons: ["gone"]`, which §8 routed to "tell the human" for a session that was gone. What this judge did stays in the
  audit line: `closed: false`, `reasons: ["gone"]` (§7).
- `rm` is the `RemoveReport` (remove.rs:31-48) of a closed session; its `regenerable` map is always empty (strict,
  §3.1) and is omitted by `skip_serializing_if` (:39). `mark` is `{written: true, at}`, `{written: false, error}` or
  `{written: false, skipped: "busy" | "error" | "extended" | "unchanged" | "kept_<kind>"}` (r3: the row an agent, cli or gui wrote is kept,
  §4; r6: `error`, §1, and `unchanged` with the row's `at`, §4; r8: `extended`, §4) for a
  session left open; `detail` is the §2 object (`landing` included). Nothing else of `RemoveReport` is lost
  (`orphans`, `warnings` as :1269-1274 logs them, `trash` r6, §3.1).
- **`already_published[].push` is refreshed (r6, Claude r5 nit 1).** An entry copies its row's `push` (handlers.rs:2513, :2539), and
  after a row closes only an `Unknown` → `Done` reconcile and `assign` rewrite it (push.rs:518; strata-publish rebase.rs:911), so a
  later push leaves `Refused` in place. The scene: the first publish is refused and takes the error path, with no mark, so the push
  trigger skips the session; a human runs `git-strata push tool` and the commit lands; the agent re-runs the publish, the recovery
  §1 prescribes. r5 then answered `ok:false PushRefused` on every re-run, with a `fix` that answers `NoCommit` (push.rs:291-298), for
  work that had landed. One read-only helper, `refresh_refused(core, b, entry)`, runs at both places an entry is built: `rerun_plan`
  (handlers.rs:2422, from `landed_content`), and the publish loop (:2125, from `landed_before`). The loop is the one this scene
  reaches: the path is still in the checkpoint, so `rerun_plan` keeps it (:2455-2457) and stage answers `NoCommit`. When
  `push.kind == "Refused"` and the entry's commit, or its `landed_copy` for `b.session`, is on
  `refs/strata/remotes/origin/<publish.branch>`, the entry's `push` reads `{kind: "Pushed"}` and it gains `landed_later: true`. Any
  git error leaves the entry as it was. The row is not touched, and `publish_plan`, which shares `rerun_plan`, sees the same repos.
  Set (b) reads the entry like any `Pushed`; the human's push has already run its own trigger, so that re-judge is a repeat.
- `upstream[<sid>]` for a closed session is the read taken before teardown (clause 2's, which the helper was handed, r9, §1), with `as_of_rm: "before"`
  and `unpublished: false` on every repo, as `Rm` does for `!discard` (:1277-1281); for a session left open it is the
  fresh read of :2134-2137 as today. The existing tests `publish_reply_upstream_after_rebase` and
  `publish_upstream_keyed_by_session_multi_session_group` (crates/strata-daemon/tests/upstream.rs:422-465) keep
  passing unchanged under the fixture default `auto_close = false` (§10 row 1); an opted-in copy of each
  (`…_and_closes`) asserts the same `base_head` and `behind` from the pre-teardown read and `as_of_rm == "before"`.
- **`push`**, both forms: the same two fields, built after the lock closure returned (§1), present only when at least
  one done-marked session has an entry, judged, `disabled` or `extended` (r7: r6 said "judged", against its own "a `disabled` one still appears
  under `sessions`"; r9, Claude r8 nit 7: r8 added `extended` above and not here). The confirmed form's JSON (`push`, `commit`, :2259) and the headless form's
  (:2320-2326) are otherwise unchanged.
- **CLI**: `publish_envelope` rebuilds `data` (confirm.rs:854-906), so `closed` and `auto_close` are carried like
  `upstream` (`carry_upstream`, :833-837). r4 (Codex r3 nit 1): the envelope's push check runs over `published` only
  (:861-890); r4 adds the same `push.kind` match over the `already` entries (`Refused` → the `PushRefused` of
  :879-882, with `detail.fix: "git-strata push REPO"`, r5, Claude r4 nit 9), so the `ok:false` envelope of a refused `already_published` push exists and carries `closed: false` with
  `not_pushed`, `push_refused` (:902-905; §2 clause 5; §11 item 22). The `push` command's envelope carries them the same way. `help.rs` DATA (:450-452) gains `closed, auto_close:{closed, reasons[], sessions}`; docs: daemon.md:119
  (`publish` row), :121 (`push` row), :103 (enrichment paragraph: "a closed session's `upstream` is read before
  teardown"), cli.md §3.2 (:121), publish.md:58-70, README.md:560; session.md's `rm` step list (§5.6) gains step
  5.5 (workspaces and kept old trees) and rm-followups §1.8 the strict-mode sentence **[line numbers at
  implementation time]**.
- **A closed session answers `NoSuchSession`, not `NotTty` (r8, Claude r7 nit 6).** A `publish --from S:…` retried after a lost reply fails in `build_batch` (`ro::select`,
  confirm.rs:545; ro.rs:30-34). `is_path_error` is false for that (:512-515), so `gate` runs over every configured repo (:692-697), and on a non-TTY any guarded repo in the config
  answers `NotTty`. The agent is told it needs a terminal for a session that is gone, and auto-close makes this common. The fix is one guard at :691:
  `Err(e) if is_path_error(&e) || e.code == Code::NoSuchSession => return Err(e)`. Nothing is sent either way, and cli.md D30 gains the sentence.
  `confirm_commands_refuse_non_tty` (tests/confirm_gate.rs:36-61; the case at :42) stays green: its `Home` has no store, so `open_store` answers `NoSuchPath` (strata-store open.rs:460-465), and the gate still
  runs. A session that closes between the batch and the plan still reads `NotTty` with `plan_error` (:718-747), and that window is accepted (§11 item 50).
- **GUI**: no new command, and no GUI receives this reply: the desktop backend registers no `publish` or `push` command (apps/desktop/src-tauri/src/lib.rs:148-158). So the requirement's "`closed: true` (CLI and GUI)" is met by the CLI's reply and by what the GUI shows, which is open for its owner (r10, Claude r9 nit 1, §11 item 60). The session leaves the live list at the next `status{summary}` (`live_sorted` excludes
  Removed, strata-session resolve.rs:37-42); a session left open shows P1's `Done` badge with the daemon's note.
  `daemon_status.features` gains `"publish.auto_close"` (strata-daemon lib.rs:124-138; next to `"session.mark"`,
  :126) so the skill and the app can tell an old daemon. P2's `archived` mark (§1.1) will record
  `archive_json.by = {kind: "daemon", trigger: "publish"}`; P1 has no row for a closed session beyond the audit line.

## 7. Audit event

The sidecar of §8, pulled forward with one event: `sessions-audit.jsonl` next to `pins.json` (`pins_path`,
strata-daemon lib.rs:159-161; `<config dir>` :142-145), written through `Core.sessions_audit`, an `AuditFile` with its own writer lock (r4, below), whose body is `audit::append`'s
(audit.rs:95-128: 0600, `O_NOFOLLOW`, 1 MiB rotation to `.1`, partial-last-line delimiting, fsync of file and
directory). The pins loader reads `audit.jsonl` and `audit.jsonl.1` only (:68-94) and skips lines that are not an
`Entry` (:88), so pin history is neither rotated away by session traffic (§8's reason for a second file) nor confused
by it. One line per judged session, closed or not (`busy` included, r3; `extended` included, r8), none for `disabled` and none for a session the
reread dropped (r5, §1). `gone` is written with `closed: false`: this judge closed nothing, and whoever did has its own
line or the agent's `rm` is the record (r5, §6):

```json
{"at":"…","event":"auto_close","session":"s-…","by":{"kind":"daemon","pid":123},"auth":"none","version":"…",
 "trigger":{"method":"publish","publish":42,"repo":null,"in_group":true},
 "closed":true,"reasons":[],"detail":{},
 "rm":{"mode":"strict","checkpoints":{"tool":17},"trash":["/…/store/trash/1762300000000-9f3c…","/…/store/trash/1762300000001-07ab…"]},
 "mark":{"written":false}}
```

`auth` is `"none"` (§8: a preserving close prompts nobody). `rm.mode` is always `"strict"` in P1 (one place to see
that the close was the §3.1 judge). `rm.trash` is `RemoveReport.trash`, the destinations (r6, Claude r5 nit 11, §3.1; r5's example
showed the sources). The texts of a note never go to the file (§8's rule: `note_len` only, here the
note is the daemon's own and its codes are already in `reasons`). A write failure is logged and reported in
`daemon_status` as the pins sidecar's is (confirm_flow.rs:264-265); it never changes the reply. §8's remaining
events (`mark`/`unmark`/`export`/`archive_*`) and the store's `session_mark_events` (§1.3) join in P2 unchanged.

**The file's name needs the requirement owner's sign-off (r8, Claude r7 nit 8; r9, Codex r8 nit 1, Claude r8 nit 8).** autoclose-req.md asks for an "audit.jsonl entry", and this writes
`sessions-audit.jsonl`. r9 keeps the sidecar and does not change the requirement. It reads those words as design-r7 §8 reads the same phrase: brief.md:12 lists "audit.jsonl
entries", and design-r7 §8, headed "`audit.jsonl` entries", answers with "a second sidecar `sessions-audit.jsonl` next to `pins.json` … so pin history is not rotated away by session traffic"
(design-r7.md:1369-1370). That reason is real: `audit.jsonl` is what `load` reads for `pinned_at` and `relaxed_at` (audit.rs:68-94), once at startup (confirm_flow.rs:164), with one rotation. If the owner wants the literal file,
only two things change: the path, and one `AuditFile` shared with `Pins::audit`, so the file keeps one lock (below). The loader skips the extra lines (audit.rs:87-90), and the price is pin
timestamps rotating away sooner. §11 item 6 holds the question, and §9 states the crash window.

**What r9 does about it, short of the answer.** Implementation can start either way, because nothing else depends on the name:

- **The default is the sidecar.** A sidecar found to be the wrong file can be renamed later, while a pin timestamp rotated away cannot be recovered, so it is the choice that loses nothing if the answer comes late.
- **The name lives in one constant,** `SESSIONS_AUDIT_FILE` in audit.rs, next to `ROTATE_BYTES`, and the two changes above are the whole of the switch.
- **Someone looking for the entry finds it.** README.md's path table (:214) gains a row, "Auto-close audit | `~/Library/Application Support/git-strata/sessions-audit.jsonl`", and daemon.md's sentence on the pin sidecar (:1144) names the second file.

**One writer lock per file (r4, Codex r3 blocker 2).** `audit::append` (audit.rs:95-128) serialises nothing itself: it
reads the file's size (`symlink_metadata`, :99-103), renames it to `.1` when the line would cross `ROTATE_BYTES`
(:104), opens the path `create | append` (:107-113), sets the mode (:114), reads the last byte and writes a delimiter
after a partial record (:117-124), writes the line (:125) and fsyncs file and directory (:126-127). Its only callers
run under the `Pins` mutex: `Pins::audit` (confirm_flow.rs:257-262) is called with the locked `PinsState` (:166-172
at load, :203 at reload, :253 at `accept`; `inner: Mutex<PinsState>`, :122-125), which is what makes `audit.jsonl`
safe. The hook has no such lock: it runs on the request's thread (`startup.rs:717` spawns one per connection) under
the **session's** queue place, and two sessions' hooks — two publishes, a publish and a push — run at once. Two
unserialised appends to one file can both pass the size check and both rename: the second rename replaces `.1` with a
file holding one line, discarding the retained history; or A renames the inode B has already opened, and B's line
lands in `.1` after the rotation while B's delimiter read races A's write. The fix is one lock per file, held across
the whole sequence: `pub struct AuditFile { path: PathBuf, rotated: PathBuf, rotate_bytes: u64, lock: Mutex<()> }` in
audit.rs, with `fn append_line(&self, line: &[u8]) -> io::Result<()>` whose body is today's :99-127 parametrised on
the two paths and the threshold, entered after `self.lock.lock().unwrap_or_else(|p| p.into_inner())` (r5, Claude r4 nit 4:
the crate's pattern, handlers.rs:65-67; a panic on a request thread while holding it must not silence every later audit
line, and the guard protects no data, only the order of the sequence); `append(pins, entry)` keeps its signature for the
pins callers and calls the same body (the pins file's lock stays `Pins.inner`, taken by every caller already; a
second mutex inside would be harmless but is not needed). `Core` gains `pub sessions_audit: AuditFile`
(startup.rs:243-263, next to `pins` :260), built next to `Pins::load` (:307) from
`pins_path(&opts.config_dir).with_file_name("sessions-audit.jsonl")`, `.1` as its rotated name and `ROTATE_BYTES`
(1 MiB, audit.rs:12). The daemon is single-instance (`InstanceLock::take`, startup.rs:273; `lock_path`, lib.rs:153-156),
so an in-process mutex is the whole story: no other process writes the file. The lock is held across two `fsync`s —
milliseconds, once per judged session, on the publish reply path; it orders nothing else: **it is taken after the session's
queue place was released, with no lock held (§1 step 6; r6, Codex r5 nit 2: r5's §1 and this sentence disagreed)**, and nothing
is taken under it (§11 item 23). After the closure is the only order that also serves a `busy` from the queue take, which holds
no place, and it keeps two `fsync`s out of the session's queue. Two triggers' lines for one session can therefore reach the
file out of order; each carries its own `at`. A write failure is logged and reported as before. Tests: §10 row 4.

## 8. Interaction with the Stop hook and the P1 mark

- **Closed session.** The hook resolves its session from the input cwd (sync.rs:106-111; `request["session"] =
  null`, :109); a Removed session's directory no longer resolves (strata-session resolve.rs:52-77) and `sync_session`
  answers `NoSuchSession` (handlers.rs:982-997), which the hook's `.ok()?` turns into silence (sync.rs:111). The hook
  never names a closed session. The agent's loop ends on its own session's `closed: true`
  (`data.auto_close.sessions[<sid>]`; `data.closed` for a single-session group, §6) with `cd ~`.
- **Session left open, marked done by the daemon.** `block_reason` (sync.rs:128-188) has two branches. An escalated `auto`
  (`escalation_new`, state `deferred`, :131-153) speaks for any repo; for a non-following one the sentence names no `sync`
  (:147; `stuck_hook_blocks_clean_workspace_once_and_does_not_offer_sync_when_not_following`, tests/sync.rs:320). r5 said a
  split-branch repo is never spoken for, which is true of the other branch only (r6, Claude r5 nit 3). That branch speaks only for a following repo
  (`!u.following` → skip, :160; a split-branch repo is `following: false` by default, config.rs:324-326) that is behind main or has pending deps (:161) **and** has `unpublished` (default `true` when
  the Tracker is cold, :162) or conflicts (:163-164). `behind` is the number of `main_tip` commits the base lacks
  (upstream.rs:135-141, the tip of :77-78). r3 said "after a full publish `behind` is 0, so the hook is silent": that
  is true of a **`Moved`** session only (r4, Codex r3 nit 3). A `Deferred`, `Waiting` or `OpenFiles` session left open
  — by holders (`open_files`), by a write during the close, by an unpublished sibling edit, by `not_pushed` on a chain
  that leads — is behind by at least its own publish commit, and F1 reads `unpublished: true`. r5 (Claude r4 nit 6)
  corrects why: the helper drops every Tracker before the judge (handlers.rs:1255, as a refused `rm` does today), so until
  `reconcile_once` (handlers.rs:459) or the next request's `take` (:279-291) re-adds one (r7, Claude r6 nit 14: `watch_tracked` runs only at the loop's start, :649) the read is `Live::default()`, `true` whatever the overlay holds (:350-352; daemon
  upstream.rs:29-35), and once it is back the published entries are still in the overlay; r4 called the Tracker warm. The
  Stop hook then says "main moved N
  commits since your base in REPO. Run `cd ~ && git-strata sync --session S` …" (:173-185). That advice is right in
  substance: the sync moves the session onto its publish (the move's own T3 judgement discharges the entries), after
  which `behind` is 0, F1 reads `false` and the hook is silent; the session is still open and marked done, and the
  agent's own `rm` ends it (below). **r6 (Claude r5 nit 3): r5 said the hook "never says publish". It does: that sentence ends
  "cd back to the workspace, resolve any conflicts it returns (…), cd back, re-run checks, then publish" (:185), and the
  escalated one ends "and cd back into the workspace" (:141). On a done-marked session that advice does harm: an agent that cds back
  is a cwd holder (open_files.rs:32-33), so its later `rm` refuses `OpenFiles`. So `block_reason` gains `done: bool`, which the hook
  reads as `data["mark"]["state"] == "done"` (the reply carries the mark, handlers.rs:1038-1040; the call is sync.rs:117). With
  `done` the following-repo sentences stop at the sync: "main moved N commits since your base in REPO. Run `cd ~ && git-strata sync
  --session S`; if it returns conflicts, resolve them as it says. This session is marked done: stay out of the workspace, because a
  shell inside it holds the session open, and run `mark active` first if you mean to keep working", and the escalated one stops
  at "then run `FIX`". The non-following sentence (:147) and every sentence for a session with no done mark are unchanged, and an older daemon's reply, which has no `mark`, gets the old words.** With
  §9 item 2 (not in
  the worktree's `sync.rs` as read; it ships with P1 or after), the once-per-mark reason is keyed by the daemon's
  `mark.at` like any other and worded by the first code: `unpublished`/`untracked_unpublished` → "publish it, or
  `mark active` to keep working; do not discard" (with `detail.regenerable` non-empty: "the listed rm.regenerable
  paths are build output by your repo's rule; publish them or run `git-strata rm --session S` yourself, which lets
  exactly those go"; with `detail.landing.*.historical` non-empty: "the listed paths have the content of an older
  publish, not of main now: publish them if the revert is meant, else restore them"); `open_files` → "close the
  processes holding the workspace (`cd ~` for your own shell), then `git-strata rm --session S`";
  `changed_during_remove` → "something wrote into the workspace or a kept pre-move tree during the close; when your
  tools are idle run `git-strata rm --session S` yourself" (with `detail.outstanding` or `detail.pending_changed`, r5:
  "main moved under the close; the next publish or push of the repo judges again"); `not_pushed` (r3 wording, Claude r2
  nit 4; r5 by `on_chain` and `chain`, §2.1) with `on_chain: true`, `chain: leads` → "ask the
  human to push REPO (`git-strata push REPO`); the next push of REPO, by `git-strata push` or by any publish that
  pushes REPO, closes this session; if it is still listed after that push, run `git-strata rm --session S`
  yourself", with `chain: diverged` → the same, with "a repo that needs a confirmation: `git-strata chain --repo REPO
  --accept-origin replay` first" (r7, Codex r6 nit 2), and with `on_chain: false` → "no push lands COMMIT; tell the human if it was meant to
  land; run `git-strata rm --session S` yourself when done"; **`not_merged` (r4) → "REPO publishes to BRANCH, not to DEFAULT; this session's work is on origin/BRANCH
  and the daemon never closes a session on that alone; when you are done, run `git-strata rm --session S` yourself (it
  judges against origin/BRANCH); the merge into DEFAULT is the human's"**; `unpublished` with
  `detail.landing.<repo>.why_none == "no_built_base"` (r4, Claude r3 nit 4) → "the listed paths are published but
  the base the judge would project onto is not built; run `git-strata rm --session S` yourself (it refuses `Dirty`, so it
  is safe)"; with `why_none == "no_landed_publish"` (r5, §3.1) and `published: false` → the plain "publish it" sentence,
  and with `published: true` → "these paths landed as a replayed copy; run `git-strata sync --session S`, then
  `git-strata rm --session S` yourself"; **r7: `kept_trees` with every item `changed: false` → "a pre-move tree is still kept (something
  held it during the move, or it holds ignored files the move did not carry); nothing in it changed, so `git-strata rm --session S` lets it
  go when you are done"; `embedded_repo` → "PATH is a nested git repository no publish covers; move out what you need, then `git-strata rm
  --session S`, which takes it to the trash"; `assigned` → "a publish conflict on PATH is assigned to this session; publish PATH, or tell the
  human to assign it to another session (`git-strata assign --file PATH --session OTHER`)"**; **r9: `deps_pending` with `own: true` and `state: awaiting_publish` → "proposal N is this session's own draft: publish its install inputs if the change is
  meant (an untracked repo, `tracked: false`, cannot be published); no command cancels a draft, so if you dropped the change, run `git-strata rm --session S` yourself when you are done"; `conflicts` with `report: true` → "your `git-strata sync` left a conflict on PATHS:
  `cd ~ && git-strata sync --session S --repo REPO` reports it again; resolve it as that says and the next publish or push of the repo judges again, or run `git-strata rm --session S` yourself if you no longer want the sync"**; any other `conflicts` or `deps_pending`,
  `unresolved`, and `kept_trees` with a `changed: true` item → "tell the human: `status` names it" (r9: "`status` or `git-strata deps list`" for `deps_pending`, because `status` shows no own proposal on an untracked repo, upstream.rs:338-340). The docs
  sentence of §9 item 5 (hooks.md:34-49, cli.md:149-157) gains "or closed by the daemon after a publish that landed
  everything", and the install note of config.md:136 / README gains the downgrade warning (config.md:147): an older
  daemon can neither read a daemon-made mark nor open the v7 store (schema check, `Corrupt` / `reason: "schema"`), so
  clearing marks does not help; to roll back, stop the daemon, restore a compatible
  `store/backups/store-v<from>-<ns>.db` (store.md), install the matching older CLI and daemon (and a config they
  accept), then start it, keeping the displaced database and its sidecars.
- **P1 `mark`.** An agent's `mark done` before its publish neither triggers nor blocks the hook; `mark active` after
  the daemon's mark is allowed (§1.2 undo) and the next publish or push judges again; `mark` on a closed session
  fails: `NoSuchSession` from the CLI's selector (`select_in`, strata-session resolve.rs:79-89), `InvalidInput reason:
  "lifecycle"` with `removed` at the socket (marks.rs:81-88). The P1 skill text (§2.2 step 8) becomes:
  **"Publish from `cd ~`. Read `data.auto_close.sessions[<your session id>]` (r4; `data.closed` is the whole group's
  and never counts other sessions the push re-judged). If its `closed` is `true` the session is gone: there is
  nothing to mark or remove; stay out of its path. Otherwise `reasons[0]` says why it stayed open: `disabled` → the
  daemon neither closes nor marks for this repo set; mark done and remove by hand as before (`mark done`, then
  `git-strata rm --session S` when finished); `unpublished` → publish the listed paths (or `mark active` and keep
  working); if `detail.regenerable` lists some of them, those are build output by the repo's rule and your own
  `git-strata rm --session S` lets them go; if `detail.landing.<repo>.historical` lists some, they hold an older
  publish's content, not main's: publish them if the revert is meant; if `detail.landing.<repo>.why_none` is
  `no_built_base`, the paths are published but the judge cannot prove it: run `git-strata rm
  --session S` yourself (it refuses `Dirty`, so it is safe); if it is `no_landed_publish` with `published: true`, they
  landed as a replayed copy: run `git-strata sync --session S`, then `git-strata rm --session S`; `open_files` → close the holders, then `git-strata rm
  --session S` yourself; `changed_during_remove` → wait until your tools are idle, then `git-strata rm --session S`;
  `not_pushed`, `push_refused` → `detail.repo` may be another repo than the one you just published, and an earlier publish of yours may be what is waiting (`detail.outstanding` lists them all); if
  `detail.on_chain` is `true`, hand the push to the human; the next push of that repo closes the session, and
  `git-strata rm --session S` is the fallback if it stays listed; if it is `false`, no push lands that commit: tell the human if it was meant to land and run `git-strata rm --session S` when done; `not_merged` → the repo publishes to another branch
  than its default, in this call or an earlier one: the daemon never closes such a session; run `git-strata rm --session S` yourself when done and
  tell the human the merge is theirs; `kept_trees` with every `changed` false → run `git-strata rm --session S` when you are done;
  `embedded_repo` → `detail.embedded_repos` names a nested git repository no publish covers: move out what you need, then `git-strata rm
  --session S`, which takes it to the trash; `assigned` → `detail.assigned` lists every path, by repo: publish them, or tell the human to assign them to another session (for a repo that is not one of this session's, only the human can);
  `deps_pending` with `detail.own` true and `detail.state` `awaiting_publish` → your own deps draft never landed: publish its install inputs if you meant the change (a repo with `detail.tracked` false cannot be published); no command cancels a draft, so if you dropped the change, run `git-strata rm --session S` when you are done;
  `conflicts` with `detail.report` true → your `sync` left a conflict: `cd ~ && git-strata sync --session S --repo <detail.repo>` reports it again; resolve it as that says, or run `git-strata rm --session S` if you no longer want the sync;
  `busy` → another operation had the session, nothing was judged or marked; run
  `git-strata rm --session S` when it is done, or publish again; `extended` → a repo was added to this session while this call ran: nothing was judged or marked and the session is yours, so publish the new work when it is ready; anything else → tell the human. If `mark.written`
  is `false` and `mark.skipped` is not `unchanged`, these reasons are in this reply only: act on them now. Never `rm --discard`."** The same sentences go to SKILL.md:98-99 (loop step 8) and :174 (the `Dirty`
  row; r8 corrects r7's :171), publish.md:137-146, README.md:287-289 and :487 (r9 corrects r3's :467), and help.rs's `RM_AFTER` (:536-543; r8 corrects r7's "the `rm` text at :522-547", which also covers `MARK_AFTER`, next bullet).
- **A `done` mark enrols the session in auto-close, and the text must say so (r8, Claude r7 nit 7).** Under §1 any `done` mark, whoever wrote it, makes a session a candidate on a later
  publish or push of its repos. A human who marks a session done to keep its workspace can lose that workspace to the trash on another session's push, and today's text promises the opposite. r7 also
  said Hard rule 1 "is unchanged", which is false: it says "leave removal to the human", against the requirement ("agents need no manual rm") and against this section's own `rm` sentences.

  | Where | Today | New text (r8; r9 for the rows so marked) |
  |---|---|---|
  | `MARK_AFTER`, help.rs:527-529 | "Finish agent work with mark done; leave removal to the human. … Neither changes files, checkpoints or lifecycle." | "A full publish closes its session by itself. When it stays open, the reply says why. A done session, marked by anyone, is closed by the daemon on a later publish or push of its repos once everything has landed, unless one of its repos has publish.auto_close = false: its workspace goes to the trash (recoverable until trash gc) and its checkpoints stay. To keep a session, mark it active: that clears the mark, note and attribution. The mark command itself changes no files, checkpoints or lifecycle." |
  | help.rs:31 | "mark done / mark active are metadata only; agents finish with mark done." | "mark done / mark active ask nothing; a done session can be auto-closed later (git-strata mark --help)." |
  | help.rs:201 | "mark --session S done after publishing; leave removal to the human." | "A full publish closes the session; otherwise read data.auto_close, which says what is left." |
  | `RM_AFTER`, help.rs:538, :542 | "# agents finish here"; "Agents finish with mark done, not rm; resume with mark active." | "# the daemon's fallback after a publish"; "Agents finish with publish: a fully landed session closes itself. If it stays open, data.auto_close says when `rm` is yours to run; resume with mark active." |
  | SKILL.md:183-185, Hard rule 1 | "Finish with `mark done`, not `rm`; leave removal to the human. Marking preserves files and checkpoints. … `rm` without discard remains available for existing automation, outside this default loop." | "A full publish closes the session; you need no `rm`. If it stays open, follow `data.auto_close` (step 8), which says whether `git-strata rm --session S`, never with `--discard`, is yours to run. A done mark, yours or the daemon's, lets the daemon close the session on a later publish or push of its repos once everything has landed: the workspace goes to the trash and the checkpoints stay. To keep working, or to keep the workspace, run `git-strata mark --session S active`, and do so before an `add` if you marked it done yourself." The rest of the rule, from "Never bypass a human gate", is unchanged. |
  | README.md:303-305 (r9, Claude r8 nit 5) | "Agents finish with `mark done` and leave removal to the human. Existing `rm` automation remains supported outside the default loop; discard still requires the human's TTY and macOS confirmation." | "A full publish closes its session: the workspace goes to the trash and the checkpoints stay. When it stays open, `data.auto_close.reasons[0]` says why and what is left, including when `rm` is yours to run; a done mark is written only when the daemon's judge actually marked the session (not when it was `disabled`, `busy`, `extended`, failed, or kept another writer's mark). Discard still requires the human's TTY and macOS confirmation." The next sentence, "Marking adds no archive/export operation", is unchanged. |
  | args.rs:205 (r9, nit 5) | "Remove a session (agents finish with `mark done`)" | "Remove a session (a full publish closes its session by itself)" |
  | skills/git-strata/reference.md:57 (r9, nit 5) | "Outside the default agent loop (finish with `mark done`)." | "A full publish closes the session; run this when `data.auto_close` says it is yours." The rest of the cell is unchanged. |
  | skills/git-strata/SKILL.md:3 (r9, nit 5) | "Covers the CLI loop (new, checkpoint, sync --check, publish, mark done)" | "Covers the CLI loop (new, checkpoint, sync --check, publish, which closes a fully landed session)" |
  | docs/modules/cli.md:629-631 (r9, own sweep) | "Finish agent work with `git-strata mark --session S done --note "what landed and what is left"`. This marks completion without removing files, releasing the workspace, changing the lifecycle, or deleting checkpoints. The human handles session removal." | "A full publish closes its session. `git-strata mark --session S done --note "what landed and what is left"` records completion and itself removes no files, releases no workspace, changes no lifecycle and deletes no checkpoints, but the daemon can close a done session on a later publish or push of its repos once everything has landed." |

  **Left as they are, on purpose (r9):** README.md:800, help.rs:131 and :207 and reference.md:57's last clause say that a `Dirty` from an agent's own `rm` means "mark done and report unpublished work". That is still true: a done mark on a session with unpublished work is refused at every later judge, so it cannot close that work away.
- **Why the `open_files` and `changed_during_remove` cases need the agent.** Nothing re-judges a session when its
  holders go away or its writers stop; a later publish or push of its repos would, but a finished session has none.
  The agent's own non-discard `rm` (Q4: allowed, harmless; since r2 it runs the §3.2 recheck too) is the way; the
  alternative, a daemon retry on the Stop hook, would put a remove on a 5 s read path (`HOOK_BUDGET`, sync.rs:17)
  and is not proposed. The hook does not retry a `changed_during_remove` either: a write during the close means
  something is still writing, and the session should stay open until it stops. `not_merged` (r4) is the agent's too:
  the event it waits for — a merge into the default branch on the remote and a fetch of its tracking ref — is not the
  daemon's, and the daemon has no trigger on it (§11 item 20). `not_pushed` is the one case the daemon does re-judge
  (§1: the push trigger and, r3, a publish that pushes the repo), because the event it waits for is the daemon's own;
  with `on_chain: false` there is no such event (r5), so that case is the agent's too. Another session's push can still
  re-judge a session whose reason was curable (a holder gone, a writer stopped): set (b) does not narrow by reason (§11
  item 30). **r9: an own deps draft and a saved `sync` report are the agent's too:** nothing the daemon waits for ends either (§3.2, §2 clause 6).

## 9. Races

| Race | What happens | Source |
|---|---|---|
| Another publish of the same session starts (a second group, or the human's) while the hook waits. | `publishing` is true (a `Running` row naming the session, resolve.rs:41-47); the hook waits at most one lock wait in total (flag wait, queue take and post-take check share the deadline, §1), holding nothing, and gives up with `Busy publishing` or `Busy lock_wait deadline: true` → `busy`, **no mark** (nothing was judged; §4), audit line written. The other publish's own hook judges when it lands. | resolve.rs:49-131 (M5, N5, R5 decisions); locks.rs:499-501, :536-542 |
| The agent runs `rm`, `mark`, `checkpoint` or `sync` on the same session during the publish. | All take the session's queue place (`with_sessions`, handlers.rs:1126, :1142; locks.rs:484-486), so they run before or after the hook, never inside it. An agent `rm` that wins makes the hook find `Removed` → `gone`, no mark, read as closed for a group session (r5, §6); one that loses gets `NoSuchSession`. An agent `mark done` that wins is kept (§4: only a `daemon` row is overwritten). | handlers.rs:1246; remove.rs:138-141 |
| **`add` of a repo while the hook runs (r7, Codex r6 blocker, Claude r6 blocker 1; r8, Claude r7 nit 4).** | Before the hook has its queue place: step 3 reads the new manifest and clause 0 judges it there. A `false` → `disabled`, no mark, no audit line, S and both workspaces stay; **a `true` → `extended` (clause 0b): no judge, no mark, an audit line, S and both workspaces stay (r7 judged the new repo like any member and could close the session the `add` had just extended). A daemon mark is gone by then, so on set (b) or `push` the reread drops S without a judgement.** After the hook has the place: `add` waits for it and then finds S closed (`NoSuchSession` or `InvalidInput lifecycle`, new/add.rs:54-64) or, if S was refused, adds as usual. `add_repos`'s main-follow after the queue (handlers.rs:1509) holds only the repo lock, and §3.2's writers cover it. | handlers.rs:1494-1499, :1496; new/add.rs:54-64, :163-168; §1 |
| **`assign` names S during the close (r7, Claude r6 nit 2; r8, Codex r7 blocker).** | `assign` now takes S's queue place before the repo lock. Before the hook's place: clause 6 reads it → `assigned`, mark done, S open. During the hook: `assign` waits. S closed → `NoSuchSession`, nothing written. S refused → the assignment lands and the next judge reads it. Its own first-take bound can answer `Busy lock_wait` first, with nothing written. So no close leaves a path assigned to the session it removed. A `Removing` S under the place is a torn remove, refused `InvalidInput lifecycle`. An assignment made before an agent's `rm` still strands the path, as today (§3.2, §11 item 41). | handlers.rs:1226-1240, :1246; rebase.rs:884-887, :907; stage.rs:525-542; locks.rs:104-113 |
| **The holder scan names the daemon itself (r7, Claude r6 nit 12).** | The scan skips only the daemon's own event-only descriptors, and T2's gates walk a workspace outside every lock, so a scan that meets such a walk can answer `open_files` with the daemon's pid. Rare and safe: S stays open and marked done, `detail.open[].command` says who held it, and the agent's `rm` or the next judge passes. Not filtered: a filter would need to prove the daemon's own fd is harmless, and the refusal costs one mark. | open_files.rs:256-259; strata-publish rebase.rs:338, :344 |
| **`push`: the repo lock and the hook (r2).** | The hook starts only after `with_repo`'s closure has returned (:2260; :2288), so the request holds no repo lock when `remove_strict` reaches the cleanup lock (remove.rs:424); the reentrancy refusal (locks.rs:435-442) and the unbounded retry (lib.rs:305-307) cannot meet. Between the closure's return and the hook's queue place another request may act on the session: a new publish → `busy`; an agent `rm` → `gone`; an edit → the judge sees it. Lock order stays session queue → repo lock everywhere (§1). | handlers.rs:2225-2260, :2283-2288; locks.rs:453-468 |
| Holders appear: the agent `cd`s back into the workspace, or a dev server is started, between the publish and the hook. | `refuse_open_files` runs at Phase A (:175), before the recheck (:305, step 5.4) and after it (step 5.6); a holder seen by any refuses `OpenFiles` → `open_files`, mark done, nothing removed. One that appears after the last scan and before the retire is the R4 window today's `rm` has (:302-305); not new. An agent that publishes from inside its workspace (against SKILL.md:91's `cd ~`) is itself a `Hold::Cwd` holder (open_files.rs:32-33) and the post-publish move is already `Deferred open_files` (rebase.rs:52-59, :583-585): the close refuses and the note says so. | remove.rs:334-360 |
| **A write lands after the publish's checkpoint (r2, three windows; r3 adds the kept-tree window).** | Before Phase A's cold reconcile (:550-558) → `unpublished` (regenerable or not, §3.1). Between Phase A and the repo's BeforeRemove checkpoint → the first recheck, `changed_during_remove after: dirty_check` (:249-286). After the repo's checkpoint, while other repos are checkpointed or the first holders scan runs, by a writer that has closed the file → **the post-scan recheck, `changed_during_remove after: checkpoint` (§3.2)**; by a writer still holding it → `open_files` at step 5.4 or 5.6. **Into a kept old tree after Phase A's comparison (:641-657)** → the post-scan recheck's part (b), `changed_during_remove after: checkpoint` with `old_trees` (r3; an agent's `rm` only, since a strict close has already refused any kept tree in Phase A, r7). Residual: a writer that opened after step 5.4, finished after step 5.5's read of its tree and closed before step 5.6 (or opened after 5.6): the R4 class, bytes in the trash until gc. Nothing is removed in any detected case. | remove.rs:555-556, :266-286, :641-657; §3.2 |
| **A fresh revert matches an older publish (r3).** | Not a race, a state: the strict judge projects onto the newest publish on `origin/<default_branch>` (r4's anchor) that descends from the base and never matches content against older ones (§3.1), so the revert stays `unpublished` with `landing.historical` naming it; the agent's own `rm` keeps today's D57 answer (§11 item 7). | remove.rs:573-577, :943-975, :1061-1106 |
| The post-publish rebase of this session was deferred or is `Waiting` on a deps proposal. | The workspace is on its old base with the published entries in its overlay; the projection onto the publish commit lets them go when that commit is an ancestor of the **default branch's** tracking ref (`candidates` with `cfg.default_branch` in strict mode, §3.1; :981, :1008-1010) and its base is built, which the rebase did before deferring (rebase.rs:471, §3.1), so the close succeeds iff pushed to the default branch (a split-branch repo's publish is `not_merged` at clause 4 before the judge runs, r4); `Waiting` sets `deps` (clause 6) and keeps it open. | rebase.rs:52-62, :471, :504-512; remove.rs:573-595 |
| **Another session's publish pushes a repo this done-marked session waits on (r3; r4 widened and bounded).** | That publish's hook judges this session too (§1, set (b)): `closed` if everything else holds, else its mark is refreshed with the current reasons (§4); the outcome goes under `auto_close.sessions[<sid>]` with `in_group: false` and never into the caller's `closed` (§6). r4: the set also keys on `already_published[R].push == Pushed` (handlers.rs:2115-2126, :2539) — a first run that pushed R and then failed on a later repo took the error path and ran no hook; the re-run reaches the success path with the push recorded there (Claude r3 nit 2) — and it excludes split-branch repos, whose push moves no ref the proof reads. Without this, the session stayed `not_pushed` until an agent's `rm`, because a later `git-strata push` of a chain that no longer leads answers `NoCommit` (push.rs:292-297). | handlers.rs:2111, :2115-2126, :2539; push.rs:666-668 |
| Multi-session group. | Sessions are judged in id order, each under its own queue place; a session's close deletes only its own pin (:421-430) and never another's base, which clause 4 holds on origin anyway. The re-judged done-marked sessions (set (b)) follow, in id order, each under its own queue place. | handlers.rs:2042-2043 |
| `config reload` during the publish or between the predicate and the judge. | Clause 0 reads the generation the request pinned on entry (r6, r7, §5) and, r8 (Claude r7 nit 3), the current one. A `false` that reached the current catalog before a session's step 3 gives `disabled`, with no mark. A `true` waits for the next request. After step 3 nothing reads the flag: a judge already running finishes, still strict and non-discarding. A repo a reload added and `add` brought in after this request's pin is one the snapshot lacks, so clause 0 answers `disabled` (r7). The strict judge reads `rm.regenerable` only to name the subset (§3.1), never to exempt. | catalog.rs:15; strata-session catalog.rs:84-86; upstream.rs:78; periodic_fetch.rs:91; main_sync.rs:208; reload.rs:129-155 |
| `du` is scanning the session. | `with_du_hold` cancels and waits `HOLD_WAIT` (5 s), else `Busy du_cancel_timeout` → `busy` `{reason: "du_cancel_timeout"}`, no mark (r4: by code, §4; the hook holds its queue place here and still marks nothing — nothing was judged). | handlers.rs:1907-1911; du.rs:50, :726-753 |
| **A `config reload` tightened a pin of one of the session's repos (r4, Claude r3 nit 3).** | The fence trips: at the queue take (`check_session`, locks.rs:547 → :345-353) or inside the judge after Phase B (remove.rs:310-311); either way `Busy pins_tightened` (locks.rs:812-816) → `busy`, no mark; the judge removed nothing (the fence precedes teardown, :313-314). The agent's re-run or the next publish judges again under the new pins. | locks.rs:331-353, :812-816; remove.rs:310-314 |
| **A split-branch repo in the manifest (r4, Codex r3 blocker 1).** | Published in this call → clause 4 `not_merged` on the publish commit (or on the base, when the post-publish rebase moved the session onto it, push.rs:858): mark done, nothing judged, nothing removed; the chain was pushed to `origin/<publish.branch>` as the publish asked. Untouched sibling → clause 4 passes on the base (L's tip or an ancestor), and the judge decides as for any repo. Published in an earlier call → clause 4 `not_merged` on that commit, whatever this call published and whether or not the overlay is empty (r5, §2.1; r4 relied on the strict judge's `no_landed_publish`, and an empty overlay gave it nothing to refuse). Never: a close whose commits are not on `origin/<default_branch>`. | proposal.rs:386-396; new/mod.rs:573, :581-583; remove.rs:980-983, :1008-1010 |
| **An earlier publish of this session has not landed (r5, Codex r4 blocker).** | Not a race, a state, and independent of the trigger. Clause 4 reads every publish row and the `rebase_pending` row of every manifest repo (§2.1). So a deferred `push = false` commit, an earlier release-only commit, a replay's unpushed copy or an `Unknown` publish refuses the close on an own publish of another repo, on set (b) and on `push`, with an empty overlay too: `not_pushed` or `not_merged`, mark done, the row intact, nothing removed. | §2.1; handlers.rs:2408; sessions.rs:682 |
| **A writer changes the session's `rebase_pending` row during the close (r5).** | Main-follow and a replay hold only the repo lock. Before step 5.5 → its proof reads the row: an unlanded target refuses with `changed_during_remove`, `outstanding`, and a landed one is proved. After it → `set_removing_if_unchanged` finds the keys moved: `changed_during_remove`, `pending_changed: true`, nothing retired. T2's deferral keeps the keys (one for `conflicts` refuses, below); T2's move needs the queue place the hook holds, so it defers `busy`. | main_follow.rs:177, :225-261; resolve.rs:861-863; pending.rs:439-453; §3.2 |
| **A stale target writer resumes after `Removing` commits (r6, Codex r5 blocker 1).** | Main-follow read S as Active, or a replay or `reconstruct` read its row, before the commit. The write's own transaction reads the lifecycle: `enroll_main` and `retarget_pending` need `active`; `plan_replay`, `settle_planned` and `promote_pending` refuse `removing` and `removed`. No row appears and no head changes, the writer goes on with its other sessions, and `Removed` deletes only what step 5.5 proved. | rm_state.rs:17; main_follow.rs:169, :177; sessions.rs:682; §3.2 |
| **A deps proposal, an own draft or a `conflicts` deferral appears during the close (r6, Codex r5 blocker 2; r10, Codex r9 blocker).** | `observe_main` and `deps_propose` hold only the repo lock, and T2's gates hold nothing, so clause 6's earlier read does not stop them. If one commits before `Removing`'s transaction, line 3 finds it: `deps_pending` or `conflicts` with `after: "checkpoint"`, mark done, S Active, nothing retired, its target on main or not. **An own draft counts on any repo of S, tracked or not (r9).** **An own draft that comes after is no longer written (r10): its insert reads `removing` or `removed` in its own transaction, the writer answers `InvalidInput lifecycle` or `NoSuchSession`, and S closes with no draft.** A main proposal that comes after is recorded, concerns the sessions that stay and stays listed; a `conflicts` deferral that comes after goes with its row at `Removed`. | proposal.rs:415, :487; handlers.rs:1763-1777; store deps.rs:355-372; rebase.rs:504-512; §3.2 |
| **An own deps draft sits on an untracked sibling (r9, Codex r8 blocker).** | Not a race, a state, and independent of the trigger. `front` is untracked, its overlay is empty and every base is on main, but S holds an `awaiting_publish` proposal in `front`. Clause 6 (b) reads it with no tracked filter: `deps_pending`, `own: true`, `tracked: false`, mark done, S Active, both workspaces and pins in place, the draft still in `deps list`. The agent's `rm` is the way out, because a draft on an untracked repo cannot be published or cancelled (§3.2). | handlers.rs:202-207, :1763-1777; deps.rs:570-573; §3.2 |
| **A pending main proposal sits on an untracked sibling (r9).** | No effect on the close, as in r8: S closes and the proposal stays open for the sessions it concerns. A rollout would skip S's untracked workspace anyway (§11 item 53). | deps.rs:571; rollout.rs:349-351 |
| **A `Pending` proposal whose session has gone is retargeted or replaced (r10).** | Unchanged. `same_to` and `ensure_target` insert a `pending` row that keeps the origin, and `record_proposal` reads the lifecycle for a row with no target only, so main-follow and `deps approve` go on working after the proposer's `rm`. | proposal.rs:258-274; strata-deps rollout.rs:140-161; store deps.rs:364; §3.2 |
| **`sync` saved a conflict report (r9, Claude r8 nit 1).** | The report's only writer holds S's queue place, so clause 6 (c) sees it or S is gone first. S published `p` = v1 and stayed open, T published `p` = v2, S's `sync` answered `Conflict` and left the workspace unmoved. The projection onto S's own publish would discharge `p`, so the next judge used to close S and delete the report. Now: `conflicts`, `report: true`, mark done, S Active. A move clears the report and the next judge decides. | sync_move.rs:434, :488, :615-616; sessions.rs:683, :955, :1016; remove.rs:573-595 |
| **The hook panics, or the daemon is shutting down when it takes the queue (r6, Claude r5 nits 4 and 5).** | The publish stays what it was: `ok: true`, `published` intact. The session's entry is `error` with no mark, and the audit line is written. Without the hook's own `catch_unwind` a panic would have made the reply `Unknown handler_panicked`. | handlers.rs:969, :1050-1062; locks.rs:385-386; §1 |
| **A refused publish was pushed by a human before the agent's re-run (r6, Claude r5 nit 1).** | `already_published[].push` reads `Pushed` with `landed_later: true`, so there is no `PushRefused` and no `push_refused`. The session goes on to clause 7, where a refused publish's unbuilt base can still answer `no_built_base` (§3.1). | handlers.rs:2125, :2422; §6 |
| **Re-judges with unchanged reasons (r6, Claude r5 nit 9).** | Every push of the repo judges a waiting session again. With the same note nothing is written, so `mark.at` stays and §8's once-per-mark reason does not fire again. | marks.rs:99-104; §4 |
| **`mark active` between the enumeration and the queue place (r5, Codex r4 nit 1).** | The reread under the queue place finds no row: the session is not judged, nothing is marked or audited, and it is not in the reply. A `mark active` that arrives after the reread waits for the queue place and then deletes whatever the hook wrote; if the hook closed the session it answers `NoSuchSession`. | §1; marks.rs:89-92; handlers.rs:1126 |
| **Set (b) or an agent's `rm` removes a group session before its own hook runs (r5, Claude r4 nit 3).** | B's row is `Done`. Before B's hook takes B's queue place, another publish that pushed the repo re-judges done-marked B and closes it, or the agent's `rm` removes it. B's hook finds `Removed`: `closed: true`, `by: "other"` in B's reply, `gone` in this judge's audit line. **r10 (Claude r9 nit 3): a row already `Removed` at step 1 takes no queue, so a tightening reload in the same run cannot turn that answer into `busy` (§1).** | §6; §1 step 1; locks.rs:345-353; push.rs:892 |
| **The cost of set (b) and the `push` trigger (r5, Claude r4 nit 7).** | Each done-marked session holding the pushed repo is judged on the reply path, one after another. **A failing clause 4 keeps the judge from running; the other cheap clauses still run, because `reasons` lists them all (§2; r9, own correction of r5's "stops").** The judge is at most the holders scans (about 0.3 s each, open_files.rs:39-41) and the cold scans, for a session that may be refused for a reason the push cannot change. **Each judged session reads `store.publishes()` once under its place, for clauses 4 and 6, and a close once more at step 5.5 (r8, Claude r7 nit 9: r7's `assigned` read it once per manifest repo, rebase.rs:922, uncounted here). It also makes one `ReadMode::Full` upstream read at clause 2 (handlers.rs:965), which the helper reuses (r9), and, at clause 6, one `open_proposals(store, None)` call in total and one `sync_report` read per manifest repo (r9; r10, Claude r9 nit 5: r9's row counted `open_proposals` per repo).** No cap and no narrowing (§11 item 30). Done-marked sessions are few by construction: each ends at its next close or `rm`. | §11 item 30; records.rs:225 |
| **The daemon dies between `Removed` and the audit line (r8, Claude r7 nit 8).** | Step 6 runs after the closure, so `Removed` has committed and the line has not been written. After the restart the close stands with no line in `sessions-audit.jsonl`, and the caller's connection has closed, so it reads `Unknown socket_closed` and then finds `NoSuchSession`. What remains is the `Removed` row and its `session_marks` row until the session's gc, the checkpoints, and the trash directories until trash gc. Not closed: a line written first would record closes that then refuse, and a second fsynced line per close is a write for one crash window in an observer's file. A failed append has the same shape and is logged and reported in `daemon_status` (§7). | §1 step 6; sessions.rs:676-684; git-strata rpc.rs:72-76; gc.rs:284 |
| **Two hooks append to `sessions-audit.jsonl` at once (r4, Codex r3 blocker 2).** | Two publishes of different sessions, or a publish and a push, reach §7 on two request threads (startup.rs:717). `Core.sessions_audit.lock` serialises one line's size check, rotation, open, delimiter, write and fsyncs against the other's; the second writer sees the rotated file. Nothing else is taken under that lock. | audit.rs:95-128; §7 |
| The publish row of this group is `Unknown` (push timed out, :877-891). | Not reached: the handler returns the error (:2127-2130) and the hook runs only on the success path; the next re-run reconciles the row (`unknown_row_is_reconciled_when_the_remote_has_the_commit`, crates/strata-publish/tests/push.rs:787) and judges then. A later publish of **another** repo does reach a hook while this row is still `Unknown` (`refuse_open` is per repo, :362-372): clause 4 reads the row and refuses until its commit is on L (r5, §2.1). | strata-publish push.rs:877-915 |

## 10. Tests

| # | Where | Test |
|---|---|---|
| 1 | crates/strata-daemon/tests/upstream.rs (the `call`/`ok`/`new`/`advance` helpers, :15-73, and the publish fixture of :422-441). **The fixture and its default (r3, Claude r2 nit 5, audited).** `Dfx` builds a typed `Config` (tests/support/mod.rs:377) with one repo `publish: PublishTarget { branch: "main", push: true, confirm: false }` (:299-303), `pin_unpushed: true` (:305) and `rebase.quiet_secs: 0` (:310, "a publish moves its own session at once"), and `write_config` serialises it to `config.toml` (:398-401; `Dfx::with` calls it at :394). The default becomes `publish: PublishDefaults { auto_close: false }` in the :377 literal (import at :344-346; `auto_close: None` in the :299-303 literal once `PublishTarget` has the field), which `toml::to_string` writes as `[publish] auto_close = false` (not the default, so not skipped). A test opts in by mutating `fx.config` and calling `write_config()` **before** `prepare()`/`prepare_with(c)`, the pattern of daemon.rs:535-539 (`seed_pins`, :417-421, reads the file back through `Config::parse` first). Why the default is `false`: six existing tests publish a fully landed session and then use it, and would fail under a default-`true` fixture: `confirmed_publish_is_bound_to_the_shown_chain_top` (daemon.rs:558 closes; the next `external(&root, …)` at :547, via :565, has no workspace), `publish_rerun_of_a_directory_selector_commits_new_edits` (:1013 closes; `external` :1017), `publish_rerun_tolerates_a_pruned_landed_commit` (:1052 closes; `store.record_publish` :1057 → `NoSuchSession`, strata-store records.rs:129-132), `publish_rerun_of_a_partly_changed_batch_commits_the_changed_path` (:1086 closes; `external` :1089), `rm_waits_for_publishing_in_the_lock_free_gap` (:1244 closes; the `rm` of :1247 joined at :1254 → `NoSuchSession`, racing the close), and the unit test `unpublished_false_with_empty_overlay_and_checkpoint_on_old_base` (tests/unit/upstream_tracker.rs:219-233, included at handlers.rs:238-240; it publishes through `Server::handle` and then unwraps the tracker map at :243-246, which `watch_tracked` fills for Active sessions only, handlers.rs:405-412). Tests that stay open under `true` do so for a reason the design names and a change there would silently flip: daemon.rs:946 (`Waiting` on a deps proposal, clause 6), upstream.rs:504 (`fail_for_test` → clause 2), upstream.rs:560 and daemon.rs:1778 (an edited path outside the group), reload.rs:488 and :870 (`push = false`). `quiet_secs: 0` means the post-publish rebase moves at once; a deferred variant of a test in this row sets `rebase.quiet_secs` high in `fx.config` before `write_config` (or plants a holder) so the overlay keeps the published entries and the projection is what the judge uses. | `publish_closes_a_fully_landed_session`: edit, checkpoint, `publish` → `closed: true`, `auto_close.sessions[s].closed`, `rm.checkpoints.tool` present, `rm.regenerable` absent, `upstream[s].as_of_rm == "before"` and `unpublished == false`, the store row `Removed`, the pin gone, the root in the trash, no `session_marks` row written; `publish_marks_done_when_a_sibling_repo_has_unpublished_edits` (a two-repo session, edit in both, publish one → `closed: false`, `reasons == ["unpublished"]`, `detail.repos` names the other repo, the mark row has `by_kind: "daemon"` and the note's first line, the session still Active); `publish_marks_done_when_only_regenerable_edits_remain` (Codex r1 blocker 2: `rm = { regenerable = ["gen/**"] }` in the fixture config before `prepare`, so the pin has it; edit `gen/a.json` and `src/index.ts`, publish `src/index.ts` only → `closed: false`, `reasons == ["unpublished"]`, `detail.repos.tool.paths == ["gen/a.json"]`, `detail.regenerable.tool == ["gen/a.json"]`, the session Active with the daemon's note naming `(rm.regenerable)`; then an agent `rm {discard:false}` succeeds and its report lists `regenerable.tool == ["gen/a.json"]` as today, tests/rm_regenerable.rs:38-59); **`publish_marks_done_not_pushed_for_an_untracked_sibling_on_an_unpushed_chain`** (r3, Codex r2 blocker 1: two repos, `tool` `push: true`, `front` `push: false`, `pin_unpushed` at its default `true`; session A = `new {repos: ["front"]}` edits, checkpoints and publishes `front`, so `front`'s chain leads origin unpushed and A is marked `not_pushed`; session B = `new {repos: ["tool"]}` with `only` false (rpc.rs:94-97), so `front` joins B untracked with `head` = that chain top (new/mod.rs:148-153, :573); B edits `tool`, checkpoints, publishes → `closed: false`, `auto_close.sessions[B].reasons == ["not_pushed"]`, `detail.repo == "front"`, `detail.tracked == false`, `detail.head` = the chain top, B Active with a `daemon` mark, `refs/strata/pins/<B>` still present in `front` (r2 closed B and deleted the pin: the assertion the fix flips); then `push front` → the push reply closes **both** A and B, both rows Removed, both pins gone); **`publish_rejudges_done_marked_sessions_when_it_pushes_their_repo`** (r3, Claude r2 nit 4; r5 setup, Codex r4 nit 2: a pre-receive hook that exits 1 for session A's publish of `tool` → the refused first run takes the error path (:2127-2130) and writes **no mark** (assert `ok: false`, `store.mark(A)` is `None`, no audit line), the row closed `Done` with `stopped: "PushRefused"`; A re-runs the same publish → `already_published[0].push.kind == "Refused"`, the success path, `reasons == ["not_pushed", "push_refused"]`, a `daemon` mark, the chain unpushed; hook removed; session B, created now so that its `new` builds the chain top's base (base.rs:59-63), which A's projection needs, publishes `tool` → `published[0].push.kind == "Pushed"`, `auto_close.sessions[A].closed == true` with `in_group: false`, `sessions[B].closed == true`, both Removed; variant: B created before A's first publish → `sessions[A].reasons == ["unpublished"]`, `landing.tool.why_none == "no_built_base"` (§3.1), the note names `git-strata rm --session S`, and that `rm` passes by D57); **`publish_marks_done_when_a_revert_matches_an_older_publish`** (r3, Codex r2 blocker 3, end to end; r6, Codex r5 nit 1: with auto-close on, the first fully landed publish would remove S, so the setup runs under the fixture's `auto_close = false` and the flag is turned on by a reload before the last publish; the test lives in tests/reload.rs for its `call` and `serving`, :16-41: publish `src/p.ts` v1, `sync` onto it, publish v2, `sync` onto it, write v1 back, `fx.config.publish.auto_close = true`, `fx.write_config()`, `config_reload` (:109), publish `src/q.ts` → `closed: false`, `reasons == ["unpublished"]`, `detail.repos.tool.paths == ["src/p.ts"]`, `detail.landing.tool.historical == ["src/p.ts"]`, the session Active; then an agent `rm {discard:false}` passes as today and its BeforeRemove checkpoint holds `src/p.ts`); `publish_marks_done_not_pushed_when_push_is_false` (`push = false` in the fixture config → `not_pushed` with the repo and ref; then `push` of that repo → the push reply has `closed: true` and the session is Removed); `push_judges_only_done_marked_sessions` (two sessions on a `push = false` repo, one marked done by an agent, one not → only the marked one closes); `publish_marks_done_open_files` (a child process with its cwd in the workspace, as `rm`'s OpenFiles tests do → `open_files`, `detail.pids`, nothing removed, `last_rm.detail.by == "auto_close"`); `publish_marks_done_conflicts_and_deps_pending` (the fixture of `deps_and_auto_fields_follow_store_and_cached_summary`, :610 → `deps_pending` with the proposal id; a workspace row with `conflicts` → `conflicts`); `publish_keeps_an_agent_mark_note` (agent `mark done --note x` first; the hook's refusal leaves `note == "x"` and `by_kind == "agent"`, `mark.skipped == "kept_agent"`); **`publish_refreshes_the_daemons_own_note`** (r3, Claude r2 nit 3: a first refusal `unpublished`, then the edit published and a holder planted → the second refusal is `open_files` and the row's note is the second one, `by_kind` still `daemon`); `auto_close_disabled_judges_nothing` (`[publish] auto_close = false`, then per-repo `true` on one repo of two → `reasons == ["disabled"]`, no mark, no audit line; per-repo `false` under global `true` the same); **`publish_busy_when_another_publish_is_running`** (a `Running` row planted for the session → `reasons == ["busy"]`, **no mark row** (r3), `mark.skipped == "busy"`, an audit line with `closed: false`); `multi_session_group_closes_each_session_independently` (the :444-465 fixture with one session dirty → `closed: false`, `sessions[a].closed`, `sessions[b].reasons == ["unpublished"]`); `publish_error_path_judges_nothing` (a `CheckFailed` fixture → no mark, no audit line); `already_published_refused_push_marks_push_refused` (Claude r1 note: a `Done` row with `stopped: PushRefused`, push.rs:416-430, re-run → `already_published[0].push.kind == "Refused"`, `reasons == ["not_pushed", "push_refused"]` (r5: clause 4 evaluates first, §2), `detail.stopped == "PushRefused"`, the session open). **r4 rows.** **`publish_marks_done_not_merged_for_a_split_branch_repo`** (Codex r3 blocker 1, the deferred-rebase regression): on the sandbox, branch `release` from main's tip and push it (`fx.sb.git(&main, &["checkout", "-q", "-b", "release"])`, `fx.sb.commit`, push, back to `main`: the shape of strata-publish tests/chain.rs:201-205; `Dfx.sb`, tests/support/mod.rs:349-352, `sb.git` :143, `sb.commit` :161, `fx.main` :538); `fx.config.repos[tool].publish.branch = "release"` with `deps_dirs`, `install` and `install_inputs` empty on that repo (config.rs:578 forbids the split otherwise), `rebase.quiet_secs` high so the post-publish rebase defers on `recent_writes`, `auto_close` on, `write_config` before `prepare`; the session edits, checkpoints, publishes → `published[0].push.kind == "Pushed"` (release pushed), the rebase report `Deferred`, `upstream[s].repos.tool.ahead == 0` (the r3 hole's precondition), **`closed: false`, `sessions[s].reasons == ["not_merged"]`, `detail.what == "publish"`, `detail.commit` = the publish commit, `detail.publish_branch == "release"`, `detail.default_branch == "main"`, `detail.ref == "refs/strata/remotes/origin/main"`**, a `daemon` mark whose note names `origin/release`, the session Active, the pin present, `refs/strata/remotes/origin/release` at the publish commit and `refs/strata/remotes/origin/main` unchanged (r3 closed this session: the assertion the fix flips). Variant (ii), `quiet_secs: 0` → `Moved` onto the release commit → `reasons == ["not_merged"]` with `detail.what == "base"` and `detail.head` = that commit (clause 4 before clause 3: `ahead` is not the first reason). Variant (iii), the sibling: two repos, `tool` publishing `main` with `push: true`, `front` split to `release`, untouched; the session publishes `tool` → `closed: true`, both pins gone (an untouched split-branch sibling does not hold the session open). Variant (iv): after (i), merge `release` into `main` on the remote (`fx.sb.git`), fetch `main` on the daemon's handle (`core.gits[&tool].fetch("origin", "main")`), then the agent's `rm {discard: false}` passes as today (D57 against `origin/release`) — the close of a split-branch session is the agent's. **`publish_rejudges_sessions_whose_repo_an_earlier_run_pushed`** (Claude r3 nit 2; r5 fixture, Codex r4 nit 2: repos run in sorted order, handlers.rs:2046, :2074, so the repo that pushes must sort before the one that fails, and r4's `push: false` repo could not have pushed at all): `Dfx::with(&["front", "tool"])`, both `push: true`, `tool` with a check that fails until a file exists (`checks`, the `headless_push_replays_a_diverged_chain_then_pushes` shape, tests/resolve.rs:1039). A pre-receive hook refuses session A's publish of `front`; A re-runs → `not_pushed`, `push_refused`, a `daemon` mark; hook removed. Session B, created now so that its `new` builds the chain top's base, has a group of `front` and `tool`: B's first run publishes and pushes `front`, whose push lands A's commit too, then fails on `tool` with `CheckFailed`: error path (no hook), A still Active, its mark unchanged by this run. B adds the file, re-runs → `already_published[0].repo == "front"` with `push.kind == "Pushed"`, `auto_close.rejudged == [A]`, `sessions[A].closed == true` with `in_group: false`, both Removed; `closed` reports B's group only. **`closed_reports_the_group_only`** (Codex r3 nit 2): the `publish_rejudges_done_marked_sessions_when_it_pushes_their_repo` scene with a cwd holder planted in A → A stays open (`open_files`), B closes → **`closed: true`** (B's group), `auto_close.reasons == []`, `sessions[A].closed == false` with `in_group: false` and `reasons == ["open_files"]`, `rejudged == [A]`. **`publish_busy_by_code_marks_nothing`** (Claude r3 nits 3 and 6): (a) a seam `auto_close.before_judge` pauses the hook after the publish's last lock release; a `config reload` that tightens `confirm` on the repo runs at the pause; release → `reasons == ["busy"]`, `detail.reason == "pins_tightened"`, no mark row; (b) a du scan held on the session with its cancel token ignored (the du.rs:1165-1166 shape) → `detail.reason == "du_cancel_timeout"`, no mark row; both: an audit line with `closed: false`, the session Active, the publish row `Done`. **`publish_marks_done_open_files_for_a_deferred_session`** (Codex r3 nit 3): a holder with its cwd in the workspace during the publish → the rebase is `Deferred open_files`, the hook `open_files`; the reply's fresh `upstream[s].repos.tool` has `behind >= 1` and `unpublished == true` (r5, Claude r4 nit 6: the helper dropped the Tracker, so `Live::default()` reads `true`; the published entries are in the overlay as well, so the assertion holds either way), and `block_reason(&upstream, true)` over that JSON (row 10) returns the "main moved" sentence with neither "cd back" nor "publish" (r6, Claude r5 nit 3), not `None`; the holder leaves, `sync` → `Moved`, `behind == 0`, `block_reason` is `None`, the session still Active and marked done. **`publish_marks_done_no_built_base_names_rm`** (Claude r3 nit 4): a done-marked `Deferred` session whose target base was gc'd (the `rm_t3_without_a_built_base_stays_dirty_and_builds_none` shape, built base removed), re-judged by another session's publish that pushes the repo → `unpublished`, `detail.landing.tool.why_none == "no_built_base"`, the note's line names `git-strata rm --session S`, not "publish". **`push_of_a_split_branch_repo_runs_no_hook`**: see row 2. |
| 2 | crates/strata-daemon/tests/daemon.rs (the socket-level `call(&sock, "push", {repo})` shape, :1301, :1332, :1352) or upstream.rs, with the scripted confirmer (`fx.confirmer(vec![Verdict::pass()])`, tests/support/mod.rs:477-483; `prepare_with`, :506-518) | `push_confirmed_form_closes_done_marked_sessions_outside_the_repo_lock` (both r1 blockers 1): a pin `confirm: true, push: false` (the shape of confirm_flow.rs:91, via its `set_flags`), a session published (the hook leaves it `not_pushed`, marked done by the daemon), then `push {repo, shown: <chain top>}` → the call **returns** (the harness timeout is the deadlock detector: r1's placement hangs here with the row `Removing`), `push.kind == "Pushed"`, `closed: true`, `auto_close.sessions[s].closed`, the session Removed, `refs/strata/pins/<sid>` gone, `log.shown().len() == 1`; `push_headless_form_closes_done_marked_sessions` (`confirm: false, push: false`: the :2271 path, after `main_sync::reconcile`); `push_refused_judges_nothing` (a pre-receive hook that exits 1, as :1298-1303 → `ok:false`, no mark, no audit line). **`push_of_a_split_branch_repo_runs_no_hook`** (r4): a repo publishing `release` with `push: false`, a session published to it and marked done `not_merged` by the publish; `push tool` → `push.kind == "Pushed"`, the reply has neither `closed` nor `auto_close`, the session Active, its mark and note unchanged, no audit line. **`push_closed_ignores_disabled_sessions`** (r5, Claude r4 nit 8): two done-marked sessions on a `push: false` repo, one of them with a second repo at `auto_close = false` → `push` closes the other: `closed: true`, `rejudged` lists the closed one only, `sessions[<disabled>].reasons == ["disabled"]`, no audit line for it. |
| 3 | crates/strata-daemon/src/auto_close.rs (static, in `tests/rpc.rs` next to `confirm_is_not_channel_auth`, :498-505) | `auto_close_never_discards`: the module's source contains none of `RemoveCall::Agent`, `discard`, `RemoveDiscard`, `require_validated`, `ConfirmToken`, `remove(`; the one `remove_session(` call passes `RemoveCall::AutoClose`. **`landed_clauses_cover_every_manifest_repo`** (r3, Codex r2 blocker 1, unit): the clause-4 function over a manifest with a tracked and an untracked repo and a fake `is_ancestor` that fails for the untracked one → `not_pushed {repo: untracked, tracked: false}`; with both on origin → passes. **`landed_clauses_anchor_on_the_default_branch`** (r4, Codex r3 blocker 1, unit): the clause-4 function over a `RepoConfig` with `publish.branch = "release"`, `default_branch = "main"` and a fake `is_ancestor` that answers `true` for `refs/strata/remotes/origin/release` and `false` for `…/main` → `not_merged {what: "publish", publish_branch: "release", default_branch: "main", ref: "refs/strata/remotes/origin/main"}` for a published commit and `{what: "base"}` for the base; the same fake with `publish.branch = "main"` → `not_pushed`; `true` for `…/main` → passes; the fake panics when asked about `origin/<publish.branch>` while the branches differ, so the function provably never consults it. `reasons_follow_the_evaluation_order`: a manifest failing clauses 3 and 4 lists `not_merged` before `ahead`. **r5 units of `outstanding` with a fake `Ancestry` (§2.1):** `outstanding_reads_every_publish_row_of_the_pair` (rows of an earlier call, nothing from a reply: `what: "publish"`); `outstanding_ignores_another_sessions_row` (Claude r4 nit 2); `outstanding_counts_every_pending_source` (`head` off L with `source` `publish`, `replay`, `main` → `what: "pending"`; on L → passes); `outstanding_fails_closed_on_planned_columns` (`planned_head` off L; `planned_publish` naming a `Running` or missing row); `outstanding_fails_closed_on_an_unknown_row_without_a_commit`; `outstanding_skips_a_done_row_with_a_null_commit`; `outstanding_folds_the_pending_row_into_its_publish_item`; `outstanding_has_no_cap` (40 rows, only the oldest off L → one item); `on_chain_follows_the_chain_state` (one case per line of §2.1's table, `rewound` included). **r6:** `outstanding_stands_on_a_pruned_commit` (Claude r5 nit 6: the real `Ancestry` over a `Git` whose publish commit is gone, `is_ancestor` and `merge_base` answering `NoSuchCommit`, gitio refs.rs:96-117 → an item with `what: "publish"` and `on_chain: false`, not `Err`; a fake answering any other code → `Err`, reported as `error {code}`); `reopened_items_report_their_own_codes` (a `Dirty changed_during_remove` with `reopened: [{code: "deps_pending"}]` → `reasons == ["deps_pending"]`, `detail.after == "checkpoint"`, and with `pending_changed` and no `reopened` → `["changed_during_remove"]`). |
| 4 | crates/strata-daemon/src/audit.rs unit tests (:130-166; the `rotates_reads_both_files_and_ignores_corrupt_lines` shape pads the file to `ROTATE_BYTES`, :148-150) and crates/strata-daemon/tests/daemon.rs (`std::thread::scope` as :1243-1255) | `sessions_audit_is_a_separate_file_and_rotates_independently`; `auto_close_line_shape` (the §7 keys; `auth == "none"`; `rm.mode == "strict"`; no note text; `trigger.in_group`); `pins_loader_ignores_the_sessions_file`; `busy_writes_an_audit_line_and_no_mark`. **`audit_file_concurrent_appends_lose_nothing_across_a_rotation`** (r4, Codex r3 blocker 2, unit): an `AuditFile` with `rotate_bytes = 6 KiB`; 8 threads append 10 lines each (about 120 bytes, `{"thread": t, "seq": n}`), so exactly one rotation happens somewhere in the burst → the main file plus `.1` hold 80 lines, every line parses, every `(thread, seq)` pair appears once, `.1` is non-empty, within each thread every line in `.1` precedes every line in the main file, both files are 0600. A `#[cfg(test)]` seam `audit.after_rotate` parks the rotating thread between the `rename` and the `open`: with the lock the other threads block until it finishes (asserted: the main file is absent or empty while it is parked, and no other thread's line is in `.1`); the same test without the lock loses the parked thread's predecessors or doubles the rotation — the assertion the fix makes deterministic. **`two_sessions_hooks_append_concurrently`** (daemon-level): the sessions file padded to `ROTATE_BYTES - 100` as :148-150 does, two sessions in two groups publish on two threads with `auto_close` on → both `auto_close` lines present across the two files, each session once, both parse. The existing `rotates_reads_both_files_and_ignores_corrupt_lines` stays green: the pins file keeps `Pins.inner` as its lock (confirm_flow.rs:257-262). **`audit_file_survives_a_poisoned_lock`** (r5, Claude r4 nit 4, unit): a thread panics while holding `AuditFile.lock`; the next `append_line` writes its line. **`audit_append_holds_no_queue`** (r6, Codex r5 nit 2, daemon-level; `audit.after_rotate` moves from `#[cfg(test)]` to the `test-seams` feature so a daemon test can arm it): S is left open by a second unpublished edit, the sessions file is padded as above, and the seam parks the hook's append; `mark {session: S, state: "active"}` succeeds at the pause, so the queue place was released first. |
| 5 | crates/strata-store/tests/migrate_v7.rs (:118-121), marks.rs | `by_kind` accepts `'daemon'` and rejects anything else (the :118-121 probe flips to accepted); `set_mark(…, Daemon, …)` round-trips through `mark()`; the v7 migration's `sqlite_schema` text for `session_marks` names the four kinds. A store at the old text is refused at open with `Corrupt reason: "schema"` (r7, Claude r6 nit 3): `open_refuses_schema_drift` (crates/strata-store/tests/open.rs:212) covers that check, so no new test. |
| 6 | crates/strata-session/tests/rm_regenerable.rs (the `set` helper :33-35 and the fixture of :37-72) and **remove_landed.rs (the T3 helpers `t3_session` :748 and `t3_publish(fx, s, rep, writes, deletes, fetch, build)` :758; `mv` from tests/remove.rs)** | `remove_strict_counts_regenerable_as_unpublished`: the :39-48 setup (`gen/a.json` Added, `src/index.ts` Modified, both regenerable) → `remove_strict` is `Dirty` with `repos.tool.paths == ["gen/a.json", "src/index.ts"]` and `detail.regenerable.tool` the same two, the session Active, no BeforeRemove checkpoint written; then `remove(id, false, None)` passes as :49-59 asserts. `remove_strict_projects_regenerable_entries` (a regenerable entry whose content the newest publish holds is discharged by the projection like any other; the non-strict path still lets it go). `remove_strict_recheck_has_no_regenerable_exemption` (the `Meddling` double of remove_landed.rs:365-373 writes a regenerable path between step 4 and the checkpoint → `changed_during_remove after: dirty_check`; the same under `remove(false)` passes). **`remove_strict_refuses_a_fresh_revert_a_historical_publish_matches`** (r3, Codex r2 blocker 3: `t3_session`; write `src/p.ts` v1, `t3_publish(…, fetch: true, build: true)` → C1, `mv` onto it; v2, publish → C2, `mv` onto it; write v1 back; publish `src/q.ts` → C3; two variants: (i) `mv` onto C3 → `remove_strict` is `Dirty`, `repos.tool.paths == ["src/p.ts"]`, `landing.tool.why_none == "no_newer_publish"`, `landing.tool.historical == ["src/p.ts"]`; (ii) no move → `Dirty`, `landing.tool.target.commit == C3`, `landed.tool.rebased == 1` (`src/q.ts`), `repos.tool.paths == ["src/p.ts"]`; both: the session Active; then `remove(id, false, None)` passes today by D57 and its BeforeRemove checkpoint holds `src/p.ts`: the assertion that pins the agent's `rm` as unchanged, §11 item 7). **`remove_strict_discharges_only_by_projection`** (r3: the deferred shape, base B, one publish C with `fetch: true, build: true`, no move, overlay = exactly the published entries → `remove_strict` passes, `checkpoints.tool` present, `regenerable` empty; with `build: false` → `Dirty unpublished`, `landing.tool.why_none == "no_built_base"`, while `remove(false)` passes by D57; `rm_t3_without_a_built_base_stays_dirty_and_builds_none`, :830-840, stays green and shows that neither mode builds one). **`remove_strict_recheck_has_no_historical_discharge`** (r3: after C1 and C2 as above with the workspace on C2, a `Meddling` hook writes `src/p.ts` = v1 between step 4 and the checkpoint → `remove_strict` is `Dirty changed_during_remove after: dirty_check` with `repos.tool.paths == ["src/p.ts"]`; `remove(false)` passes through `recheck`'s D57, :1134-1140). **`remove_strict_anchors_candidates_on_the_default_branch`** (r4, Codex r3 blocker 1): a `t3_publish` variant `t3_publish_on(fx, s, rep, "release", writes, deletes, fetch, build)` that lands the publish on `release` on the sandbox remote and fetches `release` (so `refs/strata/remotes/origin/release` moves and `…/main` does not), with `fx.cfg_mut("tool").publish.branch = "release"` and `deps_dirs` cleared; the session's overlay = exactly the published entries, `fetch: true, build: true` → `remove(id, false, None)` passes today (D57 against `origin/release`: the control), **`remove_strict` is `Dirty unpublished`** with `landing.tool.why_none == "no_landed_publish"`, `landing.tool.anchor == "refs/strata/remotes/origin/main"`, `repos.tool.paths` = the published paths, the session Active and no checkpoint written; then merge `release` into `main` on the remote and fetch `main` → `remove_strict` passes (C descends from B); variant: `release` branched before B → `remove_strict` stays `Dirty` with `why_none == "no_newer_publish"` while `remove(false)` passes (the documented false negative, never a false close). **`remove_strict_recheck_anchors_on_the_default_branch`**: the `Meddling` shape of `remove_strict_recheck_has_no_historical_discharge` on the split repo → `changed_during_remove`; `remove(false)` passes through `recheck`'s D57 against `origin/release`. **`remove_strict_historical_covers_remaining_entries_only`** (r4, Claude r3 nit 9): a refusal with one discharged path and one unpublished path whose content an older publish holds → `landing.tool.historical` names the unpublished path only (the content match ran over the remainder); a refusal where nothing remains after the projection has no `historical` key. |
| 7 | crates/strata-session/tests/remove.rs and remove_landed.rs (Codex r1 blocker 3 and Codex r2 blocker 2; the two-repo fixture `Fx::with(&[…])` as remove_landed.rs:820; `Meddling` :365-373; `SessionSeam::PauseAt` + `Worker::spawn("cwd:")` as tests/remove.rs:817-838; the kept-old-tree fixture of tests/remove.rs:325-348: a `Worker::spawn("cwd:")` holder in `src`, `mv` under `MoveCwd { allow_cwd_holders: true }`, `old_tree` kept, the holder dropped) | `rm_refuses_a_write_finished_after_the_checkpoint_of_another_repo`: a `Meddling` hook that, when called for `tool` (second in `BTreeMap` order), writes and closes `src/late.ts` in `front`'s workspace (already checkpointed) → `Dirty changed_during_remove`, `detail.after == "checkpoint"`, `repos.front.paths == ["src/late.ts"]`, the session Active, the file on disk; a second `remove` passes and its `front` checkpoint holds `src/late.ts` (today: the first remove passes and the file exists only under the trash: the assertion the fix flips). `rm_refuses_a_write_finished_before_the_first_holder_scan`: `PauseAt "remove.before_removing"` (:304), at the pause write and close a file, release → the same refusal. `rm_refuses_a_deletion_after_the_checkpoint` (delete a base file at the pause → an entry `Deleted` appears → refused). `rm_refuses_an_untracked_repo_write_after_dirty_check` (an untracked repo of the session, written at the pause → refused with `repos.<untracked>`). **`rm_refuses_a_write_into_a_kept_old_tree_before_the_first_holder_scan`** (r3, Codex r2 blocker 2: the kept-tree fixture, then `PauseAt "remove.before_removing"`; at the pause write and close `<old>/src/late.ts`, release → `Dirty changed_during_remove`, `detail.after == "checkpoint"`, `detail.old_trees[0].path == old`, `detail.repos` empty, the session Active, the file still at `<old>/src/late.ts`; today the remove passes and the file exists only under the trash: the assertion the fix flips; a second `remove` is `Dirty kept_trees` from Phase A, as tests/remove.rs:338-345 asserts for a write before the `rm`). **`rm_refuses_a_kept_old_tree_it_cannot_compare_during_the_recheck`** (r3: at the pause make the old tree's `src` unreadable (`chmod 0`), release → refused (`unwrap_or(false)` fails closed); restore the mode in the test's cleanup). **`rm_refuses_a_deletion_in_a_kept_old_tree_after_phase_a`** (delete `<old>/src/index.ts` at the pause → refused). **r7 (Claude r6 nit 6): `remove_strict` refuses all three scenes earlier, in Phase A, so their pauses are never reached; r6's `remove_strict_*` variants are replaced by `remove_strict_refuses_an_unchanged_kept_old_tree`** (the kept-tree fixture, nothing written → `Dirty`, `old_trees[0].changed == false`, `old_trees[0].path == old`, the session Active, no BeforeRemove checkpoint; the control `remove(id, false, None)` passes, today's rule) **and `remove_strict_names_a_changed_kept_old_tree`** (`<old>/src/late.ts` written before the call → `old_trees[0].changed == true`). **`rm_refuses_a_holder_that_appears_during_the_recheck`**: `PauseAt "remove.after_recheck"`, spawn the cwd `Worker` (in the workspace, and in a second run inside the kept old tree), release → `OpenFiles` (proves step 5.6 exists and covers old trees). `rm_discard_is_unchanged_by_the_recheck` (the same `Meddling` write and the same old-tree write under `remove(id, true, token)` → removed, `discarded: true`; documents §11 item 9). `remove_rechecks_the_before_remove_checkpoint` (:376-401) stays green and gains `detail.after == "dirty_check"`. `rm_refuses_open_files_found_just_before_removing` (tests/remove.rs:817-838) stays green. **`rm_refuses_an_untracked_repo_that_became_unhealthy_during_the_remove`** (r4, Claude r3 nit 5): an untracked repo of the session whose workspace directory is renamed away at `remove.before_removing` → step 5.5 (a) reads `unhealthy()` itself → `Some(why)` → `Dirty changed_during_remove` with `repos.<untracked>.workspace`; today the Phase B loop skips the repo (:192-194) and the remove passes: the assertion the fix flips. |
| 8 | crates/strata-daemon/tests/upstream.rs | `unpublished_cold_reads_true` (:267) stays green and is cited by name in `publish_closes_a_fully_landed_session`'s comment: the close succeeds while the Tracker is cold, because the judge reconciles from disk. |
| 9 | crates/git-strata/tests/publish_from.rs or cli.rs (the fake-daemon shape of `publish_from_*`, :19-75) | `publish_envelope_carries_closed_and_auto_close` (ok reply); **`refused_already_published_push_is_ok_false`** (r4, Codex r3 nit 1: a fake-daemon reply with `published: []` and `already_published: [{repo, push: {kind: "Refused", message}}]` → `ok: false`, `error.code == "PushRefused"`, `error.detail.fix == "git-strata push tool"` (r5, Claude r4 nit 9), `data.closed == false`, `data.auto_close.reasons == ["not_pushed", "push_refused"]`, `data.already_published` intact; today the envelope is `ok: true`: the assertion the change flips); **`already_published_landed_later_is_ok`** (r6, Claude r5 nit 1: `push: {kind: "Pushed"}` with `landed_later: true` → `ok: true`, the flag carried in `data.already_published`); `push_envelope_carries_closed`; `envelope_carries_rejudged_sessions` (`auto_close.rejudged` and a `sessions[<sid>].in_group == false` entry survive the rebuild). **`live_product_default_closes_on_publish`** (r7, Claude r6 nit 9; cli.rs with `Live`, the shape of :1001-1005): the fixture's `[publish]` table removed from the text (so the real default, table absent), `\npush = false` replaced by `\npush = true`; `lv.serve`: a session writes and publishes → `ok: true`, `data.closed == true`, `data.auto_close.sessions[s].closed == true`, and `status --session s` answers `NoSuchSession`. It is the one CLI test of the default path through the real TOML parse, daemon and envelope; every other `Live` test keeps the fixture's `auto_close = false`. |
| 10 | crates/git-strata/tests/sync.rs (`block_reason` is pure, sync.rs:128; r6: it gains `done: bool`, and the closure of tests/sync.rs:286 passes `false`) | `hook_silent_for_a_moved_session_with_cold_tracker` (`behind: 0`, `deps: None`, `unpublished: None` → `None`); **`hook_names_main_moved_for_a_deferred_session_left_open`** (r4, Codex r3 nit 3: `following: true`, `behind: 1`, `unpublished: Some(true)`, `done: true` → `Some(..)` containing "main moved 1 commits since your base" and the `sync` command, :173-185; the same with `unpublished: None` → the same sentence, :162's default); **`hook_for_a_done_session_never_says_cd_back_or_publish`** (r6, Claude r5 nit 3: that case and an escalated `auto` on a following repo, `done: true` → neither "cd back" nor "publish"; both with `done: false` → today's sentences, the control; end to end, a fake daemon whose reply has `mark: {state: "done"}` and one with no `mark`, the shape of tests/sync.rs:320-336); **`hook_silent_for_a_split_branch_repo`** (`following: false`, `behind: 3`, `unpublished: Some(true)` → `None`, :160; r6: with an escalated `auto` → `Some(..)` naming "non-following" and no `git-strata sync`, :147, whichever `done` is); with §9 item 2: `hook_words_daemon_mark_by_reason` (one case per code of §8, `regenerable`, `historical`, `no_built_base`, `no_landed_publish`, `changed_during_remove`, `not_pushed`'s new sentence, `not_merged`, `busy`, and r7's `kept_trees` by `changed`, `embedded_repo` and `assigned` included). |
| 11 | crates/strata-daemon/tests/rpc.rs | `features_lists_publish_auto_close`; `PublishTarget` with `auto_close` parses and an unknown sibling key still fails (`deny_unknown_fields`). |
| 12 | crates/strata-types (config.rs tests) | `[publish] auto_close` defaults (r3, Codex r2 nit 1: a config **without** the table reads `true`; an **empty** `[publish]` table reads `true`; `[publish] auto_close = false` reads `false` and round-trips through `to_string` → parse as `false`; the default serialises to no `[publish]` table); per-repo override wins over either global value; config.md Appendix A still parses; `[publish] auto_clse = true` is refused (`deny_unknown_fields`, r4, Claude r3 nit 10), as is `[publish] auto_close = "yes"`; `PublishDefaults::default().auto_close == true` (r5, Claude r4 nit 5: the hand-written `Default`). **`auto_close_follows_a_config_reload`** (r6, Claude r5 nit 10; crates/strata-daemon/tests/reload.rs, `call` and `serving`, :16-41): `[publish] auto_close = false` → a publish answers `disabled`; `fx.config.publish.auto_close = true`, `write_config`, `config_reload` → `ok: true`, nothing in the restart list (reload.rs:129-155), and the next publish closes; the same flip back to `false` on a second session → `disabled`, which fails if the hook read `Core.config`. |
| 13 | apps/desktop (lib/view.test.ts:59-73's `marked()` fixture) | a `by.kind: "daemon"` mark renders `done` with its note (the union of types.ts:156 gains `'daemon'`). |
| 14 | skills / docs | `help_topics.rs` (crates/git-strata/tests/help_topics.rs) sees the new DATA line; the skill's `rm --session` sentences at SKILL.md:98-99 and publish.md:137 are replaced, checked by the existing wording tests of the skill if any, else by eye. **`note_commands_parse`** (r7, Codex r6 nit 2; crates/git-strata/tests/help_topics.rs, which already imports `git_strata::args::Cli`, :3; `Cli::try_parse_from`, as lib.rs:59): every command the §2.1, §4 and §8 sentences name parses: `chain --repo REPO --accept-origin replay`, `push REPO`, `rm --session S`, `sync --session S`, `mark --session S active`, `assign --file F --session S`; and `chain REPO --accept-origin replay`, r6's sentence, is refused (the control). |
| 15 | crates/strata-daemon/tests/upstream.rs, r5, the blocker's regressions. **The fixture:** `Dfx::with(&["front", "tool"])` (support/mod.rs:369-377), `auto_close` on, `front`'s `rebase.quiet_secs` high so its publish defers on `recent_writes` (:308-310), `write_config` before `prepare`. S = `new {repos: ["front", "tool"]}`. T, the unrelated session, = `new {repos: ["tool"], only: true}` (rpc.rs:95-97), so `front` is not T's. "Restore" = `external(&ws(&core, &S, "front"), "rm new.txt")`, leaving `front`'s overlay empty on base B. `push: false` scenes set it on `front`; split-branch scenes branch `release` as row 1's `publish_marks_done_not_merged_for_a_split_branch_repo` does, with `front.publish.branch = "release"` and `deps_dirs`, `install`, `install_inputs` empty. | **`earlier_deferred_push_false_sibling_holds_the_close_on_own_publish`** (Codex's scene): S writes `front/new.txt`, checkpoints, publishes `front` → `push` null, the rebase `Deferred recent_writes`, `reasons == ["not_pushed"]`, `detail.what == "publish"`, `detail.commit` = C, `detail.on_chain == true`, `detail.chain == "leads"`, `detail.pending.source == "publish"`, `detail.pending.reason == "recent_writes"`, a `daemon` mark. Restore. S writes `tool/x.txt`, checkpoints, publishes `tool` → `published[0].push.kind == "Pushed"`, **`closed: false`, `sessions[S].reasons == ["not_pushed"]`, `detail.repo == "front"`, `detail.commit` = C**, S Active, both pins present, **`store.rebase_pending_of(S, front).head == Some(C)`**, `front`'s `refs/strata/remotes/origin/main` unchanged (r4 closed S here and deleted the row, sessions.rs:682: the assertions the fix flips). Then `push {repo: "front"}` → `push.kind == "Pushed"`, `closed: true`, S Removed, both pins gone. **`earlier_deferred_push_false_sibling_holds_the_close_on_a_rejudge`**: the same first publish and Restore, no second publish by S. T writes `tool/t.txt`, publishes `tool` → `Pushed`; set (b) holds S because S has `tool` in its manifest and a `daemon` mark → `auto_close.rejudged == [S]`, `sessions[S].in_group == false`, `sessions[S].closed == false`, `sessions[S].reasons == ["not_pushed"]`, `sessions[S].detail.repo == "front"`, `closed: true` (T's group), S Active, the row intact. Then `push front` closes S. **`earlier_split_branch_sibling_holds_the_close_on_own_publish`**: S publishes `front` → `Pushed` on `release`, `Deferred`, `reasons == ["not_merged"]`, `detail.what == "publish"`. Restore. S publishes `tool` → `closed: false`, `reasons == ["not_merged"]`, `detail.repo == "front"`, `detail.publish_branch == "release"`, `detail.default_branch == "main"`, `detail.ref == "refs/strata/remotes/origin/main"`, S Active, the row intact (r4 closed S here). Then merge `release` into `main` on the remote and `core.gits[&front].fetch("origin", "main")`; T publishes `tool` → `sessions[S].closed == true`: a merged split publish closes on a later judge. **`earlier_split_branch_sibling_holds_the_close_on_a_rejudge`**: the same first publish and Restore; T publishes `tool` → `sessions[S].reasons == ["not_merged"]`, `sessions[S].in_group == false`, `closed: true` (T's group), S Active, the row intact. **`a_record_sync_consumed_does_not_hide_an_unlanded_publish`** (why the row alone is not the proof): `push: false`, the first publish; origin gains O from another clone (`Chain::advance_origin`, tests/resolve.rs:117-122); `core.gits[&front].fetch("origin", "main")` → `Diverged`; Restore; `sync {session: S, repo: "front"}` → `moved` onto O, `rebase_pending_of(S, front)` is `None` (sync_move.rs:499). S publishes `tool` → `reasons == ["not_pushed"]`, `detail.what == "publish"`, `detail.commit` = C, no `detail.pending`, `detail.on_chain == true`, `detail.chain == "diverged"`. **Then `push front` (headless) replays and pushes → `replayed.commits == 1`, `closed: true`, S Removed**: C is off L for good and its copy names the publish (`Strata-Publish: <id> (front)`). **`replay_adopted_target_holds_the_close_until_it_lands`**: `push: false`, the first publish; origin gains O; fetch; a pre-receive hook on `front`'s origin exits 1 (the shape of tests/daemon.rs:1298-1303); `push front` → `PushRefused` with `detail.replayed`; the row is `{head: C′, source: "replay", unpushed: true}` (pending.rs:245-247; rm_state.rs:590-595). Restore. S publishes `tool` → `reasons == ["not_pushed"]`, `detail.commit` = C, `detail.copy` = C′, `detail.pending.source == "replay"`, `detail.pending.unpushed == true`, `detail.on_chain == true`, `detail.chain == "leads"`, the row intact. Hook removed; `push front` → `closed: true`. **`unknown_publish_of_a_sibling_holds_the_close`**: `front` `push: true`; `strata_publish::seams::fail_once(git, FailPoint::BeforePush)` (strata-publish tests/catch_up.rs:298) makes the publish of `front` fail after its commit: error path, no mark, the publish row `Unknown`, the pending row `head = C`, `publish_unfinished`. Restore (`candidates` reads `Done` rows only, remove.rs:987, so a non-empty overlay would stay `unpublished` after the push). S publishes `tool` → success (`refuse_open` is per repo) → `reasons == ["not_pushed"]`, `detail.phase == "unknown"`, `detail.commit` = C, `detail.pending.reason == "publish_unfinished"`. `push front` → `closed: true`, the publish row still `Unknown` until the next publish reconciles it (push.rs:493-528). **`chain_drop_names_the_agents_rm`**: `push: false`, the first publish; origin gains O; fetch; Restore; `chain_resolve {repo: "front", choice: "AcceptOriginDrop"}` (tests/rpc.rs:240). S publishes `tool` → `reasons == ["not_pushed"]`, `detail.on_chain == false`, `detail.chain == "equal"`, the note names `git-strata rm --session S`; then `rm {discard: false}` passes (the agent's `rm` is not held by the proof, §11 item 27). **`main_target_on_an_unpushed_chain_holds_the_session`**: `push: false`, `catch_up_every` long (support/mod.rs:358-359) so T2 stays out; S has `front` tracked and clean; U = `new {repos: ["front"], only: true}` publishes `front` → chain top Cᵤ, main-follow enrolls S (main_follow.rs:253-261). S publishes `tool` → `reasons == ["not_pushed"]`, `detail.repo == "front"`, `detail.what == "pending"`, `detail.pending.source == "main"`, `detail.head` = Cᵤ. `push front` closes S and U. **`group_mate_is_not_held_by_a_commit_it_did_not_publish`** (Claude r4 nit 2): `push: false`; S = `new {repos: ["tool"]}` created first, so `front` joins it untracked with its base on L; U = `new {repos: ["front"], only: true}`; one group of `batch(U, front)` and `batch(S, tool)` → `sessions[S].closed == true`, `sessions[U].reasons == ["not_pushed"]`, `closed: false` (r4 held S with `what: "publish"`). **r6, the two replay false negatives (Claude r5 nit 8):** **`landed_replay_with_a_stale_row_refuses_until_t2`**: a pin `confirm: true, push: false` on `front` (row 2's shape), so the push takes the confirmed form, which runs no reconstruct; the first publish, deferred, row `{head: C}`; origin gains O; fetch; `chain_resolve {repo: "front", choice: "AcceptOriginReplay"}` (no plan, resolve.rs:643-652); Restore; `push {repo: "front", shown}` with the scripted confirmer → `Pushed`, and the push trigger judges S: the publish row passes by its copy, `reasons == ["not_pushed"]`, `detail.what == "pending"`, `detail.head` = C, `detail.on_chain == false`. A T2 pass discards the row (`stale_target`, pending.rs:83-91), and then T's publish of `tool` closes S on set (b). **`landed_replay_on_a_moved_session_refuses_on_its_old_head`**: `push: false`, `front`'s `quiet_secs: 0`, so S publishes and is `Moved` onto C; a cwd `Worker` in `front`'s workspace; origin gains O; fetch; `push front` (headless) replays and pushes, and `rebase_replayed` defers `open_files` with the row `{head: C′, source: "replay"}`; the push trigger judges S → `reasons == ["not_pushed"]`, `detail.what == "base"`, `detail.head` = C, `detail.on_chain == false`. The holder leaves; a T2 pass moves S onto C′; the agent's `rm {discard: false}` passes. |
| 16 | crates/strata-daemon/tests/upstream.rs, r5, the reread and `gone`. Pauses as tests/old_tree_yield.rs:105 (`d.sessions.arm(SessionSeam::PauseAt { … })`) at a new `auto_close.after_enumerate` and r4's `auto_close.before_judge`. | **`push_rereads_the_done_mark_under_the_queue`** (Codex r4 nit 1: enumeration → unmark → queue): a `push: false` repo, S published and marked done by the daemon (`not_pushed`). `push {repo}` on a thread, paused at `auto_close.after_enumerate`; `mark {session: S, state: "active"}` succeeds at the pause (the assertion that the enumeration holds no queue); release → `push.kind == "Pushed"`, the reply has neither `closed` nor `auto_close`, S Active, **`store.mark(S)` is `None`** (the hook did not bring it back), no audit line, S's pin present. **`publish_set_b_rereads_the_done_mark_under_the_queue`**: the same with T's publish that pushes the repo → `rejudged == []`, no `sessions[S]`, `closed` is T's. **`mark_done_after_the_enumeration_waits_for_the_next_trigger`**: `mark done` on an unmarked session at the same pause → not judged in this run, judged by the next push. **`own_session_removed_before_its_hook_reads_closed`** (Claude r4 nit 3): B's publish paused at `auto_close.before_judge`; the agent's `rm {discard: false}` removes B at the pause; release → `closed: true`, `sessions[B]` = `{closed: true, in_group: true, by: "other", reasons: [], rm: null}`, no `upstream[B]`, the audit line `closed: false`, `reasons: ["gone"]`, no mark write attempted. |
| 17 | crates/strata-session/tests/remove_landed.rs (`t3_session` :748, `t3_publish` :758), crates/strata-store/tests/rebase_pending.rs, crates/strata-gitio/tests/replay.rs, r5, the proof inside `remove_strict`. | **`remove_strict_refuses_an_outstanding_target_at_the_recheck`**: a session whose overlay is restored to empty, with a `t3_publish(…, fetch: false, build: true)` commit off L → `remove_strict` is `Dirty changed_during_remove`, `detail.after == "checkpoint"`, `detail.outstanding[0].what == "publish"`, the session Active; `remove(id, false, None)` passes (the control, §11 item 27). **`remove_strict_refuses_when_pending_changes_after_the_proof`**: `PauseAt "remove.after_recheck"`; at the pause `store.enroll_main(…)` with a head on L → `Dirty changed_during_remove`, `pending_changed: true`, the session root still at its path, the row present (the guard, not the proof, caught it). **`remove_strict_passes_a_deferral_during_the_close`**: a row enrolled with `enroll_main` whose head is on L; at the pause `defer_pending` → removed, the row gone with `Removed`. **Store:** `set_removing_if_unchanged_compares_every_key` (equal keys → `Removing`; a moved `rev`, a new row, a set `planned_head` or `planned_publish` → `PendingMoved`, the lifecycle unchanged; a moved lifecycle → `LifecycleMoved`). **Gitio:** `landed_copy_finds_a_replayed_commit` (`replay_commits`, then the copy on `m..tip`, naming the session); `landed_copy_needs_the_session_trailer` (r6, Claude r5 nit 2: the same `Strata-Publish: <id> (<repo>)` with `sessions: s-other`, as another store's commit would carry → `None`; a subject-only commit → `None`; a group's `sessions: s-a, s-b` → found for either); `landed_copy_none_when_the_commit_is_pruned`; `landed_copy_ignores_a_copy_past_the_limit`. `commit_of` is untouched: its subject fallback stays covered by strata-publish's existing tests, and `commits_between_trailers`' callers (crates/strata-publish/tests/r2.rs:143, :632) still pass with the new field. |
| 18 | r6, the two Codex r5 blockers. crates/strata-store/tests/rebase_pending.rs (`world`, :45, an Active session); crates/strata-publish/tests/main_follow.rs (`fx`, `advance`, `row`, :9-22, :232, plus a new `PublishFault::PauseBeforeMainTarget`, seams.rs:76-99, hit in `reconcile_locked` right before :225 and :253; `f.sessions()`, `SessionSeam::PauseAt` as tests/remove.rs:821); crates/strata-daemon/tests/upstream.rs (the store writes of `deps_and_auto_fields_follow_store_and_cached_summary`, :620-649; pauses as row 16). In the daemon scenes the target is a real commit on origin: pull, commit, push, `core.gits[&repo].fetch("origin", "main")`, the shape of strata-publish's `advance`. The daemon tests' own `advance` (:61-73) will not do: it commits on `fx.main`, whose HEAD lacks the publish, and sets the tracking ref to that commit, so L would lose the publish. | **Blocker 1, the regression Codex asked for: `stale_main_follow_cannot_enroll_after_removing`**: S on `tool`, clean; the target is `advance(&f, false)`, and in a second variant U's `push = false` chain top, the unpushed C of Codex's scene. Arm `PauseBeforeMainTarget`; a thread runs `main_follow::reconcile` and stops holding the repo lock, having read S as Active. Arm `PauseAt "remove.unpin"` (:420, before the cleanup lock); a second thread runs `remove_strict(S)` and stops there, and `lifecycle(S) == Removing`. Release the writer and join it (`Ok`) → **`row(&f, &S)` is `None`** (r5 has C's row here, and `Removed` drops it unproved: the assertion the fix flips). Release the close → `Removed`, the proof's claim intact. **`stale_main_follow_cannot_retarget_after_removing`**: S has a row `{head: H}` with H on L; a second `advance`; the writer stops before `retarget_pending`; the same order → `row(&f, &S).head == H` while `Removing`. **Store, `target_writers_refuse_a_session_leaving_for_removal`**: on `removing`, then on `removed`: `enroll_main` → `Ok(false)` and no row; `retarget_pending`, `settle_planned(adopt: true)` and `promote_pending(Some(..))` → `Ok(false)` and the head unchanged; `plan_replay` → `Ok(())` and no `planned_head` for that session, while another session's plan in the same call is written. `settle_planned(adopt: false)` and `promote_pending(None)` still clear. **Store, `replay_writers_keep_creating_and_unknown_main_writers_do_not`**: on `creating` and `unknown`, `plan_replay`, `settle_planned` and `promote_pending` behave as today (no regression of `session_heads`' set, resolve.rs:877-885), and `enroll_main` and `retarget_pending` answer `false`. **Blocker 2, the regression Codex asked for: `deps_proposal_during_phase_b_refuses_the_close`**: `auto_close` on; S edits and checkpoints; arm `PauseAt "remove.before_removing"`; publish on a thread and wait for the pause. At the pause: the commit above as `target`, `record_proposal` with `origin: None`, `to` = `"b".repeat(64)`, `tree` and `head: target` (the :628-646 call), and `enroll_main(S, repo, &ws.base, &target, &tree, None, now)`. Release → `ok: true`, `published[0].push.kind == "Pushed"`, `closed: false`, **`sessions[S].reasons == ["deps_pending"]`**, `detail.proposal` = that id, `detail.state == "pending"`, `detail.after == "checkpoint"`, a `daemon` mark, **S Active, its root and pin in place, `rebase_pending_of(S, repo).head == Some(target)`, the proposal still `Pending`**, and `last_rm.detail.reason == "changed_during_remove"` (r5 closed S here: step 5.5 (c) accepted the target and the keys matched). Variant: the same writes at `remove.after_recheck` → refused too, `reasons == ["changed_during_remove"]`, `pending_changed: true`, because the row moved; with the proposal alone at that pause → `["deps_pending"]`. **`own_draft_during_phase_b_refuses_the_close`**: at the pause, `record_proposal` with `origin: Some(S)`, no tree or head, as `deps_propose` writes it → `["deps_pending"]`, `detail.state == "awaiting_publish"`. **`conflicts_deferral_during_phase_b_refuses_the_close`**: `quiet_secs` high, so the row is `{head: C, reason: "recent_writes"}`; at `remove.after_recheck`, `defer_pending(S, repo, rev, "conflicts", …)`, which keeps the keys → `["conflicts"]`, `detail.auto_reason == "conflicts"`, S Active. **`applied_proposal_no_longer_holds_the_session`**: after the first scene, `decide_proposal(p, Applied)`; T's publish of the repo re-judges S (set (b)) → the transaction passes, S closes. **Store, `set_removing_if_unchanged_reads_clause_6`**: a `pending` proposal with another `to_set` → `Reopened`, code `deps_pending`, the lifecycle unchanged; `to_set` equal to the base's deps → `Removing`; this session's `awaiting_publish` → `Reopened`; another session's → `Removing`; **r9: a `pending` proposal of another origin on a repo the session has untracked → `Removing`; this session's own `awaiting_publish` or `pending` on such a repo → `Reopened`, `own: true`, `tracked: false` (r8 answered `Removing`: the assertion the fix flips); its `applied` or `superseded` one → `Removing`;** a row with `reason = 'conflicts'` → `Reopened`, code `conflicts`; a workspace with `conflicts` → the same; **r9: a `sync_report` row → `Reopened`, code `conflicts`, `report: true`.** |
| 19 | r6, the hook's failure paths and the mark. crates/strata-daemon/tests/upstream.rs (pauses as row 16; `core.locks.shutdown()`, locks.rs:713; a new `test-seams` panic at `auto_close.in_closure`, inside step 2's closure, so the queue place is held when it fires). | **`hook_panic_leaves_the_publish_landed`** (Claude r5 nit 4): the seam panics once → `ok: true`, `published[0].push.kind == "Pushed"`, `sessions[S].reasons == ["error"]`, `detail.code == "handler_panicked"`, `mark.skipped == "error"`, an audit line, no mark row, S Active; then `rm {discard: false}` on S takes the queue place and passes, so nothing stayed held. **`queue_take_failure_writes_no_mark`** (Claude r5 nit 5): at `auto_close.after_enumerate`, `core.locks.shutdown()`; release → `reasons == ["error"]`, `detail.code == "ReadOnly"`, `mark.skipped == "error"`, no mark row. **`rejudge_with_the_same_reasons_keeps_mark_at`** (Claude r5 nit 9): S `not_pushed` on `front`, a daemon mark at `at₀`, its note naming no publish id; two sessions T₁ and T₂ each publish `tool` (each closes, so one session cannot do both), and set (b) judges S each time → `store.mark(S).at == at₀`, `mark.skipped == "unchanged"`, the note the same; S then publishes a second `front` commit → the note lists both and `at` moves. **`refused_publish_pushed_by_a_human_reads_pushed_on_the_rerun`** (Claude r5 nit 1): a pre-receive hook refuses A's first publish: error path, no mark; hook removed; `push tool` → `Pushed`, and the trigger skips A, which has no mark; A re-runs the same publish → `already_published[0].push.kind == "Pushed"`, `landed_later == true`, no `push_refused` or `not_pushed`, and `reasons == ["unpublished"]` with `landing.tool.why_none == "no_built_base"` (the refused publish built no base, §3.1); variant, session B created before the re-run so that its `new` builds that base → `closed: true`. `already_published_refused_push_marks_push_refused` (row 1) stays green: a commit that has not landed keeps `Refused`. |
| 20 | r7, the blocker and the nits that change behaviour. crates/strata-daemon/tests/upstream.rs (`call` on the test's thread, :15-24; threads as tests/reload.rs:488; `Dfx::with(&["front", "tool"])`, `auto_close` on globally, `front` with `publish.auto_close = Some(false)`, `write_config` before `prepare`; S = `new {repos: ["tool"], only: true}`, rpc.rs:95-97). Two seams: r4's `auto_close.before_judge`, which §1 now places between steps 1 and 2 holding nothing, and a new `auto_close.after_reread`, right after step 3 under the place. Both reach `Seams::hit`, which is `pub(crate)` (strata-session seams.rs:69), through one new `test-seams` wrapper on `Sessions`. crates/strata-session/tests/remove_landed.rs (`t3_session` :748, `t3_publish` :758, `land`) and remove.rs. | **The blocker, the regression both reviews ask for: `add_of_opted_out_repo_during_the_hook_disables_the_close`**: S edits `tool` and checkpoints; arm `PauseAt "auto_close.before_judge"`; publish on a thread and `wait_paused`, so step 1 has passed over `{tool}`; `add {session: S, repos: ["front"]}` returns `report.added.front` at the pause, which proves the hook holds no queue; release → `ok: true`, `published[0].push.kind == "Pushed"`, `closed: false`, `sessions[S].reasons == ["disabled"]`, `sessions[S].detail.repo == "front"`, `rejudged == []`, **S Active, both workspaces on disk, `refs/strata/pins/<S>` in both repos, `store.mark(S)` is `None`, no line in `sessions-audit.jsonl`** (r6 removes both workspaces here: the assertions the fix flips). Claude's sketch held `add` at `add.build` first, and that order cannot run: an `add` holding S's queue also blocks the publish's `record_publish` (push.rs:770-778), so the publish never reaches the hook, and `Seams` has one pause slot (seams.rs:25-28). **`add_on_an_unpushed_chain_does_not_mark_an_opted_out_session`** (Claude's second scene): `front` also `push: false`, and U = `new {repos: ["front"], only: true}` has published it, so `front`'s chain leads origin (U answers `disabled`); the same pause and `add`, so S's `front` head is that chain top → `reasons == ["disabled"]`, not `["not_pushed"]`, and no mark row. **`push_rejudge_after_add_of_opted_out_repo_is_disabled`** (the set (b) path; **r8: with an agent's `mark done` at `at₀`, because `add` now clears a daemon's mark, row 21**): `tool` `push: false`, S published, `not_pushed`, the agent's mark at `at₀`; `push {repo: "tool"}` on a thread, paused at `auto_close.before_judge`; `add front`; release → `push.kind == "Pushed"`, no top-level `closed`, `auto_close.closed == false`, `auto_close.sessions[S].reasons == ["disabled"]`, `rejudged == []`, S Active, `store.mark(S).at == at₀`, no new audit line. **`add_after_the_reread_waits_and_finds_the_session_closed`** (the queue makes the read final): arm `PauseAt "auto_close.after_reread"`; publish on a thread and `wait_paused`; `add front` on a second thread, which waits for the place; release → the publish answers `closed: true`, `add` answers `NoSuchSession` or `InvalidInput` (slim_and_add.rs:116-117), or `Busy` if its own first-take bound ran out during the close, never `added`; S Removed, and `front` has no workspace or pin of S. **`failed_holder_scan_is_error_not_unknown`** (Claude r6 nits 4, 5): `fail_open_files_under(<paths.root>/sessions/.repos/<S>, StrataError::new(Code::Unknown, …))` (strata-deps seams.rs:166; thread-local, and `call` runs the handler on this thread; on this path only `refuse_open_files` scans that directory, remove.rs:337-338, while the post-publish move scans the workspace and its staging, move_workspace.rs:623-624, :718) → `refuse_open_files` answers `Code::Unknown` with S `Active` → `reasons == ["error"]`, `detail.code == "Unknown"`, `mark.skipped == "error"`, no mark row, an audit line, S Active; keyed on the code, this would have read `unknown`. **`failure_after_removing_is_unknown`**: `d.sessions.arm(FailAt { phase: "remove.unpin", errno: EIO })` → the row is `Unknown` → `reasons == ["unknown"]`, `detail.state == "unknown"`, no mark row although `set_mark` would accept that row (marks.rs:83), `closed: false`. **`upstream_unavailable_writes_no_mark`**: `core.upstream.fail_for_test(true)` (the :498 shape) → `reasons == ["upstream_unavailable"]`, `mark.skipped == "error"`, no mark row, S Active. **`assignment_to_the_session_holds_the_close`** (Claude r6 nit 2): a `Done` publish row of `tool` planted with `set_publish_outcome`, its outcome naming `repo: "tool"` and `assigned: [{path: "src/a.ts", session: S, at: <now>}]` (the shape `assign` writes, rebase.rs:909-911); S publishes `src/b.ts` → `reasons == ["assigned"]`, `detail.repo == "tool"`, `detail.paths == ["src/a.ts"]`, a `daemon` mark, S Active; S publishes `src/a.ts` → the assignment has ended (rebase.rs:947) → `closed: true`; variant, the row names another session → S closes on the first publish. **`publish_marks_done_for_a_nested_repository`** (Claude r6 nit 1): `git init` plus one commit at `<ws>/vendor/x` (the shape of strata-overlay tests/classify.rs:583); S publishes an unrelated path → `reasons == ["embedded_repo"]`, `detail.embedded_repos.tool.paths == ["vendor/x"]`, a `daemon` mark, S Active, `vendor/x/.git` on disk; then the agent's `rm {discard: false}` passes (today's rule). **Session level, `remove_strict_refuses_a_nested_repository`**: tracked and untracked repos both refuse; the control `remove(id, false, None)` passes. **`remove_strict_refuses_a_nested_repository_at_the_recheck`**: `PauseAt "remove.before_removing"`, `git init` at the pause → `Dirty changed_during_remove`, `after: "checkpoint"`, the directory among `repos.tool.paths`; the same under `remove(false)` passes. **Codex r6 nit 1, `remove_strict_discharges_an_entry_equal_to_the_target`**: `t3_session`; the workspace's `src/big.ts` = v3; `land` puts v2 on origin; `t3_publish(…, [("src/big.ts", v3)], …, true, true)` → `remove_strict` passes (r4 called this a conflict). **`remove_strict_refuses_own_content_a_later_publish_replaced`**: `src/p.ts` = v1 and `src/q.ts` in the workspace; `t3_publish` of `src/p.ts` = v1 (C1); `land` of `src/p.ts` = v2; `t3_publish` of `src/q.ts` (C3) → `Dirty`, `repos.tool.paths == ["src/p.ts"]`, `landing.tool.target.commit` = C3, `landing.tool.historical == ["src/p.ts"]`; the control `remove(id, false, None)` passes by D57. **`outstanding_reads_an_unknown_rows_applied_ref_change`** (Claude r6 nit 10, unit next to row 3's): an `Unknown` row with no `outcome.commit` and an applied `UpdateRef` in `ref_changes` → the item's `commit` is that ref's `new`, as `row_commit` reads it. |
| 21 | r8. crates/strata-daemon/tests/upstream.rs (row 20's fixture and seams; `remove.after_recheck`, §3.2). **The conflict row** is real, the shape of strata-publish tests/rebase.rs:489-494: `auto_close` on; A and B edit `src/a.ts` differently and checkpoint; A publishes and closes; B's publish answers `Conflict` on the error path, and `detail.publish` names row `p`, whose outcome lists the path. S = `new {repos: ["tool"]}` edits `src/s.ts` and checkpoints. Also crates/strata-publish/tests/rebase.rs, crates/strata-daemon/tests/reload.rs, crates/strata-session/tests/remove_landed.rs, the unit tests of remove.rs (:1183-1189), rm_note.rs and auto_close.rs, crates/git-strata/tests/confirm_gate.rs and help_topics.rs, crates/strata-daemon/src/audit.rs's tests. | **The blocker, the two regressions Codex asks for. `assign_before_the_judge_holds_the_close`:** arm `PauseAt "auto_close.before_judge"`; S's publish on a thread and `wait_paused`; `assign {group: p, path: "src/a.ts", session: S}` answers `{}` at the pause, because the hook holds no queue; release → `closed: false`, `reasons == ["assigned"]`, `detail.repo == "tool"`, `detail.paths == ["src/a.ts"]`, a `daemon` mark, S Active; B's re-publish of the path answers `Conflict` with `reason == "assigned"`, naming S, which is alive to resolve it. **r9 (Claude r8 nit 3): this test and the next set `fx.lock_wait = Duration::from_secs(60)` before `prepare` (support/mod.rs:355, :389, :462). The fixture's 300 ms also bounds `assign`'s queue take, so `assign` would answer `Busy` long before the pause ends or the judge's scans finish.** **`assign_during_the_close_waits_and_writes_nothing`:** arm `PauseAt "remove.after_recheck"`; S's publish on a thread and `wait_paused`; `assign` as above on a second thread, which has not returned while the pause holds; **`store.publish_record(p).outcome` has no `assigned`** (r7 writes it here: the assertion the fix flips); release → `closed: true`, S `Removed`; `assign` answers `NoSuchSession`; a variant at the fixture's 300 ms answers `Busy` with `reason: "lock_wait"` during the pause; in both cases the row still has no `assigned`, `assigned_to(&store.publishes(), &S)` is empty, and B's re-publish of the path does not answer `reason == "assigned"`. Variant, the agent's `rm {discard: false}` at the same pause → the same. **`assign_waits_then_lands_when_the_close_refuses`:** a second unpublished edit in S, `PauseAt "auto_close.after_reread"`; `assign` on a thread; release → `reasons == ["unpublished"]`, S Active, `assign` answers `{}`; T's publish of `tool` re-judges S (set (b)) → `sessions[S].reasons == ["assigned"]`. **`assign_refuses_a_session_leaving_for_removal`** (strata-publish, next to `assign_names_resolver_and_does_not_push`, which stays green): `set_lifecycle(&b, Active, Removing, None, now)` (sessions.rs:645) → `assign` is `InvalidInput`, `reason == "lifecycle"`, the row unchanged; on to `Removed` → `NoSuchSession`. **`assign_holds_the_queue_while_it_waits_for_the_repo`:** a thread holds `core.locks.with_repo(tool)`; `assign` naming S on a second thread; polling `core.locks.try_session(&S, …)` reaches `None`, which it never would if `assign` waited for the repo first; release → `assign` answers `{}`. **Every repo, one read (Claude r7 nit 9): `assigned_to_reads_every_repo_of_the_rows`** (unit): an assignment to S in `front`, with S holding only `tool` → `{front: [path]}`; one naming another session → absent; a later `Done` row landing the path → absent; no `assigned` in any row → empty. **`assignment_in_a_repo_the_session_does_not_hold_holds_the_close`:** `Dfx::with(&["front", "tool"])`; the conflict row on `front`; S = `new {repos: ["tool"], only: true}`; `assign` names S; S publishes `tool` → `reasons == ["assigned"]`, `detail.repo == "front"`, a `daemon` mark, S Active (r7 read manifest repos only and closed S). **Nit 1: `strict_dirty_message_never_names_discard`** (unit, next to :1183-1189): `repos`, `old_trees` with `changed` true and false, `late_writes`, `embedded_repos` and `unresolved` → no message contains `--discard`; `changed: false` reads "unchanged" and never "changed old trees"; the nested path is named; the `Agent` assertions at :1183-1189 are untouched. **`attempt_summary_names_kept_trees_and_nested_repositories`** (rm_note.rs): every item `changed: false` → `Dirty (kept old trees)`; a mix, or an item with no `changed` key → "changed old trees"; `embedded_repos` → "nested git repositories". **`close_refusal_stores_no_discard_advice`:** row 20's nested-repository scene and row 7's kept-tree scene → `store.rm_attempts(S)[0].detail["message"]` has no `--discard` and starts "auto-close refused". **`remove_strict_refuses_an_unknown_session`** (own correction, §3.1): an `Unknown` row → `InvalidInput`, `reason == "lifecycle"`; the control `remove(id, false, None)` → `Dirty` with `unresolved` naming `session_unknown` and its `--discard` sentence, as today. **Nit 2: `judge_read_is_error_not_a_refusal`** (auto_close.rs unit, over the pure mapping from `StrataError` to an outcome): `repos: {tool}` with `landed_errors: {tool}` → `reasons == ["error"]`, `detail.code == "judge_read"`, `detail.errors.tool` present, no mark; the same with `projection_errors`; `repos: {tool, front}` with `landed_errors: {tool}` → `reasons == ["unpublished", "error"]`, `detail.repos` names `front` only, a mark whose note does not name `tool`; `landed_errors: {tool}` with `old_trees` non-empty → `["kept_trees", "error"]`. **`remove_strict_reports_a_failed_landed_read`** (the `remove_landed_git_error_is_conservative` shape, remove_landed.rs:346-363, under `remove_strict`) → `Dirty`, `landed_errors.tool` present, S Active, so the mapping's input exists. **Nit 3 (reload.rs): `false_set_during_the_publish_disables_the_close`:** at `auto_close.before_judge`, `fx.config.publish.auto_close = false`, `write_config`, `config_reload`; release → `reasons == ["disabled"]`, no mark row, no audit line, S Active; variant, a per-repo `Some(false)`. **`true_set_during_the_publish_waits_for_the_next_request`:** global `false` at entry; at `auto_close.after_enumerate` flip to `true` and reload; release → `disabled`; the next publish closes. `auto_close_follows_a_config_reload` (row 12) stays green. **Nit 4: `add_of_an_opted_in_repo_during_the_hook_is_extended`:** row 20's fixture with `front` not opted out; `add front` at `auto_close.before_judge`; release → `closed: false`, `reasons == ["extended"]`, `detail.repos == ["front"]`, `mark.skipped == "extended"`, no mark row, one audit line, S Active, both workspaces and pins in place (r7 closed S here); S then edits and publishes `tool` again → the baseline holds `front` → `closed: true`. **`add_during_the_publish_is_extended_too`:** the same `add` at `PublishFault::PauseBeforeRebase` (strata-publish seams.rs:89, :166), so step 1's read would already see `front` → `reasons == ["extended"]`, which fails if the baseline is read at step 1 rather than at entry. **`add_clears_the_daemons_mark_and_keeps_an_agents`:** S `not_pushed` with a daemon mark; `add front` → `store.mark(S)` is `None`; `push tool` → S is not judged, no `auto_close`, S Active. Variants: an agent's `mark done` → `add` keeps it, `by_kind == "agent"`; an `add` naming only a repo S has → the daemon's mark stays. **Nit 5: `failed_scan_records_refused_not_failed`:** row 20's `fail_open_files_under` scene → `store.rm_attempts(S)[0].outcome == Refused`, and the `status` note reads "refused: Unknown; nothing removed"; the same under the agent's `rm`; row 20's `failure_after_removing_is_unknown` scene → `Failed`, today's answer. **Nit 6: `closed_session_on_a_non_tty_is_no_such_session`** (confirm_gate.rs, `guarded_live`): a session removed; `publish --from S:a.txt` from the sandbox on a non-TTY → `NoSuchSession`, not `NotTty`, and no request reaches the daemon; `confirm_commands_refuse_non_tty` stays green as the control (§6). **Nit 7: `mark_help_says_a_done_session_can_close`** (help_topics.rs): `git-strata mark --help` names auto-close and the trash and no longer says "leave removal to the human"; `rm --help` and the top-level help agree (§8's table). `note_commands_parse` (row 14) stays green. **Nit 8: `audit_write_failure_keeps_the_close`** (sessions-audit.jsonl replaced by a directory, so `symlink_metadata` is not a file, audit.rs:99-102): the reply is still `closed: true`, S `Removed`, and `daemon_status` names the failure. The crash window itself is stated in §9, not tested. **Nit 10: `removed_group_session_with_an_opted_out_repo_reads_gone`:** a group of two sessions on `tool` (A is the one that sorts first; the other, S, gets `add front`, opted out, before the publish); `PauseAt "auto_close.before_judge"` stops A's hook; the agent's `rm {session: S}` at the pause; release → `sessions[S]` = `{closed: true, in_group: true, by: "other"}`, not `disabled`. **`set_b_judges_a_session_holding_two_pushed_repos_once`:** S done-marked, holding `front` and `tool`; T's group pushes both → `rejudged == [S]`, one audit line for S. |
| 22 | r9. crates/strata-daemon/tests/upstream.rs (row 20's `call`, threads and seams; `core.upstream.fail_for_test`, the :498 shape). **The sibling fixture:** `Dfx::with(&["front", "tool"])`, `auto_close` on, `write_config` before `prepare`; S = `new {repos: ["tool"]}` with `only` absent (rpc.rs:95-97), so `front` joins untracked. "Draft" = write `NEW_DEP` (tests/deps.rs:52-60, moved to the support module) to `package.json` in S's `front` workspace, then `deps_propose {session: S, repo: "front", switch: false}` as tests/deps.rs:103-106 → `proposal.state == {kind: "AwaitingPublish"}`, id `p`. "Restore" = write `PACKAGE_JSON` back (support/mod.rs:187, :316), so `front`'s overlay is empty. Also crates/strata-session/tests/remove_landed.rs and remove.rs, the unit tests of auto_close.rs, crates/git-strata/tests/help_topics.rs. | **The blocker, the regression Codex asks for: `own_draft_on_an_untracked_sibling_holds_the_close`:** Draft; Restore; S edits `tool`, checkpoints and publishes it → `ok: true`, `published[0].push.kind == "Pushed"`, `closed: false`, `sessions[S].reasons == ["deps_pending"]`, `detail.repo == "front"`, `detail.proposal == p`, `detail.state == "awaiting_publish"`, `detail.own == true`, `detail.tracked == false`, a `daemon` mark whose note names `p`, **S Active, both workspaces on disk, `refs/strata/pins/<S>` in both repos, and `deps_list` still lists `p`** (r8 closes S here and `deps_list` loses `p`: the assertions the fix flips). Then the agent's `rm {discard: false}` passes and `deps_list` no longer lists `p`: the way out, with `Agent` unchanged. **`own_draft_on_an_untracked_sibling_holds_a_rejudge`:** an agent's `mark done` on S; Draft; Restore; T = `new {repos: ["tool"], only: true}` publishes `tool` → `rejudged == [S]`, `sessions[S].in_group == false`, `sessions[S].reasons == ["deps_pending"]`, `detail.tracked == false`, `closed: true` (T's group), S Active: the trigger does not change the answer. **`own_draft_on_an_untracked_sibling_during_the_close_refuses`** (line 3): S edits and checkpoints `tool`; arm `PauseAt "remove.after_recheck"`; publish on a thread and `wait_paused`; at the pause, `record_proposal` on `front` with `origin: Some(S)`, no tree or head, as row 18's `own_draft_during_phase_b_refuses_the_close` writes it on a tracked repo; release → `ok: true`, `reasons == ["deps_pending"]`, `detail.tracked == false`, `detail.after == "checkpoint"`, a `daemon` mark, S Active, its root and both pins in place (r8's line 3 does not read `front` and commits `Removing`: the assertion the fix flips). **`main_proposal_on_an_untracked_sibling_does_not_hold_the_close`** (the control, §11 item 53): `record_proposal` on `front` with `origin: None`, `to` = `"b".repeat(64)` and a real `tree` and `head` (row 18's call) → S closes, `closed: true`, and `deps_list` still lists the proposal. Store: row 18's `set_removing_if_unchanged_reads_clause_6`. **Nit 1: `saved_sync_conflict_holds_the_close`:** `Dfx::new()`, `auto_close` on, `catch_up_every` long (support/mod.rs:358-359) so T2 stays out; a cwd `Worker` in S's workspace; S writes `src/p.ts` = v1, checkpoints, publishes → `Pushed`, `Deferred open_files`, `reasons == ["open_files"]`, a `daemon` mark. T writes `src/p.ts` = v2, checkpoints, publishes → T closes. The holder leaves. `sync {session: S, repo: "tool"}` → `Conflict`, `detail.reason == "sync_conflict"`, `store.sync_report(S, tool)` non-empty, the workspace row's `conflicts` empty. U publishes `src/u.ts` → `sessions[S].closed == false`, `sessions[S].reasons == ["conflicts"]`, `detail.repo == "tool"`, `detail.report == true`, `detail.conflicts == ["src/p.ts"]`, S Active, the report intact (r8 closes S here, because the projection onto S's own publish discharges `src/p.ts` and `Removed` deletes the report: the assertions the fix flips). Then S writes `src/p.ts` = v2 and runs `sync` with `resolved: ["src/p.ts"]`, `target` and `generation` from that `Conflict` → moved, report empty (sessions.rs:955); V publishes `src/v.ts` → `sessions[S].closed == true`. Variant from the refusal: the agent's `rm {discard: false}` passes (`sessions.rs:683` deletes the report). **Nit 2: `remove_strict_phase_b_projection_failure_is_err`** (remove_landed.rs): `t3_session`, `t3_publish(…, fetch: true, build: true)`, no move, overlay = exactly the published entries; a `BeforeCheckpoint` double next to `Meddling` (:365-373) that runs the real `strata_overlay::checkpoint` and then overwrites that workspace's git index with garbage, so `read_index` (`project`, remove.rs:961) fails on `parse_index`'s signature check (strata-gitio index.rs:211-213) → `remove_strict` is `Err` and not `Dirty`, S Active, its root in place. **`remove_strict_phase_b_lost_base_refuses`:** the double instead removes the target's built base → `project` answers `Ok(None)` → `Dirty changed_during_remove`. `rm_t3_phase_b_recomputes_the_projection_from_the_checkpoint` (:859) stays green for `Agent`. **`phase_b_failure_maps_to_error`** (auto_close.rs unit): that `Err` on an `Active` row → `reasons == ["error"]`, `mark.skipped == "error"`, no mark. **Nit 3:** row 21's two `assign` tests. **Nit 4: `strict_open_files_message_names_no_rerun`** (tests/remove.rs, a `Worker::spawn("cwd:")` holder): `remove_strict` → `OpenFiles`, the message starts "auto-close refused", names `git-strata rm --session` and lacks "run the same command"; `remove(id, false, None)` keeps today's message. Row 1's `publish_marks_done_open_files` gains `store.rm_attempts(S)[0].detail["message"]` with the strict text. **Nit 5:** row 21's `mark_help_says_a_done_session_can_close` also asserts that the top-level help's `rm` line no longer says "agents finish with `mark done`"; the README, skill, reference and cli.md rows of §8's table by eye. `note_commands_parse` (row 14) gains `sync --session S --repo REPO` and `deps list --repo REPO`. **Nit 6: `assigned_detail_lists_every_repo`:** row 21's `assignment_in_a_repo_the_session_does_not_hold_holds_the_close`, with a second assignment to S in `tool` → `reasons == ["assigned"]`, `detail.assigned` has both repos with their paths, `detail.repo == "front"`, `detail.paths` is `front`'s, and the note names both paths. **Nit 7: `push_with_only_an_extended_session_has_auto_close`:** row 20's `push_rejudge_after_add_of_opted_out_repo_is_disabled` with `front` not opted out → `auto_close.sessions[S].reasons == ["extended"]`, `auto_close.closed == false`, no top-level `closed`, `rejudged == []`, `store.mark(S).at` unchanged. Row 4's `auto_close_line_shape` already asserts `auth == "none"`, which r8's example lacked. **Nit 9: `close_uses_clause_2s_upstream_read`:** arm `PauseAt "auto_close.after_clauses"`; publish on a thread and `wait_paused`; `core.upstream.fail_for_test(true)` at the pause; release → `closed: true`, `upstream[S].as_of_rm == "before"`, `unpublished == false` (a second read would fail and omit `upstream[S]`: the assertion the fix flips). **Mapping, extending row 3's `reopened_items_report_their_own_codes`:** `{code: "conflicts", report: true}` → `["conflicts"]`, `detail.report == true`; `{code: "deps_pending", own: true, tracked: false}` → `["deps_pending"]`, `detail.tracked == false`. |
| 23 | r10. crates/strata-daemon/tests/upstream.rs (row 22's sibling fixture, Draft and Restore; row 20's threads; `call` runs the handler on the calling thread, :15-24). **One new seam:** `strata_deps::test_seams::after_build(hook)`, thread-local and one-shot like `before_main_lock` (strata-deps seams.rs:92-99, :178-180), called when `build` (build.rs:92) returns, on every path, with its build lock already dropped. The writer cannot use a `PauseAt`, because `Seams` has one pause slot and the close needs it (strata-session seams.rs:30-40), and `after_attempt_begins` fires before the inputs are copied, where Restore would answer `Drift inputs_changed` (build.rs:142-148). **"The writer"** = a thread that arms `after_build` with a hook that signals and then waits on a channel, and then runs Draft's `deps_propose`. Also crates/strata-store/tests/sessions.rs (`active`, :86; `assert_reason`, `assert_code`, support/mod.rs:159-164), crates/strata-deps/tests/rollout.rs (`world`, :353; `ws_new` makes an Active row, :327-342) and tests/proposals.rs. | **The blocker, the regression Codex asks for: `stale_draft_after_removing_is_refused`:** the writer runs up to its hook; Restore; S edits `tool` and checkpoints; arm `PauseAt "remove.unpin"` (remove.rs:420); S's publish on a thread and `wait_paused`; `store.session(&S).lifecycle == Removing`. Let the writer go and join it → `ok: false`, `error.code == "InvalidInput"`, `detail.reason == "lifecycle"`, `detail.lifecycle == "removing"`, **and `store.proposals(None, false)` has no row with `origin == Some(S)`** (r9 answers `ok: true` with `AwaitingPublish` and records the row: the assertions the fix flips). Release → `ok: true`, `closed: true`, S `Removed`, still no such row. Variants: the tracked repo (`package.json` in `tool`, restored) → the same; `switch: true` → the same error, and no move ran. **`stale_draft_after_removed_is_no_such_session`:** no pause; the writer goes after the publish has answered `closed: true` → `NoSuchSession`, no row. **The control Codex asks for: `stale_draft_before_removing_refuses_the_close`:** the same writer, with `PauseAt "remove.after_recheck"`; at the pause let it go and join it → `ok: true`, `proposal.state == {kind: "AwaitingPublish"}`; release → `ok: true`, `closed: false`, `reasons == ["deps_pending"]`, `detail.repo == "front"`, `detail.own == true`, `detail.tracked == false`, `detail.state == "awaiting_publish"`, `detail.after == "checkpoint"`, a `daemon` mark, S Active, its root and both pins in place, and `deps_list` lists the draft. Rows 18 and 22 write this scene with `record_proposal` directly; this one runs the real writer, so all that differs from the regression is which side of the commit it lands on. **Originless proposals: `main_proposal_after_removing_stays_listed`:** at `remove.unpin`, `record_proposal` with `origin: None` (row 18's call) → `Some(id)`; release → `closed: true`, and `deps_list` lists it as `Pending`. **Store: `draft_insert_reads_the_origins_lifecycle`** (tests/sessions.rs, next to `open_proposals_skip_removed_sessions`, :545): a draft on an Active origin → inserted; after `Active → Removing` → `assert_reason(InvalidInput, "lifecycle")`, no new row, and a draft passed as `replaces` is still `AwaitingPublish`; after `Removing → Unknown` (with a scene) → inserted, as today; a second origin taken on to `Removed` → `assert_code(NoSuchSession)`, no new row; an origin with no row → inserted, as today; a `pending` row (tree and head set) naming the `Removed` origin → inserted, which is the store half of the next two tests. **Deps, why the rule stops at drafts: `retarget_keeps_the_origin_of_a_removed_session`** (tests/rollout.rs; the setup of `observe_main_retarget_keeps_session_origin`, :1485-1505, up to `p` `Pending` with `origin == session(&s)`): S taken to `Removing` and `Removed` (the calls of proposals.rs:985-989) → `open_proposals` still lists `p` (Claude r9 nit 7); `observe` onto a new tip → `p2.origin == session(&s)`, `Pending`, and `p` `Superseded { by: p2.id }`. **`approve_replacement_keeps_the_origin_of_a_removed_session`** (the shape of `approve_files_replacement_when_sealed_id_differs`, :1218-1285, with `p` = S's draft promoted by a second `propose` with a target, then S removed, then `fx.sb.set_bun_version` as :1233) → `ensure_target` answers `Target::Replaced`, with `q.origin == session(&s)`. Codex's wording read literally fails both. **The one existing test that must change: `open_proposals_lists_open_and_live_only`** (tests/proposals.rs:996): it makes `s2` `Removed` first and then proposes its draft (:1000, :1019-1023), which r10 refuses. `s2` now proposes while Active and is removed afterwards; the assertions after it (:1040-1055) stand. No other test that names a `Removing` or `Removed` lifecycle inserts a draft after it **[re-grep at implementation time]**. These stay green: `open_proposals_skip_removed_sessions` (its `c` has no row, :564, and `b` proposes before it is removed, :563, :566-567), `proposal_slots_supersede_in_one_transaction` and `promote_and_decide_are_compare_and_swap` (`awaiting` origins with no row, tests/deps.rs:226-230, :269-272), `gc_reclaims_removed_sessions_after_keep` (the draft precedes `remove`, tests/gc.rs:261-262), and rows 18 and 22's scenes, where S is Active at the pause. **Claude r9 nit 3: `removed_group_session_under_a_tightening_reload_reads_gone`:** a group of two sessions on `tool`, A sorting first; at A's `auto_close.before_judge`, the agent's `rm {session: S}`, then a `config reload` that tightens `confirm` on `tool` (row 1's `publish_busy_by_code_marks_nothing`); release → `sessions[A].reasons == ["busy"]`, `detail.reason == "pins_tightened"`, and `sessions[S]` = `{closed: true, in_group: true, by: "other"}`, not `busy`, with its audit line `closed: false`, `reasons: ["gone"]`. Variant, set (b): a done-marked session already `Removed` at step 1 → absent from `sessions`, `rejudged` and the audit file. The other r10 nits change text only; rows 2, 18 and 22 already cover what they describe (`push_refused_judges_nothing`; an own `pending` refusing at line 3). |

## 11. Decisions taken here and what stays open

1. **`push` re-judges done-marked sessions only.** Alternative: every live session of the repo (closes agents'
   sessions on a human's push without any signal from them). Recommend as written; a session that is not marked done
   is not finished. r3: a publish that pushes a repo re-judges the same set (Claude r2 nit 4), so the two triggers
   agree on what a push means for a waiting session.
2. **Clause 4 (base on origin) over `ahead == 0` alone, for every manifest repo.** `ahead` is 0 against an unpushed
   chain top under `pin_unpushed` (proposal.rs:386-396); closing then would delete the only durable holder of the base
   (the pin) and leave it to a chain that drop/replay rewrite (r7 §5.4). P1 has no head export, so the close waits for
   the push; the `push` trigger (§1) closes it then. r3 (Codex r2 blocker 1): untracked siblings get their base the
   same way (new/mod.rs:573) and their pin goes with the close too (:421-430), so the clause has no `tracked` filter;
   the cost is one `is_ancestor` per untracked repo. Alternative: export the sibling's base commit under a durable
   ref at close time: that is §5.4's head export, not in P1. Recommend as written. r4: the ref is the default branch's
   tracking ref, for the base and for the published commits (item 19). r5: for every commit the close would abandon, read
   from the store (§2.1, items 25 to 29).
3. **`deps_pending` is conservative.** A pending main proposal loses nothing when the workspace goes; the brief lists
   it, so it holds the session open. Could be dropped later without changing anything else. **That applies to every `pending` proposal, an own one too (r10, Claude r9 nit 7: it stays visible after `Removed`, deps.rs:571, and refusing it rests on the requirement's text). Only an own `awaiting_publish` draft vanishes with S, so that read is not optional (item 52).**
4. **`open_files` and `changed_during_remove` leave the session open** until the agent's own `rm` or a human;
   stated in the skill text (§8). Recommend as written rather than a daemon retry.
5. **The reasons live in the note in P1** (no `reason` column, no events table); move to §1.3's columns in P2.
6. **The sidecar is §8's file with one event**; the pins file is never touched. **Open, for the requirement owner (r8, Claude r7 nit 8; r9, Codex r8 nit 1, Claude r8 nit 8):**
   autoclose-req.md says "audit.jsonl entry" and this writes `sessions-audit.jsonl`, as design-r7 §8 resolved the same phrase. Recommend keeping it, so that session traffic
   does not rotate pin timestamps away. The literal file is a small change (§7) if the owner prefers it. **Implementation need not wait: the sidecar is the default, because it is the choice that can be undone (§7). The other open questions are items 56, 58, 59 and 60.**
7. **The daemon's judge is strict; the agent's `rm` keeps the §1.8 exemption and D57's content match** (r2, r3).
   The exemption is the human's pinned rule for an explicit removal; the close is unattended and claims
   `unpublished: false`, which F1 (handlers.rs:368) defines without the exemption. r3 adds the historical match to the
   strict-only list: it is unsound for a close (§3.1) but it is also today's documented `rm` behaviour, tested by
   `remove_landed.rs`, and the agent's `rm` keeps its BeforeRemove checkpoint of the revert until gc. Making the
   agent's `rm` strict as well (projection-only, no exemption) is a behaviour change for every agent and for
   `rm_regenerable.rs` and `remove_landed.rs`, not asked for; recommend as written, and **flag for rm-followups**: the
   per-entry D57 match lacks the `newer` guard that :573-577 gives the projection, so an agent's `rm` lets a fresh
   revert to an older publish's content go (recoverable from the checkpoint); a one-line guard (`candidates` filtered
   by `newer`) would close it for `rm` too, at the price of refusing more.
8. **Where the post-scan recheck sits** (r2). Three orders were weighed by what each loses to the trash: (P)
   recheck → scan → retire still loses a closed quick write between the recheck and the scan; (X) scan → recheck →
   retire loses only writers that opened after the scan, but puts a cold scan where :302-303 promises only lifecycle
   writes; (Z, chosen) scan → recheck → scan → retire keeps X's guarantee and :302-303's sentence at the cost of one
   more `open_files_many`. The stronger alternative, retire first and checkpoint from the trash path (no write can
   land by path after the rename; an fd held across it is an R4 holder), would checkpoint after `Removing`, turning
   every checkpoint failure into an Unknown scene and putting a check into teardown that "must never perform this
   check itself" (:307-309, `new`'s rollback): recommend Z now, the trash-path checkpoint as a separate rm-followups
   round if the residual ever shows in a `last_rm`. r3 adds kept old trees to the recheck (§3.2) rather than freezing
   them, for the same reason.
9. **`--discard` is unchanged by the recheck** (r2). Under `--discard` a write after the checkpoint is lost from the
   checkpoints too, today and after r2; extending step 5.5 to discard (refuse, or record a second checkpoint without
   refusing) is a two-line change left to the rm-followups track because it changes what a Touch ID confirmed. The
   auto-close never discards, so this addendum does not need it.
10. **`remove_strict` as a separate entry point** rather than a flag on `remove` (r2): a `bool` next to `discard`
    invites `remove(id, true, token, …)` in the hook; a function with no token parameter cannot discard by
    construction, and the static test of §10 row 3 keeps `remove(` out of the hook module altogether.
11. **`'daemon'` on the wire and in the store** (r2, Claude r1 note): readers of the P1 CLI/GUI are loose (§4); the
    store reader of an older daemon is not. Alternative: write daemon marks as `'cli'` to avoid the new value; rejected
    because the mark's `by` is the only place a human can tell the daemon's mark from their own.
12. **Projection-only discharge, onto the newest publish that descends from the base** (r3, Codex r2 blocker 3).
    Alternatives weighed: (a) keep the content match but only against candidates newer than the base: still unsound
    when two newer publishes hold different contents of the same path (v1 at C1, v2 at C2, revert to v1 matches C1);
    (b) project onto origin's current tip: refuses a landed edit that another session changed afterwards, and every
    candidate is an ancestor of the tip anyway; (c) require the overlay to be empty (no discharge at all): refuses
    every `Deferred` session, i.e. the common agent flow within the quiet window. The chosen rule is the code's own
    second stage (:566-595) made the only stage; `newer` (:575-577) is what makes it immune to the revert.
13. **The judge builds no base** (r3). `project` needs a built base for the target tree; after a publish the rebase
    has built it before any deferral (rebase.rs:471), and `rm` has never built one (:941; `rm_t3_without_a_built_base
    _stays_dirty_and_builds_none`). Building one inside the hook would put a base build (an APFS clone plus a deps
    install) on the publish reply path and a repo lock take under the session queue, for a case (a gc'd base of an
    old publish) that `landing.why_none` names and the agent's `rm` still handles. Recommend as written.
14. **`busy` marks nothing, and `busy` is the code** (r3, Claude r2 nit 1; r4, Claude r3 nits 3 and 6). Alternative: a
    second bounded `with_sessions` take just for the mark; rejected because the other operation holding the session has
    its own outcome and because a mark written after the other operation's `rm` would hit `NoSuchSession` anyway
    (marks.rs:81-88). r4: the rationale is "nothing was judged", which also covers the two `Busy` cases met while the
    hook holds its queue place (`pins_tightened` at the fence, the du timeout); keying on two reason strings sent those
    to `error`, which then marked a transient as a refusal (r7: `error` no longer marks at all, item 44).
15. **The daemon overwrites only its own note** (r3, Claude r2 nit 3). The alternative, never overwriting, leaves the
    first refusal's reasons on a session whose situation changed; appending would hit the 2000-byte cap.
16. **The test fixture defaults to `auto_close = false`** (r3, Claude r2 nit 5), the §10 tests opt in. The product
    default stays `true` (autoclose-req.md). The audit of every `publish` call site in crates/strata-daemon/tests
    (§10 row 1) found six tests that publish a landed session and then use it (`external` in its workspace, a store
    write naming it, an `rm`, a tracker-map unwrap) and five more that stay open only because of one clause. The
    alternative, keeping the fixture at the product default and rewriting those tests, spends the change on tests
    that were not about closing and makes every future publish test think about it. The default-`true` path is
    exercised by row 12 (config) and by every row-1 test that opts in through the same key. r7: the CLI's hand-written `Live`
    fixture gets the same default (item 45).
17. **v7 amended in place** (r3, Claude r2 nit 8). Verified unmerged (main at 695f3cd); a v8 rebuild only if that
    changes before this lands.
18. **Withdrawn (r7, Codex r6 nit 1).** r4 held open a second discharge rule for a blocked entry whose content equals the target's.
    Such an entry is never blocked: `rebase_with` answers equal sides first (strata-overlay rebase.rs:549-550), so the rule could not
    fire and the example behind it was wrong. The false negative that does exist has differing sides and is the same state as the
    fresh revert; it stays refused (§3.1), and its cost is a done mark whose note names the agent's `rm`.
19. **Split-branch repos: prove against the default branch, mark `not_merged`, never close on the publish** (r4, Codex
    r3 blocker 1; Claude r3 nit 1). Alternatives weighed: (a) clause 0 answers `disabled` for any session with a
    split-branch repo (no mark, no judge): simple and sound, but it drops the requirement's fallback (mark done with
    the reason) for a supported configuration, and an untouched split sibling would block a same-branch repo's close
    for no reason; (b) keep `origin/<publish.branch>` as the anchor for split repos ("landed where the repo publishes"):
    D57's `rm` rule and what r3 did; it closes before main has the work (Codex's scene) and the requirement says main;
    (c) prove against L for every repo and name the cause by the branch pair (chosen): same-branch repos are unchanged,
    split-branch repos get a true answer and a note that says what to do, the proof costs one `is_ancestor` per
    commit, and a merged-and-fetched split repo even closes when something judges it. The push trigger and set (b)
    skip split-branch repos (item 20).
20. **The push trigger and set (b) skip split-branch repos** (r4). A push of `release` moves
    `refs/strata/remotes/origin/release` (push.rs:289-303) and fetches only that branch (:551-563); the proof reads
    `…/main`, so its input has not changed when the push runs, and a re-judge would run Phase A of the judge (holders
    scan, cold reconcile) for every done-marked session of the repo and answer as before. Alternative: re-judge anyway,
    so that a merged-and-fetched split session closes on a later push; rejected for the reason above. The agent's `rm`
    is the documented end of these sessions (§8).
21. **`closed` is the group's** (r4, Codex r3 nit 2). Alternative: keep the conjunction over every judged session and
    have the skill read `sessions[S]` always; rejected because `closed` would be `false` for a caller whose own session
    is gone, and because the single-session reply — the CLI's usual case — should answer the caller's question in one
    field. The re-judged sessions keep their truth under `sessions[<sid>]` and `rejudged`.
22. **The CLI checks `already_published` pushes** (r4, Codex r3 nit 1). A behaviour change outside the hook: a re-run
    after a refused push answers `ok:false PushRefused` instead of `ok:true`. Alternative: leave the envelope and rely
    on `closed: false` plus `push_refused`; rejected because an agent that branches on `ok` (the envelope's contract)
    would read a refused push as success, which r3 wrongly claimed the CLI already prevented. One test (§10 row 9).
23. **The audit lock is an in-process mutex per file on `Core`** (r4, Codex r3 blocker 2). Alternatives: an `flock`
    on the file (defends against a second process; there is none — the daemon is single-instance, startup.rs:273 —
    and `flock` across a `rename` needs care); reusing `Pins.inner` for the sessions file (couples session traffic to
    the pins state and holds the pins mutex across the hook's fsyncs); a per-session lock (wrong granularity: the file
    is shared). The mutex is held across the whole rotate/open/delimit/write/fsync sequence, is taken with no queue place or
    lock held (r6, §7), and nothing is taken under it.
24. **Evaluation order 0, 1, 4, 2, 3, 5, 6, 7 instead of renumbering** (r4). The clause numbers are what three rounds
    of reviews cite; renumbering would make the r1–r3 reviews unreadable against this file. Clause 4 reads git and the
    store only and its codes name the cause; `ahead` keeps `unchecked` and the diverged cases.
25. **The proof reads the publish rows and the pending row, not the pending row alone** (r5, Codex r4 blocker). Codex named
    the committed pending target as a starting point. Alone it passes after a `sync` consumed the row, after T2 discarded
    it, and after `RebaseTargetConflict` kept an older target (§2.1). The publish rows alone would miss a target a replay
    enrolled and a `main` target. The price of reading both is false negatives where a human dropped a commit on purpose or
    a squash lost the trailer: a done mark and a note naming the agent's `rm`. Recommend as written.
26. **Every `source` counts, `main` too** (r5). A `main` target holds no work of this session and could be exempted. Not
    adopted: the rule is that a close never drops a record for an unlanded commit, T2 would move the session onto that
    commit anyway and the base test would then refuse, so an exemption would make the answer depend on T2's timing. Only a
    `push = false` or refused chain can put a `main` target off L, and the next push re-judges.
27. **The agent's `rm` keeps dropping the row** (r5). `RemoveMode::Agent` runs neither step 5.5 (c) nor the guard. The row
    steers a workspace that is being removed, the chain ref holds the commit, and the agent's `rm` is an explicit act that
    §8 names as the way out for `on_chain: false` and `not_merged`. Making it refuse outstanding targets changes documented
    behaviour (D57, an `rm` on a split-branch repo) and was not asked for. **Flag for rm-followups:** `rm` could report the
    §2.1 items in its reply, as `base_rev_unreferenced` does (handlers.rs:1282-1294), without refusing.
28. **A replayed publish is matched by its trailer** (r5). Alternatives: (a) rewrite the publish rows at replay, which
    touches `push_replaying` and `chain_resolve` and the outcome's shape, and `chain --repo REPO --accept-origin replay` has no map to
    write (resolve.rs:643-652); (b) accept the false negative, so a `push = false` session whose chain replayed never closes,
    on the path the push trigger exists for. The trailer lookup already exists (pending.rs:191-195), costs a `merge_base` and
    one `log` for a commit off L only, and its limits (first parent, 500 commits, a pruned commit) fail towards refusing.
29. **A guard on `Removing`, not only a read** (r5). A read at step 5.5 leaves a window, until `Removing`, in which
    main-follow's new row would be dropped: harmless for a `main` target, but the requirement says never. The compare is one
    transaction and its failure is a plain refusal before anything visible. Alternative: hold every manifest repo's lock
    from the proof through `Removing`; rejected because no repo-lock wait may sit between the last holders scan and the
    retire (R4, remove.rs:362-365). r6 keeps the shape and closes what it left open (items 33 and 34).
30. **Set (b) and the `push` trigger are not narrowed by reason** (r5, Claude r4 nit 7). The alternative was to judge only
    sessions whose daemon note or `last_rm.detail` names `not_pushed` or `push_refused` on the repo. Not adopted: P1 stores
    reasons as free text (item 5); `not_pushed` runs no judge, so it writes no `last_rm`; an agent-marked session has no
    reason at all and must still close on a push (`push_judges_only_done_marked_sessions`); and a session whose holders have
    gone should close at the next push. The cost is stated in §9, clause 4 makes the common waiting case cheap, and P2's
    `reason` column can narrow it later. Item 20's split-branch skip is a different case: there the push cannot change the
    proof's input.
31. **`gone` reads as closed for a group session** (r5, Claude r4 nit 3). Alternative: keep `closed: false, reasons:
    ["gone"]` and teach the skill; rejected because the field answers "is my session gone?", and the group's `closed` would
    be `false` with nothing left to do. The audit line keeps what this judge did.
32. **`chain --restore` is not a trigger** (r5, open). It pushes the chain (resolve.rs:581-637), so a `not_pushed` session
    could close after it. The requirement names publish and push, a restore is the human's recovery, and the note's fallback
    sentence covers it. A later round can add it next to the two `push` forms.
33. **Target writers check the lifecycle in their own transactions** (r6, Codex r5 blocker 1). Alternatives: (a) hold every manifest
    repo's lock from the proof to `Removed`: rejected in item 29, and a stale writer already holding one would stall the close; (b)
    prove the rows again just before `Removed`: by then the root is in the trash, so a failure cannot refuse anything; (c) keep any row
    a stale writer adds: `Removed` would have to skip a delete, leaving a row with no session to steer. The chosen check adds one read
    to five store methods and no state. **`active` for the main-follow writers, "not `removing` or `removed`" for the replay ones**,
    rather than Codex's `active` for all: `session_heads` and `reconstruct` deliberately serve `Creating` and `Unknown` sessions
    (resolve.rs:877-885; pending.rs:202), and a strict close only ever leaves `Active` through `removing`, so the narrower rule closes
    the hole without changing what a `Creating` session gets.
34. **The whole of clause 6's store half is in `Removing`'s transaction** (r6, Codex r5 blocker 2). Codex named proposals. `conflicts` is
    added because T2's gates write it under no lock at all, and the requirement lists it next to proposals; reading both costs little
    and leaves nothing for a later round to find (r7: `assigned`, added to the clause later, is the exception, item 41). Alternatives: a second read at step 5.5 only moves the window; keys for proposals, as
    for rows, would need a rule for which changes matter, where the clause itself already has one. **It refuses a harmless case too:** a
    proposal whose target S would never have used still holds S open, as clause 6 does (item 3). Conservative by the requirement's text.
35. **A refusal from the transaction reports clause 6's code** (r6), not `changed_during_remove`. That code's skill row says to wait and
    run `rm`, which is the wrong advice for a proposal, and the outcome should not depend on which side of the read the writer landed.
36. **`Refused` is refreshed in the reply, not in the row** (r6, Claude r5 nit 1). Rewriting the row on a later push means finding every
    row a push lands, on every push path, for a value only a re-run reads. The row stays the record of what that publish's own push did.
37. **No copy rule for `base` or `pending`, and none by subject** (r6, Claude r5 nits 2 and 8). Each would turn a done mark into a close on
    weaker evidence. The cost is a done mark whose note names the agent's `rm`.
38. **The Stop hook's wording changes by the mark, and only by the mark** (r6, Claude r5 nit 3). An agent's own `mark done` gets the shorter
    sentence as well: it has said it is finished, so "re-run checks, then publish" is wrong for it too, and `mark active` restores the old
    words. The P1 hook's Tracker and `behind` logic are untouched.
39. **Clause 0 is decided under the queue place, and the early read stays as an exit** (r7, Codex r6 blocker, Claude r6 blocker 1).
    Alternatives: (a) drop the early read: every opted-out candidate would then wait for its queue place and could answer `busy`, with
    an audit line, for a session the hook will never judge; (b) make `add` refuse while a hook is pending: a new flag, a change to `add`'s
    contract, and no gain over reading under the place `add` already needs. Rerunning the clause costs one map lookup per repo.
40. **A nested repository refuses a strict close** (r7, Claude r6 nit 1). The alternative was to only name the case and let the close take it
    to the trash, as an agent's `rm` does. Not adopted: a nested clone can hold the only copy of unpushed commits, gc deletes it after
    14 days, and here nobody chose that. The refusal is one filter over warnings the cold scan already produces. Its false negative, a
    scratch clone the agent made on purpose, costs a done mark whose note names the agent's `rm`. **Flag for rm-followups:** an agent's
    `rm` could report these paths in its reply.
41. **`assigned` is read at clause 6 only, and r8 makes that read final** (r7, Claude r6 nit 2; r8, Codex r7 blocker; §3.2). r7 accepted a late `assign` as bounded, and Codex
    showed that it breaks the requirement's "no conflicts". Reading it in `Removing`'s transaction stays rejected: that moves `assignments` below strata-publish and parses every publish row under
    the write lock. r7 also rejected a lifecycle check atomic with `assign`'s write, and rightly: it would leave the window between clause 6 and `Removing` open. Item 46 is the answer. **Flag for
    rm-followups, unchanged:** an agent's `rm` reads no assignment, so one made before it still strands the path; `stage` could ignore an assignee that is `Removed`
    (stage.rs:529-531). That changes a refusal for every session, so it is not done here.
42. **"No kept old trees" is read literally for the close, and the agent's `rm` keeps its rule** (r7, Claude r6 nit 6; §3.1). The alternative,
    r6's pass for an unchanged tree, loses no walked content, but it reads the requirement loosely and lets a tree holding uncarried ignored files go
    unattended. The refusal is one `||` in a loop that already runs, and its false negative costs a done mark whose note names the agent's `rm`.
43. **A dev server that has left the workspace is not detected** (r7, Claude r6 nit 7). `session exec` starts it with its cwd in the workspace
    (devports.rs:641), so the holder scan sees it unless it changes directory and holds no file or mapping inside. Such a server survives the close and its
    lease goes with `Removed` (sessions.rs:676-678), as after an `rm` today. The only port probe is the CLI's, `lsof` per port plus `bind_probe` (devports.rs:250-279).
    Alternatives: move it to the daemon, which spawns subprocesses per port on the publish reply path, or probe by binding, which can fail a server starting at that
    instant; either to catch a process that holds nothing of the session. Not adopted: no work is at stake, and the next session to lease the port is told it is
    `foreign` (devports.rs:300-317). The requirement's "no live holders/dev processes" is met for every process that holds anything inside the session; **whether that is enough is open for its owner (r10, item 58).**
44. **A mark records a refusal, so `error` and `upstream_unavailable` write none** (r7, Claude r6 nit 5; §4). The review named `error`; `upstream_unavailable` is
    the same failed read under its own code, so it follows. The price is that the reasons live in the reply and the audit line only, which §8's
    "these reasons are in this reply only: act on them now" already says.
45. **The CLI's live fixture defaults to off as well** (r7, Claude r6 nit 9; §5). The alternative, auditing confirm_gate.rs, publish_from.rs, cli.rs and r2.rs and
    rewriting what breaks, has item 16's cost for tests that are not about closing. One opted-in CLI test covers the product default (§10 row 9).
46. **`assign` takes the assignee's queue place before the repo lock** (r8, Codex r7 blocker; §3.2). One wrap in the handler and one more refused lifecycle, with no new state. Alternative: have
    `stage` ignore a `Removed` assignee. That also fixes an agent's `rm`, but it changes publish's refusal for every session, and `closed: true` would still leave a dead
    assignment on the row. The price is a changed contract (docs/modules/daemon.md:483) and an unbounded repo take under the place, for a rare human command.
47. **A session that grew during the run is neither closed nor marked, and `add` clears only the daemon's mark** (r8, Claude r7 nit 4; §1, §4). The nit offered "clear the mark, or record it"; both rules are here
    because each covers a path the other does not. Alternatives: (a) record it only: the agent's next command answers `NoSuchSession` on a session it just extended; (b) mark done with `extended`: it
    puts the session back in line for the next push; (c) read the baseline at step 1: a long publish's whole run goes uncovered; (d) clear every kind of mark: it deletes a note someone wrote. Accepted: a
    clean repo added before the baseline is part of what the publish judges, and an agent that extends a session it marked done itself runs `mark active` (§8). In both, a close loses nothing, because any edit in the new
    workspace refuses it.
48. **A `false` in either catalog refuses, and a `true` needs both** (r8, Claude r7 nit 3; §5). Using only the current catalog would let a reload turn a close on inside a request pinned to `false`, which breaks the
    rule that a newer config may only refuse more. No lock or new state: `current()` is an atomic load.
49. **A failed read is an error even when it arrives as `Dirty`** (r8, Claude r7 nit 2; §4). Alternative: make strict `dirty_check` return `Err` on those reads. That loses what the other repos said, and the mark for a
    real refusal beside it. The hook's mapping is a pure function and is tested as one (§10 row 21).
50. **`NoSuchSession` goes ahead of the TTY gate, keyed on the code** (r8, Claude r7 nit 6; §6). It also changes a wrong selector on a non-TTY with a guarded repo in the config from `NotTty` to `NoSuchSession`, which is the truer
    answer, and nothing is sent either way. A check for a `Removed` row would need a second store read and would answer differently for a gc'd session. `tty_refused` (confirm.rs:718-747) is left alone, so a session that closes
    between the batch and the plan still reads `NotTty` with `plan_error`, which names the real failure.
51. **`assigned` reads every repo of the publish rows, not the manifest** (r8, own finding; §3.2). `assign` does not require the assignee to hold the repo (handlers.rs:1227-1229), so a manifest read misses a real stranding.
    Making `assign` refuse such a session would be simpler to read but would change a command humans already use.
52. **A session's own proposals are read with no tracked filter, and the way out of a draft is the agent's `rm`** (r9, Codex r8 blocker; §3.2). Alternatives: (a) make `deps propose` refuse an untracked repo: it changes a command that works
    today, drafts already in a store would still escape, and a tracked repo's abandoned draft would have the same exit; (b) add `deps cancel`: a new command and a new schema edge (`awaiting_publish` may become only `pending` or `superseded`, schema.rs:364) for one false negative; (c) supersede
    the draft as `Removed` commits: that closes the session and silently drops the record the requirement says must hold it open. The chosen rule is one query in two places, and its false negative costs a done mark whose note names the agent's `rm`.
53. **A pending proposal of another origin on an untracked sibling does not hold the session** (r9, stating r1's scope on purpose). It stays visible and approvable after S goes (deps.rs:571), and a rollout skips S's untracked workspace (rollout.rs:349-351), so the close neither strands nor affects it, and
    the requirement's "no pending deps proposals" is met for everything the session owns or would be moved by. Holding S would keep every session with an untracked sibling open until a human approves a change that does not concern it. Dropping `w.tracked = 1` from line 3 (a) and reading clause 6 (a) over every repo is the change if the owner reads the sentence more widely.
54. **A saved `sync` report refuses, stale or not** (r9, Claude r8 nit 1). The review rates the close a nit because nothing is lost, and offers the strict reading of "no conflicts", which is the conservative one. Alternative: compare the report's `target` and `generation` with the workspace and ignore a stale one: a rule for which reports
    matter, where a refusal costs a done mark. It is also read in the transaction, though its writer needs the queue place, so that line 3 stays "clause 6 less `assigned`" (item 34's reasoning).
55. **A failed Phase B projection is an `Err`; a lost base is a refusal** (r9, Claude r8 nit 2; §3.1, §4). Alternative: keep the line and have the hook recognise the case, as item 49 does for Phase A: Phase B has no `projection_errors` map to read, and the checkpoint has already been taken, so nothing is lost by answering early.
56. **Open, for the requirement owner (r9, Claude r8 nit 8): the cases that write no mark.** autoclose-req.md:4 says "Otherwise only auto `mark done` with the reason", and `busy`, `error`, `upstream_unavailable` and `extended` write none. (`disabled` is the configured opt-out; `gone`, `lifecycle` and `unknown` have no session in a state to mark.) Recommend keeping it, and implementation need not wait: a mark
    records a refusal, nothing was judged in these cases, a done mark would put the session in line for later closes (§8), and a `busy` from the queue take holds no place to write under (§1). The reasons reach the reply and the audit line. For a literal reading, the cases that hold the place can mark with one arm each in §4; a `busy` from the queue take cannot, short of the second take item 14 rejected.
57. **A late draft is refused in its own insert, and only a draft** (r10, Codex r9 blocker; §3.2). Alternatives: (a) refuse every session-origin insert, Codex's wording: it breaks `same_to` and `ensure_target`, so `observe_main` and `deps approve` fail once a proposer has gone, for a row `Removed` does not hide; (b) have `deps_propose` take S's queue place around the insert, as `assign` does (item 46): `deps propose` would answer `Busy` whenever S is busy and would hold the place through an unbounded repo take, to get what one read in the transaction gives; (c) re-read drafts just before `Removed`: by then the root is in the trash, so a failure can refuse nothing; (d) supersede a late draft as `Removed` commits: it silently drops what the writer was told it has; (e) a `BEFORE INSERT` trigger: a schema change for a rule one method can hold. **No row stays as today**, on purpose: making it refuse means giving the store's tests session rows, to cover a writer that outlives `Removed` by 14 days and leaves a row nothing lists.
58. **Open, for the requirement owner (r10, Claude r9 nit 1): "no live holders/dev processes".** autoclose-req.md:3 lists dev processes, and item 43 narrows that to processes holding something inside the session. A server that changed directory and holds nothing survives the close: `dev_ports` stores `session`, `repo` and `base`, and no pid (strata-store devports.rs:27-31). **Default: item 43's reading**, because no work is at stake and the next lease of that port reports `foreign`. A literal reading is one more refusal, a probe of the session's leased ports before the judge, at item 43's cost. It only refuses more, so it can be added after implementation.
59. **Open, for the requirement owner (r10, Claude r9 nit 1): what goes to the trash with no judge.** Ignored paths, deps directories, `.git`, a nested repository under an ignored directory and anything directly in the session root leave with the root (§3.1). Today's `rm` does the same and they stay recoverable until trash gc (`GC_KEEP` 14 d; `rm.trash` names the place), but now nobody chose to remove them. **Default: keep it**, because "fully landed" can only speak of what a publish could land, and a refusal on ignored files would hold most sessions with build output open. A refusal needs a new walk, because the overlay's never enters them (scan.rs:217-226); a report of the top-level ignored paths in the reply is the cheaper way to say more. Either is additive.
60. **Open, for the requirement owner (r10, Claude r9 nit 1): `closed: true` in the GUI.** autoclose-req.md:3-4 says "reply field `closed: true` (CLI and GUI)", and the desktop backend has no `publish` or `push` command, so no GUI receives that reply (apps/desktop/src-tauri/src/lib.rs:148-158). **Default: the CLI's reply carries the field, and the GUI shows the outcome:** a closed session leaves the list at the next `status{summary}`, and one left open shows the `Done` badge with the daemon's note (§6). A later GUI `publish` command would carry the same reply with no change to the hook. A notice of closes the GUI did not start would be a new surface, so it is not proposed.

## 12. Changelog r1 → r2

| Item | Review | Change |
|---|---|---|
| 1 | Codex blocker 1, Claude blocker 1: the confirmed `push` hook sat inside `core.locks.with_repo` (today's :2225-2260); `remove`'s unpin re-takes the repo lock through `with_cleanup_lock` (remove.rs:424 → lib.rs:305-307 → locks.rs:435-442 `reentrant`), an unbounded retry after the retire: a daemon hang with the row `Removing`. | §1 `push` row rewritten: both forms run the hook after the lock closure returns (the confirmed closure returns `(PushOutcome, tip)`, the JSON is built after); the lock-order paragraph added; §9 gains the "repo lock and the hook" row; §10 row 2 adds the confirmed-form and headless-form handler tests, the harness timeout being the deadlock detector, and cites `reentrant_repo_take_is_refused_not_deadlocked`. |
| 2 | Codex blocker 2: a non-discard `rm` lets unpublished `rm.regenerable` entries go (remove.rs:622-633), so the r1 judge could close a session with an unlanded edit and report `unpublished: false`. | §3.1 `Sessions::remove_strict` (one private `remove_with(id, RemoveMode)`; strict = no exemption at :622-633, :568-570, :222-224, :258; `detail.regenerable` names the subset); §2 clause 7 and its reason codes; §6 (`rm.regenerable` always empty); §7 (`rm.mode`); §8 skill wording for the regenerable subset; §10 rows 1, 3, 6; §11 items 7 and 10. |
| 3 | Codex blocker 3: a write finished after a repo's BeforeRemove checkpoint (:218) while another repo is checkpointed escapes the first recheck and the holders scan (:305) and leaves with the root into the trash. | §3.2 Phase B step 5.5 (post-scan recheck: cold `compute` per repo compared with its checkpoint's entries, empty for untracked repos, `Unstable` counts, `changed_during_remove after: checkpoint`) between the R4 scan and a repeated R4 scan (step 5.6); seam `remove.after_recheck`; the exact residual stated; §9 race row rewritten; §10 row 7; §11 items 8 and 9. This changes every non-discard `rm`, agents' included. |
| 4 | Claude note: `published[R].push` cannot be `Refused`/`Unknown` on the success path (push.rs:666-683); only `already_published[R].push` can carry `Refused` (:416-430, :2539). | §2 clause 5 and its code (`push_refused` only), §6 CLI bullet, §10 rows 1 and 9 reworded for `already_published`. |
| 5 | Claude note: the `push` row's "after main-follow ran" described only the headless form; the confirmed form runs no rebase or main-follow, and r1's push test (fixture `confirm: false`) could not catch blocker 1. | §1 `push` row names both forms and what the confirmed form does not run; §10 row 2 (confirmed-form test with the scripted confirmer and a `confirm: true, push: false` pin). |
| 6 | Claude note: clause 4 must iterate the manifest's repos, not `upstream.repos` (upstream.rs:329-337 drops unhealthy and scene-live workspaces). | §2 clause 4 reworded; clause 3 says what `upstream.repos` holds and who refuses the rest. (r3 widens it again to untracked repos, §13.) |
| 7 | Claude note: `by.kind: "daemon"` on the wire vs a P1 CLI/GUI without the variant. | Checked against the worktree: the CLI (cmds.rs:445) and the GUI (view.ts:441-442, types.ts:156) are loose at runtime; the strict reader is an older daemon's store `mark()` (marks.rs:46). §4 states both, adds `'daemon'` to the TS union, and the downgrade note goes to the docs (§8); §10 row 13; §11 item 11. |
| 8 | Own correction. | The principle's "deletes no ref" was wrong: every `rm` deletes the session's pin last (:421-430); reworded, with clause 4 as the reason it is redundant by then. |
| 9 | Own correction. | The `mark.rs` line ranges of the worktree were off by two (`MarkByKind` :17-23, `validate_mark_note` :41-53); `marks.rs` set_mark :61-112 (lifecycle check :81-88, upsert :99-104); the `push` `Unknown` row range :877-891. |

## 13. r3 changes

| Item | Review | Change |
|---|---|---|
| 1 | Codex r2 blocker 1: clause 4 checked tracked repos only; an untracked sibling gets its base from `main_tip` at `new` (new/mod.rs:148-153, :573; the chain top under `pin_unpushed`, proposal.rs:386-396), passes `dirty_check` on an empty overlay (remove.rs:560-561, :634-639), is absent from `upstream.repos` (upstream.rs:338-340), and its pin goes with the close (:421-430). | §2 clause 4: every manifest repo, `tracked` in the detail; the principle; §10 row 1 (`publish_marks_done_not_pushed_for_an_untracked_sibling_on_an_unpushed_chain`), row 3 (unit); §11 item 2. |
| 2 | Codex r2 blocker 2: a write into a kept pre-move tree after Phase A's `old_tree_unchanged` (:641-657) was seen by no later step; old trees live in the session root (move_workspace.rs:525-526) and are retired with it (:398-416). | §3.2 step 5.5 part (b): `old_tree_unchanged` again for every existing done-move tree, failing closed; `detail.old_trees`; step 5.6 covers them; §9 row rewritten; §10 row 7 (three old-tree tests, strict variants, the holder test inside an old tree); the guarantee sentence names kept trees. |
| 3 | Codex r2 blocker 3: `split_landed` matches an entry against any origin-reachable publish of the session (:1074-1101), so a fresh revert to an older publish's content was let go (:1090-1092, :222-223, :1130) and the r2 judge closed with `unpublished: false` while F1's live overlay was non-empty. | §3.1 "current-target landing proof": in strict mode the only discharge is the T3 projection (:943-975) onto the newest origin-reachable publish that strictly descends from the base (`newer`, :575-577); `recheck` discharges no new entry historically; `detail.landing` (target, `why_none`, `historical`); the built-base argument (rebase.rs:471 before :504-512); §2 clause 7; §8 wording; §9 row; §10 rows 1 and 6; §11 items 7, 12, 13. |
| 4 | Codex r2 nit 1 (config defaults). | §5: `#[serde(default = "PublishDefaults::default_auto_close")]` on the inner field; §10 row 12 tests an absent table, an empty table and an explicit `false` round trip. |
| 5 | Codex r2 nit 2 (the unconditional late-write claim). | The principle now states the §3.2 guarantee verbatim and nothing stronger. |
| 6 | Claude r2 nit 1 (`busy` has no queue place). | §4: `busy` joins the no-mark list; reply `mark.skipped: "busy"`; audit line still written; §9 row 1; §10 row 1; §11 item 14. |
| 7 | Claude r2 nit 2 (bounded wording). | Checked and not adopted as stated: `is_visible()` is set only by `new`/`add` (handlers.rs:1477, :1502), never by publish or push; `writing()` is not visibility. §1's new paragraph states exactly what bounds the wait (resolve.rs:64-72, :119-122; locks.rs:499-501, :536-542). |
| 8 | Claude r2 nit 3 (stale daemon note). | §4 note ownership: a `daemon` row is refreshed, agent/cli/gui rows kept; §10 row 1; §11 item 15. |
| 9 | Claude r2 nit 4 (`not_pushed` stranded). | §1: a publish that pushes R re-judges the done-marked sessions holding R (the push trigger's set); §8 skill wording with the `rm --session S` fallback; §9 row; §10 row 1 (`publish_rejudges_done_marked_sessions_when_it_pushes_their_repo`); §11 item 1. |
| 10 | Claude r2 nit 5 (fixture default). | Audited every `publish` site in crates/strata-daemon/tests: six tests would break under a default-`true` fixture (named in §10 row 1 with the first failing call), five stay open only by one clause. §10 row 1: the typed fixture (`Config` literal, support/mod.rs:377) gains `PublishDefaults { auto_close: false }`, the auto-close tests opt in before `prepare`; §5 lists the struct literals the new fields touch; §11 item 16. |
| 11 | Claude r2 nit 6 (guarantee overstated for unclassified paths). | §3.2 and the principle: "every write that `compute`'s walk classifies, or that lands in a kept old tree"; ignored paths, deps directories and `.git` named as outside it. |
| 12 | Claude r2 nit 7 (`classify::changed` visibility; compare against `cp.checkpoint.entries`). | §3.2 step 5.5: `pub(crate)` at classify.rs:95 → `pub` or a re-export; the comparison target named with checkpoint.rs:388-398, :407. |
| 13 | Claude r2 nit 8 (v7 in place vs v8). | §4: amended in place, with the verification that the branch is unmerged and the developer-store case made loud (reply, audit, log); §11 item 17; §10 row 5. |
| 14 | Claude r2 nit 9 (line drift). | Every `handlers.rs` citation re-anchored to 8d9663c (`publish` :2031-2143, `push` :2204-2261, `push_headless` :2271-2339, `Rm` :1241-1298, `upstream_best_effort` :961-980, `live` :339-371, `with_du_hold` :1907-1911, `landed_before` :2525-2542, `mark` :1126-1141); `dirty_message` is :845-934 (r2 pointed at its unit test); `resolve_in`/`select_in` :52-77/:79-89; the intro says what is cited. |

## 14. r4 changes

| Item | Review | Change |
|---|---|---|
| 1 | Codex r3 blocker 1 (split `publish.branch` / `default_branch`): clause 4 and the strict judge's candidates used `origin/<publish.branch>` (remove.rs:563 → :981); a release-only publish with a deferred rebase passed clause 4 and the projection and closed before main had the commit. | §2 clause 4: `L = refs/strata/remotes/origin/<default_branch>` for the base and the published commits; codes `not_pushed` (same branch) / `not_merged` (split), `what`/`head`/`commit`/`ref`; the lineage facts (proposal.rs:386-396, new/mod.rs:573 and :581-583, gate.rs:60, push.rs:858, chain.rs:213-223) and L's refresh paths (periodic_fetch.rs:48, sync_move.rs:205-208, gate.rs:73-79, push.rs:551-563); evaluation order 0, 1, 4, 2, 3, 5, 6, 7 (clause 3's note); §3.1 anchor paragraph: strict passes `cfg.default_branch` at :563 and :249-259/:1118, `landing.anchor`, `why_none: "no_landed_publish"`; §1: the push trigger and set (b) skip split repos; §4 note example; §5 split-branch paragraph; §8 `not_merged` wording and the agent's `rm`; §9 row; §10 rows 1 (four variants), 2, 3, 6; §11 items 2, 19, 20, 24; the principle and the intro. |
| 2 | Codex r3 blocker 2 (audit writers): `audit::append` (audit.rs:95-128) has no lock; today only the `Pins` mutex (confirm_flow.rs:257-262) serialises it; concurrent hooks on request threads (startup.rs:717) could double-rotate, overwrite `.1` or interleave. | §7 lock paragraph: `AuditFile { path, rotated, rotate_bytes, lock: Mutex<()> }`, `Core.sessions_audit` (startup.rs:243-263, built at :305-307), one lock per file across rotate/open/delimit/write/fsync, the pins file keeps `Pins.inner`; §9 row; §10 row 4 (seam-made-deterministic unit test, daemon-level concurrent test); §11 item 23. |
| 3 | Codex r3 nit 1 (the refused-`already_published` CLI claim): `publish_envelope` loops `published` only (confirm.rs:861) and passes `already` through unread (:899). | §2 clause 5 corrected; the CLI gains the `already` push check (`Refused` → `PushRefused`); §6 CLI bullet; §10 row 9; §11 item 22. |
| 4 | Codex r3 nit 2 (aggregate `closed`): re-judged sessions entered `closed`, so a removed caller could read `false`. | §6: `closed` = the group's; `auto_close.rejudged`; `in_group` on every entry; §8 skill text branches on `sessions[<sid>]` and handles `disabled`, `busy` and `mark.written: false` explicitly; §2 clause 0 says the reply carries `disabled`; §9 row; §10 rows 1 (`closed_reports_the_group_only`) and 9; §11 item 21. |
| 5 | Codex r3 nit 3 (`behind` after a deferred publish): `behind` is positive for every session that stayed on its old base (upstream.rs:135-141) and `block_reason` then speaks (sync.rs:155-185). | §8 second bullet rewritten: silent for `Moved` only; a `Deferred` session left open gets the "main moved … sync" advice, which is right and never contradicts the mark; §10 rows 1 (`publish_marks_done_open_files_for_a_deferred_session`) and 10. |
| 6 | Claude r3 nit 1 (split-branch repos never close and get a misleading note). | Folded into item 1: `not_merged` with its own note, never "push REPO"; §5 paragraph. |
| 7 | Claude r3 nit 2 (set (b) misses pushes recorded as `already_published`). | §1 set (b) keys on `already_published[R].push == Pushed` too (handlers.rs:2115-2126, :2539); §9 row; §10 row 1 (`publish_rejudges_sessions_whose_repo_an_earlier_run_pushed`). |
| 8 | Claude r3 nit 3 (`busy` keyed on two reasons; `pins_tightened` went to `error`). | §2 clause 7 and §4: every `Code::Busy` is `busy` with `detail.reason` (`publishing`, `lock_wait`, `pins_tightened` at the take and at the fence, `du_cancel_timeout`); §9 rows; §10 row 1 (`publish_busy_by_code_marks_nothing`); §11 item 14. |
| 9 | Claude r3 nit 4 (`no_built_base` reported as `unpublished` with "publish" advice). | §3.1 and §8: `landing.why_none` branches (`no_built_base`, `no_landed_publish`) → the agent's own `rm`; §4 note example; §10 row 1 (`publish_marks_done_no_built_base_names_rm`). |
| 10 | Claude r3 nit 5 (step 5.5 (a) did not cover untracked repos, skipped at remove.rs:192-194). | §3.2 step 5.5 (a): `unhealthy()` read by step 5.5 itself for untracked repos, `Some` is a change; §10 row 7. |
| 11 | Claude r3 nit 6 (`busy` rationale wrong for the du case, which holds the queue place). | §4 and §11 item 14: "nothing was judged" replaces "holds no queue place"; §9 du row. |
| 12 | Claude r3 nit 7 (the projection refuses an edit another session changed between two own publishes). | §3.1 "false negative" paragraph; §11 item 18 (content match against the target only, as a later round). Withdrawn in r7 (§17 item 3). |
| 13 | Claude r3 nit 8 (F1's revert-to-base asymmetry on a `Deferred` session). | §2 clause 7's "why" column, one sentence. |
| 14 | Claude r3 nit 9 (strict mode ran the 32-candidate content match on every judgement). | §3.1 bullet: `candidates` first, projection, content match over the remainder only, `historical` omitted when nothing remains; §10 row 6. |
| 15 | Claude r3 nit 10 (`PublishDefaults` without `deny_unknown_fields`). | §5 global row; §10 row 12. |

## 15. r5 changes

| Item | Review | Change |
|---|---|---|
| 1 | Codex r4 blocker: an earlier unlanded publish (deferred `push = false`, release-only, set (b) re-judges) escaped clause 4, which tested this call's commits only; `Removed` then deleted its row (sessions.rs:682). | §2 clause 4 and new §2.1: `outstanding` proves the base, every publish row of the session (`Done`, `Unknown`) and the `rebase_pending` row (`head` of any `source`, `planned_head`, `planned_publish`) for every manifest repo against `origin/<default_branch>`, independent of the trigger and the overlay. §3.2: step 5.5 (c) and the guard on `Removing`, so a strict close deletes only rows it proved. §9 rows; §10 rows 3, 15, 17; §11 items 25 to 29. |
| 2 | Own finding while proving it: the row alone can vanish (`sync` consumes it, T2 discards it, `RebaseTargetConflict` keeps an older one), and a replay leaves the publish row naming a commit that never lands. | §2.1: the publish rows are part of the proof; a replayed copy is matched by its `Strata-Publish` trailer; §10 row 15 (`a_record_sync_consumed_does_not_hide_an_unlanded_publish`). |
| 3 | Codex r4 nit 1 (stale enumeration). | §1 per-session sequence: the done mark is reread under each queue place; §9 row; §10 row 16 (enumeration → unmark → queue). |
| 4 | Codex r4 nit 2 (§10 row 1 setup). | A refused first publish writes no mark: the test re-runs it first. Repos run in sorted order: `publish_rejudges_sessions_whose_repo_an_earlier_run_pushed` swaps the roles, and drops r4's `push: false` repo, which could not have pushed. |
| 5 | Claude r4 nit 1 (`not_pushed` assumes a leading chain). | §2.1 `on_chain` and `chain`, three note sentences; §4, §8; §10 rows 3, 15. |
| 6 | Claude r4 nit 2 (commits of another session's batch). | The proof reads own rows only; clause 5 is restricted to own batches; §10 row 15. |
| 7 | Claude r4 nit 3 (`gone` on an own session). | §6: reads as closed, `by: "other"`; §7 audit line; §9 row; §10 row 16; §11 item 31. |
| 8 | Claude r4 nit 4 (`AuditFile.lock` poisoning). | §7; §10 row 4. |
| 9 | Claude r4 nit 5 (`Default`). | §5: hand-written; §10 row 12. |
| 10 | Claude r4 nit 6 (cold Tracker). | §8 and §10 row 1 corrected. |
| 11 | Claude r4 nit 7 (set (b) cost). | Stated in §9; not narrowed, §11 item 30. |
| 12 | Claude r4 nit 8 (`disabled` in the push conjunction). | §6; §10 row 2. |
| 13 | Claude r4 nit 9 (a `fix` on the CLI refusal). | §2 clause 5; §6; §10 row 9. |
| 14 | Claude r4 nit 10 (line drift). | `validate_mark_note`, `pins_path`, `FEATURES`, `Pins::load` re-anchored. |
| 15 | Own corrections. | `no_landed_publish` no longer sends a never-published edit to `rm` (§3.1, §4, §8, `landing.published`). A refused publish builds no base, so its re-judge can answer `no_built_base` (§3.1; §10 row 1 fixes the session order and tests both). `reasons` for a refused `already_published` is `["not_pushed", "push_refused"]` (§2 clause 5, §10 rows 1, 9). The judge runs only when the cheap clauses passed (§2). Clause 1 reads under the queue place (§1, §2). |

## 16. r6 changes

The requirement in impl/autoclose-req.md is unchanged. Every r6 change refuses more or corrects a claim, with one exception: under item 5 a
push that really landed stops being reported as refused, so a session r5 held open on a stale `push_refused` can now close, still only
if clause 4 proves its commits on `origin/<default_branch>`.

| Item | Review | Change |
|---|---|---|
| 1 | **Codex r5 blocker 1** (also Claude r5 nit 7): a stale main-follow writer enrolled or retargeted after `Removing`, and `Removed` deleted the unproved row (main_follow.rs:169, :177; rm_state.rs:618, :636; sessions.rs:682). | §3.2: every target writer without the queue reads the lifecycle in its own transaction. `enroll_main` and `retarget_pending` need `active`; `plan_replay`, `settle_planned` and `promote_pending` refuse `removing` and `removed`. Tests: §10 row 18. §11 item 33. |
| 2 | **Codex r5 blocker 2**: a proposal created during removal escaped clause 6 (proposal.rs:415, :487). | §3.2: `set_removing_if_unchanged` reads clause 6's store half in the transaction that commits `Removing`, and a refusal carries clause 6's code. It also covers an own draft (handlers.rs:1763-1777) and a T2 `conflicts` deferral (rebase.rs:504-512), both found while proving this. Tests: §10 row 18. §11 items 34, 35. |
| 3 | Codex r5 nit 1. | §10 row 1: the historical-revert test sets up with auto-close off and turns it on by reload before the last publish. |
| 4 | Codex r5 nit 2. | §1 step 6, §7, §11 item 23: the audit line is written after the queue place is released. §10 row 4. |
| 5 | Claude r5 nit 1. | §6: `refresh_refused` at both places an entry is built (`landed_before` too, the path the scene takes). §2 clause 5; §10 rows 9, 19; §11 item 36. |
| 6 | Claude r5 nit 2. | §2.1: `landed_copy` needs the `sessions` trailer; the subject fallback and the shared `find_publish` are dropped from it. §10 row 17. |
| 7 | Claude r5 nit 3. | §8: the hook's real sentences; `block_reason` gains `done`. §10 rows 1, 10; §11 item 38. |
| 8 | Claude r5 nits 4, 5. | §1: three ways out of step 2, the hook's own `catch_unwind`, no mark outside the closure. §4; §10 row 19. |
| 9 | Claude r5 nit 6. | §2.1: `Ancestry` maps `NoSuchCommit`. §10 row 3. |
| 10 | Claude r5 nit 8. | §2.1: two more flow rows, the false negatives stated. §10 row 15; §11 item 37. |
| 11 | Claude r5 nit 9. | §4: an unchanged note is not written, and the note names no trigger. §6; §8's skill sentence; §10 row 19. |
| 12 | Claude r5 nit 10. | §5, §2 clause 0, §9: `core.snapshot()?.config`. §10 row 12. |
| 13 | Claude r5 nit 11. | §3.1: `Torn`, `RemoveReport.trash`. §6, §7. |

## 17. r7 changes

The requirement in impl/autoclose-req.md is unchanged. Every r7 change refuses more, writes fewer marks or corrects a claim; nothing makes a
close easier. `Agent`'s `rm` changes in no refusal.

| Item | Review | Change |
|---|---|---|
| 1 | **Codex r6 blocker, Claude r6 blocker 1**: clause 0 ran before the queue place, so a concurrent `add` of an opted-out repo was removed with the rest (handlers.rs:1494-1499; new/add.rs:163-168; remove.rs:384-430). | §1 step 3: clause 0 again over the reread manifest under the place, which `add` also needs, so it is final; step 1 stays as an early exit. §2, §5, §9 row. Tests: §10 row 20. §11 item 39. |
| 2 | Own findings while proving it. | The `add.build` order Claude sketched cannot run (an `add` holding the queue blocks the publish before the hook, push.rs:770-778), so row 20 pauses at `auto_close.before_judge`, now placed in §1. §6: a `push` with only `disabled` candidates omits the top-level `closed`, so an empty conjunction never reads as a close. |
| 3 | Codex r6 nit 1. | §3.1: r4's false-negative example and its proposed discharge rule are withdrawn (rebase.rs:549-550 answers equal sides first); the real one, with differing sides, is stated. §11 item 18. §10 row 20. |
| 4 | Codex r6 nit 2. | `chain --repo REPO --accept-origin replay` everywhere (args.rs:256-257). §2.1, §8. §10 row 14 parses every command the notes name. |
| 5 | Claude r6 nit 1. | §3.1: a nested repository refuses a strict close, `embedded_repo`; step 5.5 (a) counts the warning; what still goes to the trash unjudged is named. §4, §8, §10 row 20, §11 item 40. |
| 6 | Claude r6 nits 2, 11. | §2 clause 6: `assigned {repo, paths}`; a stale `conflicts` reason is a stated false negative. §3.2 and §9: `assigned` is read once, and why. §4, §8, §10 row 20, §11 item 41. |
| 7 | Claude r6 nit 3. | §4: an old v7 store is `Corrupt reason: "schema"` at open (open.rs:284-301); the three-place reporting is dropped. §10 row 5. |
| 8 | Claude r6 nits 4, 5. | §1 step 5 and §2 clause 7: `unknown` comes from the reread lifecycle, not the code. §4: `error` never marks, and neither does `upstream_unavailable`, the same failed read. A refusal beside one still marks. §10 row 20, §11 item 44. |
| 9 | Claude r6 nit 6. | §3.1: strict refuses every kept pre-move tree, with `changed` on each item; the agent's `rm` keeps its rule. §3.2 (b), §8, §9, §10 row 7, §11 item 42. |
| 10 | Claude r6 nit 7. | §11 item 43: a dev server that left the workspace is recorded as not detected. §2 clause 7. |
| 11 | Claude r6 nit 8. | §1: what the wait bound does not cover. |
| 12 | Claude r6 nit 9. | §5: the CLI's `Live` fixture emits `auto_close = false`. §10 row 9 adds one test of the product default. §11 item 45. |
| 13 | Claude r6 nits 10, 12, 13, 14. | §2.1 `row_commit` is reimplemented (§10 row 20). §9: the holder scan can name the daemon. §5: flipping a repo's key is not inert. §2, §5, §9: "pinned", not "current". §8: what re-adds a Tracker. |

## 18. r8 changes

The requirement in impl/autoclose-req.md is unchanged. Every r8 change refuses more, writes fewer marks or corrects text; nothing makes a close easier, and `Agent`'s `rm` gains no refusal. **One question is
open for the requirement owner: the audit file's name (§11 item 6).** Four changes reach past the hook: `assign` waits for the assignee's queue, `add` clears a daemon mark, `record_rm_attempt` reads the lifecycle, and a
closed session answers `NoSuchSession` ahead of the TTY gate. Tests: §10 row 21.

| Item | Review | Change |
|---|---|---|
| 1 | **Codex r7 blocker:** an `assign` during the close survived it and stranded a conflict path (handlers.rs:1235; rebase.rs:884-887; stage.rs:525). | §3.2: `assign` takes the assignee's queue place before the repo lock and refuses `Removing`, so clause 6's read is final. §1, §2, §9; §11 items 41, 46. |
| 2 | Own finding while proving it. | §3.2: an assignment can name a repo S does not hold, so the clause reads every repo of the publish rows (`assigned_to`). §11 item 51. |
| 3 | Claude r7 nit 1. | §3.1: strict has its own message lines and none names `--discard`; `attempt_summary` learns kept trees and nested repositories. |
| 4 | Claude r7 nit 2. | §4: a `Dirty` only a failed read explains is `error {code: "judge_read"}`, no mark. §3.1: `newer` no longer hides a failed read. §11 item 49. |
| 5 | Claude r7 nit 3. | §5: clause 0 reads the pinned and the current catalog, and a `false` in either refuses. §2, §9; §11 item 48. |
| 6 | Claude r7 nit 4. | §1, §2: clause 0b, `extended`, with the baseline read at the handler's entry. §4: `add` clears a daemon mark. §10 row 20's set (b) test uses an agent's mark. §11 item 47. |
| 7 | Claude r7 nit 5. | §4: `record_rm_attempt` reads the lifecycle, so `Unknown` on an `Active` session is `Refused`. |
| 8 | Claude r7 nit 6. | §6: `NoSuchSession` ahead of the gate. `confirm_commands_refuse_non_tty` stays green, because its home has no store. §11 item 50. |
| 9 | Claude r7 nit 7. | §8: the exact new text for `MARK_AFTER`, `RM_AFTER`, help.rs:31 and :201 and Hard rule 1, which r7 wrongly called unchanged. |
| 10 | Claude r7 nit 8. | §7, §11 item 6: the sign-off question. §9: the crash window. |
| 11 | Claude r7 nit 9. | §1 step 4, §2.1: one `rows` read shared by clauses 4 and 6. §9's cost row. |
| 12 | Claude r7 nit 10. | §1 step 1 reads the row fresh and applies to `Active` rows only; set (b) is a union. |
| 13 | Own corrections. | §2 clause 1 and §3.1: `Agent` lets `Unknown` through to `dirty_check`, and strict refuses it. Line drift: SKILL.md:163 and :174. §1: "closes all of it" withdrawn. |

## 19. r9 changes

The requirement in impl/autoclose-req.md is unchanged. Every r9 change refuses more, writes fewer marks or corrects text; nothing makes a close easier, and `Agent`'s `rm` gains no refusal and keeps its messages. **Two questions are open for the requirement owner: the audit file's name (§11 item 6, §7)
and the cases that write no mark (§11 item 56). Each has a default, so neither holds up implementation.** Tests: §10 row 22, plus the row 18 and row 21 edits.

| Item | Review | Change |
|---|---|---|
| 1 | **Codex r8 blocker:** an untracked sibling's own deps draft escaped clause 6 and line 3, and `Removed` then hid it (handlers.rs:202-207, :1763-1777; deps.rs:570-573). | §2 clause 6 (b), §3.2 line 3 (b): own proposals are read by `origin_session`, with no tracked filter. Main proposals keep theirs. §3.2 traces it and names the way out, the agent's `rm`. §4, §8, §9; §11 items 3, 52, 53. |
| 2 | Claude r8 nit 1. | §2 clause 6 (c), §3.2 line 3 (d): a saved `sync` report refuses as `conflicts`, `report: true`. §4, §8, §9; §11 item 54. |
| 3 | Claude r8 nit 2. | §3.1, §4: strict returns Phase B's failed projection as `Err`, so it marks nothing; a lost base still refuses. §11 item 55. |
| 4 | Claude r8 nit 3. | §10 row 21: both `assign` tests raise `fx.lock_wait`. |
| 5 | Claude r8 nit 4. | §3.1: strict has its own `OpenFiles` line. |
| 6 | Claude r8 nit 5; own sweep. | §8's table: README.md:303-305, args.rs:205, reference.md:57, SKILL.md:3 and docs/modules/cli.md:629-631; the `Dirty` lines that stay are named, with the reason. |
| 7 | Claude r8 nit 6. | §2 clause 6: `assigned` carries the whole map. §8. |
| 8 | Claude r8 nit 7. | §6: the `push` reply's condition names `extended`. §7: the example has `auth`. |
| 9 | Codex r8 nit 1; Claude r8 nit 8. | §7, §11 items 6, 56: both questions, each with a default and what changes if the owner answers otherwise. The name is one constant, and the README and daemon.md point at the file. |
| 10 | Claude r8 nit 9. | §1: `RemoveCall::AutoClose` carries clause 2's read, so a close reads upstream once. §6, §9's cost row. |
| 11 | Own corrections. | §8: `deps_pending`'s "`status` names it" was false for a draft on an untracked repo. §9: a failing clause 4 skips the judge, not the other cheap clauses. §8: README.md:487, not :467. |

## 20. r10 changes

The requirement in impl/autoclose-req.md is unchanged. Every r10 change refuses more, reports more exactly or corrects text; nothing makes a close easier, and `Agent`'s `rm` gains no refusal. One change reaches past the hook: `deps propose` on a session that is leaving now answers an error. **Five questions are open for the requirement owner (§11 items 6, 56, 58, 59, 60). Each has a default that can be built, and Claude r9 asks that the three new ones be decided before implementation starts.** Tests: §10 row 23.

| Item | Review | Change |
|---|---|---|
| 1 | **Codex r9 blocker:** S's own draft inserted after `Removing` escaped line 3, and `Removed` then hid it (handlers.rs:1763-1777; store deps.rs:355-418; deps.rs:570-573; gc.rs:942-943). | §3.2: `record_proposal` reads the origin's lifecycle in its own transaction for a row with no target. `removing` → `InvalidInput lifecycle`, `removed` → `NoSuchSession`, no row. §2 clause 6, §9, the principle; §11 item 57. |
| 2 | Own finding while proving it. | §3.2: the rule stops at drafts, because `same_to` and `ensure_target` insert `pending` rows that keep a gone session's origin, so Codex's wording read literally would break main-follow and `deps approve`. `open_proposals_lists_open_and_live_only` inserts a draft for a `Removed` session and must change. §9, §10 row 23. |
| 3 | Claude r9 nit 1. | §11 items 58, 59, 60, each with its default. §2 clause 7, §3.1, §6 and items 6, 43 point at them. |
| 4 | Claude r9 nit 2. | §2 clause 3: what happens to each kind of dropped workspace, and why a live-scene one leaves no hole. |
| 5 | Claude r9 nit 3. | §1 step 1: a `Removed` row reads `gone` with no queue take. Steps 1 and 6, §3.2's `assign` costs, §9, §10 row 23. |
| 6 | Claude r9 nit 4. | §3.1: Phase B's two internal effects are named. |
| 7 | Claude r9 nit 5. | §9's cost row: one `open_proposals` call, one `sync_report` read per repo. |
| 8 | Claude r9 nit 6. | §1's `push` row: the hook runs on `Ok`. |
| 9 | Claude r9 nit 7. | §2 clause 6, §11 item 3: an own `pending` refuses for the requirement's text; only a draft vanishes. |
