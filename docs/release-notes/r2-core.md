# R2 core (unreleased)

- Publish accepts repeated -m and per-repo --message-for. Without flags, manual
  checkpoint messages supply subjects; tool-call IDs do not. No-change manual
  checkpoints still record no new message. Commits carry recovery trailers.
- new --base parks selected workspaces at local commits at/ahead of main.
  Publish refuses ahead/rewound bases until the human lands or recovers them.
  Dependency approval cannot silently drop their commits. Publish lines are
  excluded from this release.
- Automatic moves ignore dependency/cache writes only where content is not
  carried. Nested seed caches and tracked files remain protected. Eligible
  stuck rows name holders and hand control back once through the Stop hook.
- Pin timestamps are in a separate audit sidecar; warnings gain timestamps
  without changing their existing wire representation.
- Successful non-discard rm reports its landed verdict in upstream metadata.

Store schema v6 is forward-only. Upgrade CLI and daemon together; older
binaries reject v6 (some RPC-only old-CLI commands still work, but direct reads
do not). Startup prints the verified backup path. Downgrade means restoring
that backup and losing subsequent store metadata; Git commits/refs remain.
Upgrade before using rebase.quiet_skip, which 0.1.2 cannot parse. Refresh the
installed skill so baseline and human hand-off guidance match the binary.
