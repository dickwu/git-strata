use std::time::{Duration, SystemTime};

use strata_types::{
    Action, AgentName, BaseGen, Batch, BlobId, Checkpoint, CheckpointId, CheckpointReason, Code, CommitOid,
    ConfirmToken, DepsSet, DepsSetId, EntryKind, ManifestEntry, OverlayEntry, ProposalId,
    PublishTarget, PushOutcome, ReleaseGroup, RepoId, RepoName, RepoPath, SessionId,
    SessionManifest, StrataError, TreeOid, WorkspaceState,
};

fn rn(s: &str) -> RepoName {
    RepoName::try_from(s.to_owned()).unwrap()
}
fn rp(s: &str) -> RepoPath {
    RepoPath::try_from(s.to_owned()).unwrap()
}
fn tree() -> TreeOid {
    TreeOid::try_from("0123456789abcdef0123456789abcdef01234567".to_owned()).unwrap()
}
fn blob() -> BlobId {
    BlobId::try_from("ab".repeat(32)).unwrap()
}
fn deps() -> DepsSetId {
    DepsSetId::try_from("cd".repeat(32)).unwrap()
}

#[test]
fn code_set_is_closed_at_forty_and_serializes_as_the_variant_name() {
    assert_eq!(Code::ALL.len(), 40);
    let unique: std::collections::HashSet<_> = Code::ALL.iter().copied().collect();
    assert_eq!(unique.len(), 40);
    assert_eq!(
        serde_json::to_string(&Code::Conflict).unwrap(),
        "\"Conflict\""
    );
    assert_eq!(
        serde_json::to_string(&Code::CheckMutates).unwrap(),
        "\"CheckMutates\""
    );
    assert_eq!(Code::NotConfirmed.to_string(), "NotConfirmed");
}

#[test]
fn strata_error_display_and_json_omit_empty_detail() {
    let err = StrataError::new(Code::Dirty, "session still has unpublished changes");
    assert_eq!(
        err.to_string(),
        "Dirty: session still has unpublished changes"
    );
    let json = serde_json::to_value(&err).unwrap();
    assert_eq!(json["code"], "Dirty");
    assert_eq!(json["message"], "session still has unpublished changes");
    assert!(json.get("detail").is_none());
    let with = err.with_detail(serde_json::json!({"paths": ["src/a.ts"]}));
    let json = serde_json::to_value(&with).unwrap();
    assert_eq!(json["detail"]["paths"][0], "src/a.ts");
}

#[test]
fn checkpoint_json_matches_the_documented_shape() {
    let at = strata_types::serde_rfc3339::parse("2026-10-22T18:04:11Z").unwrap();
    let cp = Checkpoint {
        id: CheckpointId::try_from(7_u64).unwrap(),
        session: SessionId::try_from("s-k3q7xm2a".to_owned()).unwrap(),
        repo: rn("tool"),
        base: BaseGen {
            repo: rn("tool"),
            tree: tree(),
            deps: deps(),
        },
        at,
        reason: CheckpointReason::ToolCall {
            id: "toolu_01AbCd".to_owned(),
        },
        entries: vec![
            OverlayEntry {
                path: rp("src/app/page.tsx"),
                kind: EntryKind::Modified { blob: blob() },
                mode: 0o100644,
            },
            OverlayEntry {
                path: rp("src/legacy.ts"),
                kind: EntryKind::Deleted,
                mode: 0,
            },
            OverlayEntry {
                path: rp("src/foo.ts"),
                kind: EntryKind::Renamed {
                    from: rp("src/Foo.ts"),
                    blob: blob(),
                },
                mode: 0o100644,
            },
        ],
    };
    let json = serde_json::to_value(&cp).unwrap();
    assert_eq!(json["id"], 7);
    assert_eq!(json["at"], "2026-10-22T18:04:11Z");
    assert_eq!(json["reason"]["kind"], "ToolCall");
    assert_eq!(json["reason"]["id"], "toolu_01AbCd");
    assert_eq!(json["entries"][0]["kind"], "Modified");
    assert_eq!(json["entries"][0]["mode"], 0o100644);
    assert!(json["entries"][0].get("from").is_none());
    assert_eq!(json["entries"][1]["kind"], "Deleted");
    assert_eq!(json["entries"][2]["from"], "src/Foo.ts");
    let back: Checkpoint = serde_json::from_value(json).unwrap();
    assert_eq!(back, cp);
}

