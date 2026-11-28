# CLI and daemon wire compatibility

**Upgrade the CLI and daemon together** until the hosted C1 matrix passes
for the release. The candidate checks a 0.3.0 floor in both directions while
the store schema and config format match. A release that migrates the store or adds a
config key must identify the affected older CLIs in its notes. If a local
store open reports `cli_too_old`, use the updated CLI on PATH. Shipped
0.3.x clients report `Corrupt user_version` after a store migration.

The CLI retries an individual RPC that receives `Busy upgrading` with
`retry_safe: true`. A multi-repo checkpoint keeps its earlier results.
The capability probe and any legacy parameter or envelope resend share one
retry budget. `config reload`, `mark` and `config relax` check their feature on
that probe and send through the same call. A certified sync retry still gets
its own I/O deadline. Public `daemon status` checks the peer version on its
original unannotated reply. A successful status reply without a release
`version` (v0.2.0 sends none) is below the floor, so the CLI sends nothing more.
The 0.3.2 CLI already retries certified refusals for `new` and `add`.
A shipped 0.3.x hook that meets upgrade overflow or hold expiry misses that
checkpoint. A later tool call has a different ID and cannot recreate it.

## Contract checks

The local `upgrade_wire` Cargo integration tests exercise the candidate CLI and
its RPC transport on fixture sockets. All 28 RPCs cross a simulated older
parser. The tests cover a worker change after
the feature probe, replay counts and post-send deadlines. The CLI library tests
check how a newer schema and its recorded writer version are reported. The
daemon's `rpc` suite checks every method against the same parameter corpus
used by the release matrix. To exercise the matrix harness locally with
current Cargo artifacts, build `git-stratad-test` with `test-seams`, then run
the ignored `c1_fixture_worker_contract` test. It mounts an APFS sparse image
and simulates a pre-`envelope.client` peer in front of that worker.

C1 tests both directions for the compatibility floor, the oldest published
release in the window, and the previous published release. They currently
correspond to 0.3.0, 0.3.1 and 0.3.2. The 0.3.0 tag has no release tarball.
CI verifies the tag against the recorded commit `fbc6a76` and builds its
locked CLI and daemon on the hosted macOS runner. For the published releases,
CI verifies the tarball checksums before using the binaries.

Each pairing runs the shared RPC corpus and real new, checkpoint, status
and publish preview commands. The candidate must successfully return the
`publish --dry-run` plan for the expected repo. For historical 0.3.0–0.3.2
CLIs that lack the flag, the harness observes the successful `publish_plan`
reply before the real non-TTY gate. Both paths require a valid plan and
zero publish RPCs. The matrix also checks migration errors and lets the
older daemon's parser exercise the envelope downgrade.

To run the local cases with Cargo:

```sh
cargo test -p git-strata --lib --test upgrade_wire
cargo test -p strata-daemon --test rpc
cargo build -p strata-daemon --features test-seams --bin git-stratad-test
cargo test -p git-strata --test upgrade_wire c1_fixture_worker_contract -- --exact --ignored
```

The last two commands exercise the sparse-image fixture using Python 3.11 or
later and the macOS disk-image service. Released binaries
are downloaded only by the hosted workflow.
