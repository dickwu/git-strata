mod support;
use std::collections::BTreeMap;
use strata_publish::{
    message::{Messages, Source},
    publish_with_messages,
};
use strata_types::{Checkpoint, CheckpointReason, Code, CommitOid, ReleaseGroup, SessionId};
use support::*;

fn fx() -> Fx {
    let mut fx = Fx::with(&["tool"]);
    fx.cfg_mut("tool").publish.push = false;
    fx
}
fn cp(fx: &Fx, s: &SessionId, reason: CheckpointReason) -> Checkpoint {
    let mut c = fx.checkpoint(s, "tool");
    c.reason = reason;
    c.id = fx.store().record_checkpoint(c.clone()).unwrap();
    c
}
fn manual(fx: &Fx, s: &SessionId, message: &str) -> Checkpoint {
    cp(
        fx,
        s,
        CheckpointReason::Manual {
            message: message.into(),
        },
    )
}
fn run(
    fx: &Fx,
    g: &ReleaseGroup,
    m: &Messages,
) -> strata_publish::Result<strata_publish::Published> {
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let ctx = fx.ctx(&d, &ss);
    publish_with_messages(&ctx, g, None, None, m)
}
fn body(fx: &Fx, c: &CommitOid) -> String {
    fx.sb
        .git(&fx.main("tool"), &["show", "-s", "--format=%B", c.as_str()])
}

#[test]
fn custom_message_retains_user_trailers_and_counts_final_changed_files() {
    let fx = fx();
    let s = fx.new_session(&["tool"]);
    for i in 0..22 {
        fx.edit(&s, "tool", &[(&format!("many/f{i}"), b"file\n")]);
    }
    let c = fx.checkpoint(&s, "tool");
    let g = group(vec![batch(&s, "tool", c.id, &["many"])]);
    let m = Messages {
        message: Some("default".into()),
        message_for: BTreeMap::from([(
            repo_name("tool"),
            "custom\n\nbody\n\nCo-authored-by: Person <p@example.test>".into(),
        )]),
    };
    let p = run(&fx, &g, &m).unwrap();
    let text = body(&fx, &p.commit);
    assert!(text.starts_with("custom\n\nbody"));
    assert!(text.contains("Co-authored-by: Person <p@example.test>\nsessions: "));
    assert!(text.contains(&format!("paths: 22\nStrata-Publish: {} (tool)", p.id)));
    assert_eq!(p.message_source, Source::Flag);
    assert_eq!(
        fx.store().publish_record(p.id).unwrap().unwrap().message,
        m.message_for.get(&repo_name("tool")).cloned()
    );
    let parent = fx
        .git_of("tool")
        .resolve_rev(&format!("{}^", p.commit))
        .unwrap()
        .unwrap();
    let history = fx
        .git_of("tool")
        .history_between(&parent, &p.commit, 20)
        .unwrap();
    assert!(history[0].sessions.contains(&s.to_string()));
    let trailers = fx
        .git_of("tool")
        .commits_between_trailers(&parent, &p.commit, 20)
        .unwrap();
    assert_eq!(trailers[0].publishes, vec![format!("{} (tool)", p.id)]);
}

#[test]
fn checkpoint_messages_are_manual_bounded_deduplicated_and_consumed() {
    let fx = fx();
    let s = fx.new_session(&["tool"]);
    fx.edit(&s, "tool", &[("new.txt", b"one")]);
    manual(&fx, &s, "a");
    cp(
        &fx,
        &s,
        CheckpointReason::ToolCall {
            id: "toolu_secret".into(),
        },
    );
    manual(&fx, &s, "   ");
    manual(&fx, &s, "b");
    let c = manual(&fx, &s, "b");
    let g = group(vec![batch(&s, "tool", c.id, &["new.txt"])]);
    let p = run(&fx, &g, &Messages::default()).unwrap();
    let text = body(&fx, &p.commit);
    assert!(text.starts_with("b\n\n"), "{text}");
    assert!(text.contains("- a\n") && text.contains("- b\n"));
    assert!(!text.contains("toolu_secret"));
    assert_eq!(text.matches("- b\n").count(), 1);
    assert_eq!(p.message_source, Source::Checkpoints);
    // A new BeforePublish row contains no manual message; the old window is consumed.
    let old = fx.store().checkpoint(&s, c.id).unwrap();
    let mut c = old;
    c.reason = CheckpointReason::BeforePublish;
    c.id = fx.store().record_checkpoint(c.clone()).unwrap();
    let next = group(vec![batch(&s, "tool", c.id, &["new.txt"])]);
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let ctx = fx.ctx(&d, &ss);
    assert_eq!(
        strata_publish::message::choose(&ctx, &next, &Messages::default())
            .unwrap()
            .source,
        Source::Legacy
    );
}

