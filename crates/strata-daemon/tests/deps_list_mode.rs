//! Proposal display policy uses the same effective pins as main-follow, even without sessions.
mod support;

use serde_json::{Value, json};
use std::{fs, time::SystemTime};
use strata_confirm::seams::Verdict;
use strata_daemon::{
    handlers::{Server, Trackers},
    rpc::{Method, Params, Request},
    startup::{Core, Daemon},
};
use strata_store::NewProposal;
use strata_types::{CommitOid, DepsSetId, ProposalId, SyncDeps, TreeOid};
use support::*;

fn write_pin(fx: &Dfx, mode: Option<SyncDeps>) {
    let mut pins: Value = serde_json::from_str(&pins_json(&fx.config)).unwrap();
    if let Some(mode) = mode {
        pins["repos"]["tool"]["sync_deps"] = json!(mode);
    }
    fs::write(
        strata_daemon::pins_path(&fx.config_dir),
        serde_json::to_vec(&pins).unwrap(),
    )
    .unwrap();
}

fn proposal(fx: &Dfx, core: &Core) -> ProposalId {
    let main = fx.main("tool");
    core.store
        .record_proposal(
            &NewProposal {
                repo: repo_name("tool"),
                origin: None,
                from: DepsSetId::try_from("1".repeat(64)).unwrap(),
                from_inputs: "2".repeat(64),
                to: DepsSetId::try_from("3".repeat(64)).unwrap(),
                to_inputs: "4".repeat(64),
                head: Some(CommitOid::try_from(fx.sb.head(&main)).unwrap()),
                tree: Some(TreeOid::try_from(fx.sb.tree_of(&main, "HEAD")).unwrap()),
                from_head: None,
            },
            None,
            SystemTime::now(),
        )
        .unwrap()
        .unwrap()
}

fn assert_mode(server: &Server<'_, '_>, id: ProposalId, expected: SyncDeps) {
    for params in [json!({"repo": null}), json!({"repo": "tool"})] {
        let reply = server
            .handle(Request {
                id: json!(1),
                method: Method::DepsList,
                params: Params::parse(Method::DepsList, params).unwrap(),
            })
            .0;
        assert_eq!(reply["ok"], true, "{reply:#}");
        let rows = reply["data"]["proposals"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{reply:#}");
        assert_eq!(rows[0]["id"], id.get());
        assert_eq!(rows[0]["state"]["kind"], "Pending");
        assert_eq!(rows[0]["mode"], json!(expected), "{reply:#}");
    }
}

#[test]
fn deps_list_mode_uses_effective_pin_and_config() {
    for (pin, configured, expected) in [
        (SyncDeps::Apply, SyncDeps::Apply, SyncDeps::Apply),
        (SyncDeps::Confirm, SyncDeps::Apply, SyncDeps::Confirm),
        (SyncDeps::Apply, SyncDeps::Confirm, SyncDeps::Confirm),
        (SyncDeps::Confirm, SyncDeps::Confirm, SyncDeps::Confirm),
    ] {
        let mut fx = Dfx::new();
        fx.config
            .repos
            .get_mut(&repo_name("tool"))
            .unwrap()
            .sync
            .deps = configured;
        fx.write_config();
        write_pin(&fx, Some(pin));
        let (confirmer, log, _) = fx.confirmer(vec![]);
        let core = fx.prepare_with(confirmer).unwrap();
        let d = Daemon::start(&core).unwrap();
        let trackers = Trackers::new(&core);
        let server = Server::new(&d, &trackers);
        let id = proposal(&fx, &core);
        assert!(core.store.sessions().unwrap().is_empty());
        assert_mode(&server, id, expected);
        assert!(log.shown().is_empty());
    }
}

#[test]
fn deps_list_mode_preserves_legacy_and_missing_pin_rules() {
    for configured in [SyncDeps::Apply, SyncDeps::Confirm] {
        for legacy in [false, true] {
            let mut fx = Dfx::new();
            fx.config
                .repos
                .get_mut(&repo_name("tool"))
                .unwrap()
                .sync
                .deps = configured;
            fx.write_config();
            if legacy {
                write_pin(&fx, None);
            }
            let (confirmer, log, _) = fx.confirmer(vec![]);
            let core = fx.prepare_unseeded(confirmer).unwrap();
            let d = Daemon::start(&core).unwrap();
            let trackers = Trackers::new(&core);
            let server = Server::new(&d, &trackers);
            let id = proposal(&fx, &core);
            assert_mode(
                &server,
                id,
                if legacy {
                    configured
                } else {
                    SyncDeps::Confirm
                },
            );
            assert!(log.shown().is_empty());
        }
    }
}

#[test]
fn deps_list_mode_tracks_relax_without_restarting() {
    let mut fx = Dfx::new();
    fx.config
        .repos
        .get_mut(&repo_name("tool"))
        .unwrap()
        .sync
        .deps = SyncDeps::Apply;
    fx.write_config();
    write_pin(&fx, Some(SyncDeps::Confirm));
    let (confirmer, log, _) = fx.confirmer(vec![Verdict::pass()]);
    let core = fx.prepare_with(confirmer).unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let id = proposal(&fx, &core);
    assert_mode(&server, id, SyncDeps::Confirm);
    core.config_relax(&repo_name("tool")).unwrap();
    assert_mode(&server, id, SyncDeps::Apply);
    assert_eq!(log.shown().len(), 1);
}
