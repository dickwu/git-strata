//! Which AI agent runs this `new` (cli.md `new`): `--agent`, then
//! `STRATA_AGENT`, then the markers Claude Code, Codex and Cursor set in
//! their shells. The daemon never looks at the environment; the CLI sends
//! the name, and only to a daemon whose `daemon_status.features` lists
//! `new.agent` (older daemons refuse unknown params).

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use strata_types::{AgentName, Code, StrataError};

use crate::Result;
use crate::rpc::{self, Kind};

/// The feature a daemon lists when `new` takes `agent`.
pub const FEATURE: &str = "new.agent";

/// How long the feature probe may take; past it the session is created
/// without the agent (a warning says so).
pub const PROBE_DEADLINE: Duration = Duration::from_secs(5);

/// Auto-detected agents: any of the variables set and non-empty means the
/// name. First match wins (a Claude Code shell that started Codex has both;
/// `STRATA_AGENT` settles it).
const MARKERS: &[(&[&str], &str)] = &[
    (&["CLAUDECODE", "CLAUDE_CODE_SESSION_ID", "CLAUDE_CODE_ENTRYPOINT"], "claude-code"),
    (&["CODEX_THREAD_ID", "CODEX_SANDBOX"], "codex"),
    (&["CURSOR_AGENT"], "cursor-agent"),
];

/// The agent for `new`: `flag` (`--agent`), then `STRATA_AGENT` (empty =
/// unset), then the markers; `None` when nothing says. An explicit value
/// that is not a valid name is `InvalidInput` reason `agent`, never skipped.
pub fn detect(flag: Option<&str>, env: impl Fn(&str) -> Option<OsString>) -> Result<Option<AgentName>> {
    if let Some(f) = flag {
        return named(f.to_string(), "--agent").map(Some);
    }
    if let Some(v) = env("STRATA_AGENT").filter(|v| !v.is_empty()) {
        let s = v.into_string().map_err(|_| {
            StrataError::new(Code::InvalidInput, "STRATA_AGENT is not UTF-8")
                .with_detail(json!({ "reason": "agent", "source": "STRATA_AGENT" }))
        })?;
        return named(s, "STRATA_AGENT").map(Some);
    }
    for (vars, name) in MARKERS {
        if vars.iter().any(|k| env(k).is_some_and(|v| !v.is_empty())) {
            return Ok(Some(AgentName::try_from(name.to_string()).expect("a valid built-in name")));
        }
    }
    Ok(None)
}

fn named(s: String, source: &str) -> Result<AgentName> {
    AgentName::try_from(s).map_err(|mut e| {
        e.message = format!("{source}: {}", e.message);
        e.detail = Some(json!({ "reason": "agent", "source": source }));
        e
    })
}

/// Whether the daemon on `socket` says `new` takes `agent`. Bounded by
/// `PROBE_DEADLINE`; any failure (down, slow, an older reply) is `false`.
pub fn daemon_takes_agent(socket: &Path) -> bool {
    match rpc::call_within(socket, "daemon_status", json!({ "verify": false }), Kind::Read, PROBE_DEADLINE) {
        Ok(v) => lists_feature(&v),
        Err(_) => false,
    }
}

fn lists_feature(status: &Value) -> bool {
    status["features"].as_array().is_some_and(|f| f.iter().any(|x| x == FEATURE))
}

/// An older daemon refused `agent` while parsing params (rpc `Failed`,
/// serde's "unknown field `agent`"): nothing ran, so `new` may be sent
/// again without it. Anything else is not replayed.
pub fn refused_unknown_agent(e: &StrataError) -> bool {
    let d = e.detail.as_ref();
    e.code == Code::InvalidInput
        && d.and_then(|d| d.get("rpc")).and_then(Value::as_str) == Some("Failed")
        && e.message.contains("unknown field `agent`")
}

