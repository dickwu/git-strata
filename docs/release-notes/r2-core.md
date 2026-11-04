# R2 core (unreleased)

- Publish accepts repeated `-m` and per-repo `--message-for`. Without flags, manual
  checkpoint messages supply subjects; tool-call IDs do not. No-change manual
  checkpoints still record no new message. Commits carry recovery trailers.
- `new --base` parks selected workspaces at local commits at/ahead of main.
  Publish refuses with `InvalidInput base_not_on_main` when the workspace base
  is on neither the publish target nor main. Existing rewound-chain sessions
  that used to publish while silently dropping base-only commits are now
  refused until the human lands or recovers those commits.
  Dependency approval cannot silently drop their commits. Publish lines are
  excluded from this release.
- Automatic moves ignore dependency/cache writes only where content is not
  carried. Nested seed caches and tracked files remain protected. Eligible
  stuck rows name holders and hand control back once through the Stop hook.
- Pin timestamps are in a separate audit sidecar; warnings gain timestamps
  without changing their existing wire representation.
- Successful non-discard rm reports its landed verdict in upstream metadata.

Store schema v6 is forward-only. **The 0.1.2 CLI is unusable on a v6 store.**
Upgrade the CLI and daemon together. Commands that open the store directly
include `add`, `restore`, `diff`, `deps propose`, `rm`, `resolve`, `assign`,
`publish`, devports (`session ports` / `session exec`), `checkpoint` (including
the PostToolUse checkpoint hook), `grep`, and `files`. An old checkpoint hook
fails on every tool call after the migration. A few RPC-only commands still
answer, which does not make a mixed installation supported.

Startup prints the verified pre-migration backup path. To downgrade from v6
to the v5 store used by 0.1.2, the human must:

1. Stop the daemon and ensure it has stopped.
2. Move `store/store.db` and its `-wal`, `-shm`, and `-tshm` sidecars aside and
   keep them together for recovery.
3. Copy the verified `store/backups/store-v5-<ns>.db` backup into
   `store/store.db`. If upgrading directly from an earlier schema, restore
   the actual `store-v<from>-<ns>.db` backup compatible with the old binaries.
4. Install the matching old CLI and daemon together. Remove `rebase.quiet_skip`
   from `config.toml`; 0.1.2 cannot parse it. Downgrades further back must also
   follow the older config/pin cleanup in [store recovery](../modules/store.md#44a-schema-迁移).
5. Start the matching old daemon only after restoring the store and config.

Restoring the backup loses store metadata recorded since migration, including
later session and checkpoint records. Session directories may remain on disk;
preserve and reconcile them before further work. Git commits and refs remain.
The backup is a rollback, not a way to merge newer metadata into an older store.

Refresh the installed skill with `npx skills update git-strata -g` so baseline
and human hand-off guidance match the upgraded binary.