#[test]
fn local_ahead_base_is_pinned_and_publish_refuses_until_origin_contains_it() {
    let fx = fx();
    let main = fx.main("tool");
    let ahead = fx
        .sb
        .commit(&main, &[("base-only.txt", b"must survive")], "local base");
    let ss = fx.sessions();
    let rep = ss
        .new_at(
            None,
            &[repo_name("tool")],
            strata_session::Layout::Only,
            None,
            &BTreeMap::from([(repo_name("tool"), ahead.clone())]),
        )
        .unwrap();
    let s = rep.manifest.id;
    assert_eq!(rep.manifest.repos[&repo_name("tool")].head.as_str(), ahead);
    assert!(rep.warnings.iter().any(|w| matches!(
        w,
        strata_session::NewWarning::BaseAheadOfMain { ahead: 1, .. }
    )));
    assert_eq!(
        fx.read_ref("tool", &format!("refs/strata/pins/{s}"))
            .as_deref(),
        Some(ahead.as_str())
    );
    fx.edit(&s, "tool", &[("mine.txt", b"mine")]);
    let c = fx.checkpoint(&s, "tool");
    let g = group(vec![batch(&s, "tool", c.id, &["mine.txt"])]);
    let e = run(&fx, &g, &Messages::default()).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput);
    assert_eq!(e.detail.as_ref().unwrap()["reason"], "base_not_on_main");
    assert!(
        e.detail.as_ref().unwrap()["fix"]
            .as_str()
            .unwrap()
            .starts_with("ask the human")
    );
    assert!(
        !fx.store()
            .publishes()
            .unwrap()
            .iter()
            .any(|r| r.phase == strata_store::PublishPhase::Done)
    );
    fx.sb.git(&main, &["push", "origin", "HEAD:main"]);
    fx.git_of("tool").fetch("origin", "main").unwrap();
    let p = run(&fx, &g, &Messages::default()).unwrap();
    assert!(
        fx.git_of("tool")
            .is_ancestor(&CommitOid::try_from(ahead).unwrap(), &p.commit)
            .unwrap()
    );
    assert!(
        fx.sb
            .git(&main, &["show", &format!("{}:base-only.txt", p.commit)])
            .contains("must survive")
    );
}

