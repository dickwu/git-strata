# Dev servers and `tauri dev`: per-session ports

Every session workspace gets its own block of 4 ports (a lease), so two
sessions can run `tauri dev` (or any dev server) at the same time. The main
checkout has no lease and keeps the project's default ports. Never change
ports by editing committed files (`tauri.conf.json`, `package.json`,
`next.config.*`, `.env`); start the server through git-strata instead:

```bash
cd <your workspace>                     # data.manifest.repos.<repo>.path
git-strata session ports                # read the lease: data.leases[0].ports / .env
git-strata session exec -- bun run dev  # any server that reads $PORT (Next.js does)
git-strata session exec -- sh -c 'vite --port "$PORT" --strictPort'
git-strata session exec --tauri -- bun run tauri dev
```

- `exec` runs the command in place of itself (its output, its exit code)
  in exactly one workspace (`--repo R` when the session has several) with:
  `PORT` = `STRATA_DEV_PORT` (web), `STRATA_CONNECTOR_PORT`,
  `STRATA_CONNECTOR_MCP_PORT`, `STRATA_AUX_PORT`, `STRATA_SESSION`,
  `STRATA_REPO`, `TAURI_CONNECTOR_PROJECT` (the Tauri app dir) and
  `STRATA_TAURI_DEV_CONFIG`. Child processes (e.g. Tauri's
  `beforeDevCommand`) inherit them.
- `--tauri` adds `--config $STRATA_TAURI_DEV_CONFIG` to `tauri dev`, which
  points `build.devUrl` at `http://localhost:$PORT`. The frontend dev server
  must then listen on `$PORT`. It understands `tauri dev`, `cargo tauri dev`,
  `npx|bunx|pnpx tauri dev`, `pnpm exec tauri dev`, and `bun|pnpm|yarn|npm
  run <script>` when that script is literally `tauri dev ...` (or the bare
  `tauri` CLI, with `dev` right after the script name: `bun run tauri dev`,
  `npm run tauri -- dev`). Always write `run`: `bun <script>` without it is
  refused, since the package manager may run its own command of that name.
  Options that pick another package (`--prefix`, `--filter`, `-w`,
  `--workspace`, `--cwd`, ...) and, for npm, any option before its `--` are
  refused. Anything else is `InvalidInput` `tauri_launcher`; use the form in
  `detail.fix`:
  `git-strata session exec -- sh -c 'tauri dev --config "$STRATA_TAURI_DEV_CONFIG"'`.
- `exec` does not set tauri-connector's variables. tauri-connector 0.17.7+
  binds exactly `TAURI_CONNECTOR_PORT`, `TAURI_CONNECTOR_MCP_PORT` and
  `TAURI_CONNECTOR_BRIDGE_PORT`; map the lease onto them:
  `git-strata session exec -- sh -c 'TAURI_CONNECTOR_PORT=$STRATA_CONNECTOR_PORT TAURI_CONNECTOR_MCP_PORT=$STRATA_CONNECTOR_MCP_PORT TAURI_CONNECTOR_BRIDGE_PORT=$STRATA_AUX_PORT exec tauri dev --config "$STRATA_TAURI_DEV_CONFIG"'`.
  Precedence: a port in the app's builder code > the exact env variable >
  `TAURI_CONNECTOR_*_PORT_RANGE` > a builder range > the default. A busy
  exact port is not replaced: the connector does not start (the app keeps
  running); free the port or `--reassign`, then restart. The bound ports
  are in the `.connector.json` under your workspace's `target/` (copies
  from the main checkout are removed when the session is created).
- `exec`'s variables reach only the command it starts and its children, not
  a later connector command or an MCP server that is already running. When
  you talk to the app from such a client, select it explicitly: `tauri-connector
  ... --project <tauri_app_root>` (`tauri_app_root`, else `path`, from
  `git-strata session ports`), or wrap the client:
  `git-strata session exec --no-check -- sh -c 'TAURI_CONNECTOR_PORT=$STRATA_CONNECTOR_PORT tauri-connector ...'`
  (without `--port` the CLI uses `TAURI_CONNECTOR_PORT`). Never connect
  to the connector's default ports (9555/9556): they may be another
  session's app.
- Before starting, `exec` checks the lease's ports (`--no-check` skips it;
  `session ports --check` only reports):
  - `PortInUse` `already_running`: a process of this workspace already
    listens. Reuse it; stop it only if you started it.
  - `PortInUse` `foreign`: something else holds a port. Nothing moves on its
    own: run `detail.fix` (`git-strata session ports --reassign --session S
    --repo R`) for your own session, then `exec` again. Never kill a process
    you did not start.
  - `Busy` `port_unidentified` / `port_probe_error`: retry shortly.
- A workspace without a lease yet (e.g. one created before this feature)
  gets one from the daemon on first `ports` / `exec`; an existing lease works
  without the daemon. `Busy` `daemon_down` from `ports` / `exec`: report it
  to the human; do not start the daemon yourself. `Busy` `ports_exhausted`
  (`ports` shows the workspace with `"ports": null`): no free block; tell the
  human. `Busy` `lease_changed` (`--reassign`): another caller moved the
  lease since your check; run `session ports --check` again and decide
  afresh. `Unsupported` `daemon_too_old`: the running daemon predates leases;
  tell the human (install the matching git-strata and restart the daemon).
  `Unsupported` `no_policy`: the daemon has no port-allocation policy
  installed (not about `[dev_ports]`, which is optional); tell the human.
- `exec` from another session's workspace is `InvalidInput` `cwd_mismatch`;
  from outside every workspace it runs at the workspace root. A command it
  cannot find is `NoSuchPath`.
