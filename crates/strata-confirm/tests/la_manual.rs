//! confirm.md §7 `manual_prompt_round_trip`: needs a human at the Mac.
//!
//! cargo test -p strata-confirm --features test-seams --test la_manual -- --ignored --nocapture
#![cfg(target_os = "macos")]

use std::time::Duration;

use strata_confirm::Confirmer;
use strata_confirm::seams::LaOptions;
use strata_types::{Action, RepoName};

fn push() -> Action {
    Action::PushDeploy {
        repo: RepoName::try_from("tool".to_string()).unwrap(),
        branch: "main".to_string(),
        url: "git@github.com:acme/tool.git".to_string(),
    }
}

fn reason(e: &strata_types::StrataError) -> String {
    e.detail
        .as_ref()
        .and_then(|d| d.get("reason"))
        .and_then(|r| r.as_str())
        .unwrap_or("")
        .to_string()
}

#[test]
#[ignore = "shows real Touch ID / password prompts; needs a human"]
fn manual_prompt_round_trip() {
    let c = Confirmer::with_la(LaOptions {
        interaction_not_allowed: false,
        invalidate_first: false,
        prompt_timeout: Duration::from_secs(10),
    });
    eprintln!(
        "Prompt 1 of 3: check it says '… is trying to push tool to origin/main at git@github.com:acme/tool.git.', then PASS it (Touch ID or password)."
    );
    let token = c.require(push()).expect("prompt 1 should pass");
    c.validate(&token, &push()).expect("token validates");

    eprintln!("Prompt 2 of 3: press CANCEL.");
    let e = c.require(push()).expect_err("prompt 2 cancelled");
    assert_eq!(reason(&e), "cancelled", "{e:?}");

    eprintln!("Prompt 3 of 3: do NOT answer; it should close itself after 10 s.");
    let e = c.require(push()).expect_err("prompt 3 ignored");
    assert_eq!(reason(&e), "timeout", "{e:?}");
    assert_eq!(c.record_count(), 1);
}