#[test]
fn moved_baseline_refuses_but_equal_tree_different_commit_is_allowed() {
    let fx = fx();
    let s = fx.new_session(&["tool"]);
    fx.edit(&s, "tool", &[("mine.txt", b"mine")]);
    let c = fx.checkpoint(&s, "tool");
    let g = group(vec![batch(&s, "tool", c.id, &["mine.txt"])]);
    let ss = fx.sessions();
    let main = fx.main("tool");
    fx.sb
        .git(&main, &["commit", "--allow-empty", "-m", "same tree"]);
    fx.sb.git(&main, &["push", "origin", "HEAD:main"]);
    fx.git_of("tool").fetch("origin", "main").unwrap();
    let head = CommitOid::try_from(fx.sb.head(&main)).unwrap();
    let base = ss.ensure_base(&repo_name("tool"), &head).unwrap();
    ss.move_workspace(&s, &fx.git_of("tool").repo_id(), base, head)
        .unwrap();
    let d = fx.deps_ctx("tool");
    let ctx = fx.ctx(&d, &ss);
    strata_publish::gate::check(
        &ctx,
        &g,
        &strata_publish::merge_base(ctx.git, ctx.cfg).unwrap(),
    )
    .unwrap();
    let changed = fx
        .sb
        .commit(&main, &[("upstream.txt", b"upstream")], "changed tree");
    fx.sb.git(&main, &["push", "origin", "HEAD:main"]);
    fx.git_of("tool").fetch("origin", "main").unwrap();
    let head = CommitOid::try_from(changed).unwrap();
    let base = ss.ensure_base(&repo_name("tool"), &head).unwrap();
    ss.move_workspace(&s, &fx.git_of("tool").repo_id(), base, head)
        .unwrap();
    let e = strata_publish::gate::check(
        &ctx,
        &g,
        &strata_publish::merge_base(ctx.git, ctx.cfg).unwrap(),
    )
    .unwrap_err();
    assert_eq!(e.code, Code::HeadMoved);
    assert_eq!(e.detail.as_ref().unwrap()["reason"], "baseline_moved");
    assert!(
        e.detail.as_ref().unwrap()["fix"]
            .as_str()
            .unwrap()
            .starts_with("git-strata publish --from")
    );
    let c = fx.checkpoint(&s, "tool");
    let g = group(vec![batch(&s, "tool", c.id, &["mine.txt"])]);
    run(&fx, &g, &Messages::default()).unwrap();
}

#[test]
fn second_publish_uses_b_when_main_tip_does_not_include_first_publish() {
    for split in [false, true] {
        let mut fx = fx();
        fx.cfg_mut("tool").pin_unpushed = false;
        if split {
            fx.sb
                .git(&fx.main("tool"), &["push", "origin", "HEAD:release"]);
            fx.cfg_mut("tool").publish.branch = "release".into();
            assert!(fx.read_ref("tool", "refs/strata/remotes/origin/release").is_none());
        }
        let s = fx.new_session(&["tool"]);
        fx.edit(&s, "tool", &[("first", b"first")]);
        let c = fx.checkpoint(&s, "tool");
        let one = run(
            &fx,
            &group(vec![batch(&s, "tool", c.id, &["first"])]),
            &Messages::default(),
        )
        .unwrap();
        fx.edit(&s, "tool", &[("second", b"second")]);
        let c = fx.checkpoint(&s, "tool");
        let two = run(
            &fx,
            &group(vec![batch(&s, "tool", c.id, &["second"])]),
            &Messages::default(),
        )
        .unwrap();
        assert!(
            fx.git_of("tool")
                .is_ancestor(&one.commit, &two.commit)
                .unwrap()
        );
    }
}

#[test]
fn recovery_prefers_exact_trailers_and_never_uses_subject_with_wrong_trailer() {
    for (custom, trailer, found) in [
        (true, true, true),
        (false, false, true),
        (false, true, false),
    ] {
        let fx = fx();
        let s = fx.new_session(&["tool"]);
        fx.edit(&s, "tool", &[("mine", b"mine")]);
        let c = fx.checkpoint(&s, "tool");
        let g = group(vec![batch(&s, "tool", c.id, &["mine"])]);
        let id = fx
            .store()
            .record_publish(&g, std::time::SystemTime::now())
            .unwrap();
        fx.store()
            .set_publish_phase(
                id,
                strata_store::PublishPhase::Running,
                strata_store::PublishPhase::Unknown,
                Some(&serde_json::json!({})),
                std::time::SystemTime::now(),
            )
            .unwrap();
        let rec = fx.store().session(&s).unwrap().unwrap();
        let ws = &rec.manifest.repos[&repo_name("tool")];
        let subject = if custom {
            "custom".to_string()
        } else {
            format!("strata publish {id} (tool)")
        };
        let text = if trailer {
            format!(
                "{subject}\n\nsessions: {s}\nStrata-Publish: {} (tool)\n",
                if custom { id } else { id + 99 }
            )
        } else {
            subject
        };
        let commit = fx
            .git_of("tool")
            .commit_tree(&ws.base.tree, &ws.head, &text)
            .unwrap();
        fx.sb.git(
            &fx.main("tool"),
            &["update-ref", "refs/strata/publish/main", commit.as_str()],
        );
        fx.sb.git(
            &fx.main("tool"),
            &["update-ref", "refs/strata/anchor/main", ws.head.as_str()],
        );
        let d = fx.deps_ctx("tool");
        let ss = fx.sessions();
        let ctx = fx.ctx(&d, &ss);
        strata_publish::pending::reconstruct(strata_publish::pending::Scope::of(&ctx)).unwrap();
        let pending = fx
            .store()
            .rebase_pending_of(&s, &repo_name("tool"))
            .unwrap();
        assert_eq!(
            pending.as_ref().and_then(|p| p.head.as_deref()),
            found.then_some(commit.as_str())
        );
    }
}