#[test]
fn rfc3339_round_trips_fractional_seconds_and_rejects_offsets() {
    let t = SystemTime::UNIX_EPOCH
        + Duration::from_secs(1_700_000_000)
        + Duration::from_nanos(123_000_000);
    let s = strata_types::serde_rfc3339::format(&t).unwrap();
    assert!(s.ends_with('Z'), "{s}");
    assert!(s.contains('.'), "{s}");
    assert_eq!(strata_types::serde_rfc3339::parse(&s).unwrap(), t);
    assert!(strata_types::serde_rfc3339::parse("2026-10-22T18:04:11+00:00").is_err());
    assert!(strata_types::serde_rfc3339::parse("not-a-time").is_err());
}

#[test]
fn push_outcome_and_action_use_kind_tags() {
    let pushed = serde_json::to_value(PushOutcome::Pushed).unwrap();
    assert_eq!(pushed["kind"], "Pushed");
    assert!(pushed.get("message").is_none());
    let refused = serde_json::to_value(PushOutcome::Refused("non-fast-forward".into())).unwrap();
    assert_eq!(refused["kind"], "Refused");
    assert_eq!(refused["message"], "non-fast-forward");
    let action = Action::DepsApprove {
        proposal: ProposalId::try_from(4_u64).unwrap(),
    };
    let json = serde_json::to_value(&action).unwrap();
    assert_eq!(json["kind"], "DepsApprove");
    assert_eq!(json["proposal"], 4);
    let relax = Action::ConfigRelax {
        repo: RepoId::new("/Volumes/GWD/inter/tool/.git".into()),
    };
    let json = serde_json::to_value(&relax).unwrap();
    assert_eq!(json["repo"], "/Volumes/GWD/inter/tool/.git");
}

#[test]
fn session_manifest_skips_absent_ticket() {
    let manifest = SessionManifest {
        id: SessionId::try_from("s-k3q7xm2a".to_owned()).unwrap(),
        ticket: None,
        agent: None,
        root: "/Volumes/GWD/inter/.strata/sessions/s-k3q7xm2a".into(),
        repos: std::collections::BTreeMap::from([(
            rn("tool"),
            WorkspaceState {
                base: BaseGen {
                    repo: rn("tool"),
                    tree: tree(),
                    deps: deps(),
                },
                head: CommitOid::try_from("0123456789abcdef0123456789abcdef01234567".to_owned())
                    .unwrap(),
                path: "/Volumes/GWD/inter/.strata/sessions/s-k3q7xm2a/tool".into(),
                tracked: true,
            },
        )]),
    };
    let json = serde_json::to_value(&manifest).unwrap();
    assert!(json.get("ticket").is_none());
    assert!(json.get("agent").is_none());
    let back: SessionManifest = serde_json::from_value(json).unwrap();
    assert_eq!(back, manifest);
}

#[test]
fn confirm_token_is_plain_data_with_a_debug_impl() {
    let token = ConfirmToken {
        action: Action::RemoveDiscard {
            session: SessionId::try_from("s-k3q7xm2a".to_owned()).unwrap(),
        },
        issued: std::time::Instant::now(),
        nonce: 7,
    };
    let rendered = format!("{token:?}");
    assert!(rendered.contains("RemoveDiscard"));
    let _ = DepsSet {
        id: deps(),
        repo: rn("tool"),
        inputs_hash: "ab".repeat(32),
        platform: "aarch64-apple-darwin".into(),
        toolchain: "bun 1.3.1".into(),
        dirs: vec![rp("node_modules")],
    };
    let _ = ManifestEntry {
        blob: blob(),
        mode: 0o100644,
        size: 3,
    };
    let _ = Batch {
        session: SessionId::try_from("s-k3q7xm2a".to_owned()).unwrap(),
        repo: rn("tool"),
        checkpoint: CheckpointId::try_from(1_u64).unwrap(),
        paths: vec![rp("src/a.ts")],
    };
    let _ = ReleaseGroup {
        id: 1,
        batches: vec![],
    };
    let _ = PublishTarget {
        branch: "main".into(),
        push: true,
        confirm: false,
    };
}

#[test]
fn session_manifest_carries_agent_and_reads_old_json_without_it() {
    let mut old = serde_json::json!({
        "id": "s-k3q7xm2a",
        "root": "/tmp/s",
        "repos": {},
    });
    let m: SessionManifest = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(m.agent, None);
    old["agent"] = "claude-code".into();
    let m: SessionManifest = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(m.agent, Some(AgentName::try_from("claude-code".to_owned()).unwrap()));
    assert_eq!(serde_json::to_value(&m).unwrap()["agent"], "claude-code");
    old["agent"] = "".into();
    assert!(serde_json::from_value::<SessionManifest>(old).is_err());
}