/// The warning `new` adds when the agent could not be recorded.
pub fn not_recorded(agent: &AgentName) -> Value {
    json!({ "kind": "AgentNotRecorded", "agent": agent.as_str(), "reason": "daemon_too_old" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    fn run(flag: Option<&str>, vars: &[(&str, OsString)]) -> Result<Option<String>> {
        detect(flag, |k| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone())).map(|a| a.map(|a| a.as_str().to_string()))
    }

    fn s(v: &str) -> OsString {
        v.into()
    }

    #[test]
    fn precedence() {
        let all = [
            ("STRATA_AGENT", s("aider")),
            ("CLAUDECODE", s("1")),
            ("CODEX_THREAD_ID", s("t")),
            ("CURSOR_AGENT", s("1")),
        ];
        assert_eq!(run(Some("mine"), &all).unwrap().as_deref(), Some("mine"));
        assert_eq!(run(None, &all).unwrap().as_deref(), Some("aider"));
        assert_eq!(run(None, &all[1..]).unwrap().as_deref(), Some("claude-code"));
        assert_eq!(run(None, &all[2..]).unwrap().as_deref(), Some("codex"));
        assert_eq!(run(None, &all[3..]).unwrap().as_deref(), Some("cursor-agent"));
        assert_eq!(run(None, &[]).unwrap(), None);
    }

    #[test]
    fn every_marker_counts() {
        for (vars, name) in MARKERS {
            for k in *vars {
                assert_eq!(run(None, &[(k, s("x"))]).unwrap().as_deref(), Some(*name), "{k}");
            }
        }
    }

    #[test]
    fn empty_values_are_unset() {
        let v = [("STRATA_AGENT", s("")), ("CLAUDECODE", s("")), ("CODEX_SANDBOX", s("seatbelt")), ("CURSOR_AGENT", s(""))];
        assert_eq!(run(None, &v).unwrap().as_deref(), Some("codex"));
        assert_eq!(run(None, &[("CLAUDE_CODE_ENTRYPOINT", s("")), ("CURSOR_AGENT", s(""))]).unwrap(), None);
    }

    /// (--agent, environment, expected reason)
    type BadCase<'a> = (Option<&'a str>, Vec<(&'a str, OsString)>, &'a str);

    #[test]
    fn explicit_bad_values_refuse_and_do_not_fall_through() {
        let marker = ("CLAUDECODE", s("1"));
        let cases: Vec<BadCase> = vec![
            (Some(""), vec![marker.clone()], "--agent"),
            (Some("bad name"), vec![marker.clone()], "--agent"),
            (Some("-x"), vec![], "--agent"),
            (None, vec![("STRATA_AGENT", s("é")), marker.clone()], "STRATA_AGENT"),
            (None, vec![("STRATA_AGENT", s(&"a".repeat(65)))], "STRATA_AGENT"),
            (None, vec![("STRATA_AGENT", OsString::from_vec(vec![0x61, 0xff])), marker.clone()], "STRATA_AGENT"),
        ];
        for (flag, vars, source) in cases {
            let e = run(flag, &vars).expect_err(&format!("{flag:?} {vars:?}"));
            assert_eq!(e.code, Code::InvalidInput);
            assert_eq!(e.detail, Some(json!({ "reason": "agent", "source": source })), "{flag:?} {vars:?}");
        }
    }

    #[test]
    fn feature_and_refusal_shapes() {
        assert!(lists_feature(&json!({ "features": ["x", "new.agent"] })));
        assert!(!lists_feature(&json!({ "features": [] })));
        assert!(!lists_feature(&json!({ "holds": [] })));
        let old = StrataError::new(Code::InvalidInput, "unknown field `agent`, expected one of `ticket`, `repos`, `only`")
            .with_detail(json!({ "reason": "rpc_failed", "method": "new", "rpc": "Failed" }));
        assert!(refused_unknown_agent(&old));
        let other = StrataError::new(Code::InvalidInput, "unknown field `x`").with_detail(json!({ "reason": "rpc_failed", "method": "new", "rpc": "Failed" }));
        assert!(!refused_unknown_agent(&other));
        let handler = StrataError::new(Code::InvalidInput, "unknown field `agent`").with_detail(json!({ "reason": "agent" }));
        assert!(!refused_unknown_agent(&handler));
        let w = not_recorded(&AgentName::try_from("codex".to_string()).unwrap());
        assert_eq!(w, json!({ "kind": "AgentNotRecorded", "agent": "codex", "reason": "daemon_too_old" }));
    }
}