#[test]
fn derived_multi_session_subject_and_utf8_body_cap_and_flag_precedence() {
    let fx = fx();
    let a = fx.new_session(&["tool"]);
    let b = fx.new_session(&["tool"]);
    fx.edit(&a, "tool", &[("a", b"a")]);
    fx.edit(&b, "tool", &[("b", b"b")]);
    manual(&fx, &a, &"é".repeat(25000));
    let ca = manual(&fx, &a, "x");
    manual(&fx, &b, &"界".repeat(17000));
    let cb = manual(&fx, &b, "y");
    let g = group(vec![
        batch(&a, "tool", ca.id, &["a"]),
        batch(&b, "tool", cb.id, &["b"]),
    ]);
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let ctx = fx.ctx(&d, &ss);
    let chosen = strata_publish::message::choose(&ctx, &g, &Messages::default()).unwrap();
    assert_eq!(chosen.subject(0, &repo_name("tool")), "x; y");
    assert!(chosen.render(&g, &repo_name("tool"), 2).len() <= 60 * 1024);
    assert!(
        chosen
            .text
            .as_ref()
            .unwrap()
            .contains("more messages omitted")
    );
    let flag = Messages {
        message: Some("flag".into()),
        ..Default::default()
    };
    assert_eq!(
        strata_publish::message::choose(&ctx, &g, &flag)
            .unwrap()
            .source,
        Source::Flag
    );
}

#[test]
fn stopped_publish_does_not_consume_but_committed_unknown_does() {
    let fx = fx();
    let s = fx.new_session(&["tool"]);
    fx.edit(&s, "tool", &[("a", b"a")]);
    let c = manual(&fx, &s, "keep me");
    let g = group(vec![batch(&s, "tool", c.id, &["a"])]);
    let id = fx
        .store()
        .record_publish(&g, std::time::SystemTime::now())
        .unwrap();
    fx.store()
        .set_publish_phase(
            id,
            strata_store::PublishPhase::Running,
            strata_store::PublishPhase::Done,
            Some(&serde_json::json!({"stopped":true})),
            std::time::SystemTime::now(),
        )
        .unwrap();
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let ctx = fx.ctx(&d, &ss);
    assert_eq!(
        strata_publish::message::choose(&ctx, &g, &Messages::default())
            .unwrap()
            .subject(0, &repo_name("tool")),
        "keep me"
    );
    let id = fx
        .store()
        .record_publish(&g, std::time::SystemTime::now())
        .unwrap();
    fx.store()
        .set_publish_phase(
            id,
            strata_store::PublishPhase::Running,
            strata_store::PublishPhase::Unknown,
            Some(&serde_json::json!({"commit":"a".repeat(40)})),
            std::time::SystemTime::now(),
        )
        .unwrap();
    assert_eq!(
        strata_publish::message::choose(&ctx, &g, &Messages::default())
            .unwrap()
            .source,
        Source::Legacy
    );
}

