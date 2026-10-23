//! confirm.md §7 `prompt_reason_names_the_action` (§5.6).

use std::path::PathBuf;

use strata_confirm::prompt_reason;
use strata_types::{Action, ProposalId, RepoId, RepoName, SessionId};

fn repo(name: &str) -> RepoName {
    RepoName::try_from(name.to_string()).unwrap()
}

fn push(r: &str, branch: &str) -> Action {
    Action::PushDeploy {
        repo: repo(r),
        branch: branch.to_string(),
    }
}

fn printable_ascii(s: &str) -> bool {
    s.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

#[test]
fn prompt_reason_names_the_action() {
    assert_eq!(prompt_reason(&push("tool", "main")), "push tool to origin/main");
    assert_eq!(
        prompt_reason(&Action::DepsApprove {
            proposal: ProposalId::from(12i64)
        }),
        "approve dependency proposal 12"
    );
    assert_eq!(
        prompt_reason(&Action::RemoveDiscard {
            session: SessionId::try_from("s-k3q7xm2a".to_string()).unwrap()
        }),
        "discard the unpublished changes of session s-k3q7xm2a and remove it"
    );
    assert_eq!(
        prompt_reason(&Action::ConfigRelax {
            repo: RepoId::new(PathBuf::from("/Volumes/GWD/inter/tool"))
        }),
        "apply relaxed push settings for /Volumes/GWD/inter/tool"
    );

    // Escaping: bidi override, zero-width space, newline, backslash.
    assert_eq!(
        prompt_reason(&push("tool", "a\u{202E}b\u{200B}c\nd\\e")),
        "push tool to origin/a\\u{202E}b\\u{200B}c\\u{A}d\\\\e"
    );

    // Two PushDeploy that differ only in branch read differently.
    assert_ne!(
        prompt_reason(&push("tool", "main")),
        prompt_reason(&push("tool", "release"))
    );

    // Any input: non-empty, printable ASCII only.
    let nasty = [
        "",
        "\u{0}",
        "\u{7f}",
        "\t\r\n",
        "é中文🚀",
        "\u{202E}\u{2066}\u{FEFF}",
        "x".repeat(4096).as_str().to_owned().leak(),
    ];
    for b in nasty {
        let r = prompt_reason(&push("tool", b));
        assert!(!r.is_empty());
        assert!(printable_ascii(&r), "not printable ASCII: {r:?}");
        let r = prompt_reason(&Action::ConfigRelax {
            repo: RepoId::new(PathBuf::from(format!("/tmp/{b}"))),
        });
        assert!(!r.is_empty());
        assert!(printable_ascii(&r), "not printable ASCII: {r:?}");
    }
    // No truncation.
    let long = "x".repeat(4096);
    assert!(prompt_reason(&push("tool", &long)).ends_with(&long));
}
