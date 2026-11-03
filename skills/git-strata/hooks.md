# Agent hooks (optional)

Hooks are set up by the human in the agent's own settings; git-strata never
writes them, and `npx skills add` does not install them. Without a hook, do
the same work by hand:

- after each meaningful edit: `git-strata checkpoint -m "what changed"`
- before you finish: `git-strata sync --check` (see [sync.md](sync.md))

[cli.md §3.5](https://github.com/dickwu/git-strata/blob/main/docs/modules/cli.md) documents the hook input for Claude Code only. Other
agents (Codex, Cursor, ...) run the two commands above themselves.

## Checkpoint after every tool call (Claude Code)

In Claude Code's `settings.json` (user `~/.claude/settings.json` or project
`.claude/settings.json`):

```json
{"hooks":{"PostToolUse":[{"matcher":"Edit|Write|Bash|MultiEdit","hooks":[{"type":"command","command":"git-strata checkpoint --tool-call"}]}],"SessionStart":[{"hooks":[{"type":"command","command":"git-strata daemon status"}]}]}}
```

- The session is resolved from the hook's cwd; do not add `--session`.
- When `--tool-call` is given and stdin is not a TTY, stdin is read as one JSON
  object (the hook input). Paths are taken from `tool_input.file_path`,
  `tool_input.file_paths[]` and `tool_input.edits[].file_path`; relative paths
  are joined to the object's `cwd`. A tool with no paths (e.g. `Bash`) scans
  every tracked repo of the session. Example input:
  `{"tool_use_id":"toolu_01","cwd":"/…/sessions/s-k3q7xm2a/tool","tool_name":"Edit","tool_input":{"file_path":"src/a.ts"}}`
- `--tool-call` without a value (or with an empty one) takes the id from the
  JSON's `tool_use_id`. If that is missing or empty too, or stdin is a TTY,
  the result is `InvalidInput` (`checkpoint_reason`). `--tool-call ID` with a
  value uses that ID.

## Optional Stop hook: upstream before finishing (Claude Code)

Enable this only after the deployed daemon advertises both `sync` and
`status.upstream`. The human adds the entry; git-strata does not change settings:

```json
{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"git-strata sync --check --hook"}]}]}}
```

The hook resolves its input cwd and checks cached upstream. It returns control
once when a following repo has main/deps changes and your unpublished work or
conflicts need attention. Follow its sync → resolve → checks → publish reason.
An unchanged workspace behind main is left to the background mover. Cold
cache, daemon errors, non-workspace cwd and the five-second deadline are silent.
`stop_hook_active:true` never blocks again. A block is `{decision,reason}` and
the process always exits zero, rather than using the usual envelope.
