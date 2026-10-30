//! confirm.md §7 `prompt_reason_names_the_action` (§5.6).

use std::path::PathBuf;

use strata_confirm::prompt_reason;
use strata_types::{Action, ProposalId, RepoId, RepoName, SessionId};

fn repo(name: &str) -> RepoName {
    RepoName::try_from(name.to_string()).unwrap()
}

fn push(r: &str, branch: &str) -> Action {
    push_at(r, branch, "git@github.com:inter/tool.git")
}

fn push_at(r: &str, branch: &str, url: &str) -> Action {
    Action::PushDeploy {
        repo: repo(r),
        branch: branch.to_string(),
        url: url.to_string(),
    }
}

fn printable_ascii(s: &str) -> bool {
    s.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

#[test]
fn prompt_reason_names_the_action() {
    assert_eq!(
        prompt_reason(&push("tool", "main")),
        "push tool to origin/main at git@github.com:inter/tool.git"
    );
    // Review P1-15: the URL is on the prompt, its userinfo redacted.
    assert_eq!(
        prompt_reason(&push_at("tool", "main", "https://x-access-token:ghp_secret@github.com/inter/tool.git")),
        "push tool to origin/main at https://***@github.com/inter/tool.git"
    );
    assert_ne!(
        prompt_reason(&push_at("tool", "main", "git@github.com:inter/tool.git")),
        prompt_reason(&push_at("tool", "main", "git@github.com:evil/tool.git"))
    );
    assert_eq!(
        prompt_reason(&Action::DepsApprove {
            proposal: ProposalId::from(12i64)
        }),
        "approve dependency proposal 12"
    );
    assert_eq!(
        prompt_reason(&Action::RemoveDiscard {
            session: SessionId::try_from("s-k3q7xm2a".to_string()).unwrap(), kept_trees: vec![]
        }),
        "discard the unpublished changes of session s-k3q7xm2a and remove it"
    );
    assert_eq!(
        prompt_reason(&Action::RemoveDiscard {
            session: SessionId::try_from("s-k3q7xm2a".to_string()).unwrap(),
            kept_trees: vec![
                strata_types::KeptTreeRef { path: "/s/.repos/x/tool.old".into(), count: 3 },
                strata_types::KeptTreeRef { path: "/s/t".into(), count: 0 },
            ],
        }),
        "discard the unpublished changes of session s-k3q7xm2a and remove it; \
         also discards 3 late-written files in the kept pre-move tree /s/.repos/x/tool.old; \
         also discards the kept pre-move tree /s/t"
    );
    assert_eq!(
        prompt_reason(&Action::ConfigRelax {
            repo: RepoId::new(PathBuf::from("/Volumes/Code/src/tool"))
        }),
        "apply relaxed push settings for /Volumes/Code/src/tool"
    );

    // Escaping: bidi override, zero-width space, newline, backslash.
    assert_eq!(
        prompt_reason(&push("tool", "a\u{202E}b\u{200B}c\nd\\e")),
        "push tool to origin/a\\u{202E}b\\u{200B}c\\u{A}d\\\\e at git@github.com:inter/tool.git"
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
    assert!(prompt_reason(&push_at("tool", "main", &long)).ends_with(&long));
}