#[test]
fn duplicate_latest_checkpoint_still_supplies_the_subject() {
    let fx = fx();
    let s = fx.new_session(&["tool"]);
    fx.edit(&s, "tool", &[("a", b"a")]);
    manual(&fx, &s, "a");
    manual(&fx, &s, "b");
    let c = manual(&fx, &s, "a");
    let g = group(vec![batch(&s, "tool", c.id, &["a"])]);
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let ctx = fx.ctx(&d, &ss);
    let chosen = strata_publish::message::choose(&ctx, &g, &Messages::default()).unwrap();
    assert_eq!(chosen.subject(0, &repo_name("tool")), "a");
    assert_eq!(chosen.text.unwrap().matches("- a").count(), 1);
}

#[test]
fn a_user_divider_does_not_hide_the_generated_identity() {
    for message in [
        "subject\n\n---\nbody with divider",
        "subject\n\n Co-authored-by : Person",
        "subject\n\u{a0}\nCo-authored-by: Person",
        "subject\n\nCo-authored-by\u{a0}: Person",
    ] {
        let fx = fx();
        let s = fx.new_session(&["tool"]);
        fx.edit(&s, "tool", &[("a", b"a")]);
        let c = fx.checkpoint(&s, "tool");
        let g = group(vec![batch(&s, "tool", c.id, &["a"])]);
        let m = Messages {
            message: Some(message.into()),
            ..Default::default()
        };
        let p = run(&fx, &g, &m).unwrap();
        let parent = fx
            .git_of("tool")
            .resolve_rev(&format!("{}^", p.commit))
            .unwrap()
            .unwrap();
        let rows = fx
            .git_of("tool")
            .commits_between_trailers(&parent, &p.commit, 20)
            .unwrap();
        assert_eq!(rows[0].publishes, vec![format!("{} (tool)", p.id)]);
        assert!(
            fx.git_of("tool")
                .history_between(&parent, &p.commit, 20)
                .unwrap()[0]
                .sessions
                .contains(&s.to_string())
        );
    }
}

#[test]
fn deps_only_generation_change_keeps_the_checkpoint_baseline_acceptable() {
    let fx = fx();
    let s = fx.new_session(&["tool"]);
    fx.edit(&s, "tool", &[("mine", b"mine")]);
    let c = fx.checkpoint(&s, "tool");
    let repo = repo_name("tool");
    let record = fx.store().session(&s).unwrap().unwrap();
    let ws = &record.manifest.repos[&repo];
    let id = strata_types::DepsSetId::try_from("f".repeat(64)).unwrap();
    let at = std::time::SystemTime::now();
    fx.store()
        .record_deps_set(
            &strata_types::DepsSet {
                id: id.clone(),
                repo: repo.clone(),
                inputs_hash: "f".repeat(64),
                platform: "test".into(),
                toolchain: "test".into(),
                dirs: vec![],
            },
            strata_store::DepsOrigin::Empty,
            None,
            at,
        )
        .unwrap();
    let mut switched = ws.base.clone();
    switched.deps = id;
    let manifest = fx.store().manifest(&repo, &switched.tree).unwrap().unwrap();
    fx.store()
        .record_base_gen(&switched, &manifest, at)
        .unwrap();
    // Exercise the gate on the metadata of a completed deps-only switch;
    // this component test does not run the filesystem mover.
    let n = fx
        .store()
        .begin_move(
            &s,
            &repo,
            &switched,
            &ws.head,
            &ws.path.with_file_name(".deps-only"),
            at,
        )
        .unwrap();
    fx.store()
        .set_move_state(
            &s,
            &repo,
            n,
            strata_store::MoveState::Preparing,
            strata_store::MoveState::Swapping,
            Some(&[]),
            at,
        )
        .unwrap();
    fx.store().finish_move(&s, &repo, n, &[], at).unwrap();
    assert_ne!(c.base.deps, fx.base_of(&s, "tool").deps);
    let group = group(vec![batch(&s, "tool", c.id, &["mine"])]);
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let ctx = fx.ctx(&d, &ss);
    strata_publish::gate::check(
        &ctx,
        &group,
        &strata_publish::merge_base(ctx.git, ctx.cfg).unwrap(),
    )
    .unwrap();
}
