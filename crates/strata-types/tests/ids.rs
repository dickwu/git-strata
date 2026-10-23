use strata_types::{
    BlobId, CheckpointId, CommitOid, DepsSetId, ProposalId, RepoId, RepoName, RepoPath, SessionId,
    TreeOid, ViewId,
};

fn name(s: &str) -> Result<RepoName, strata_types::StrataError> {
    RepoName::try_from(s.to_owned())
}

#[test]
fn repo_name_accepts_checkout_directory_names() {
    for raw in ["tool", "staff-api", "foo.bar", "a_b", "A1"] {
        let n = name(raw).unwrap();
        assert_eq!(n.to_string(), raw);
        assert_eq!(n.as_str(), raw);
    }
}

#[test]
fn repo_name_rejects_empty_dot_and_illegal_chars() {
    for raw in ["", ".", "..", "foo/bar", "foo bar", "foo:bar", "ü"] {
        let err = name(raw).unwrap_err();
        assert_eq!(err.code, strata_types::Code::InvalidInput);
    }
}

#[test]
fn session_id_is_s_plus_eight_base32() {
    let id = SessionId::try_from("s-k3q7xm2a".to_owned()).unwrap();
    assert_eq!(id.to_string(), "s-k3q7xm2a");
    for raw in [
        "k3q7xm2a",
        "s-k3q7xm2",
        "s-k3q7xm2a1",
        "s-K3Q7XM2A",
        "s-k3q7xm21",
    ] {
        assert_eq!(
            SessionId::try_from(raw.to_owned()).unwrap_err().code,
            strata_types::Code::InvalidInput,
            "{raw}"
        );
    }
}

#[test]
fn git_oids_are_forty_lowercase_hex_and_blobs_are_sixty_four() {
    let tree = "0123456789abcdef0123456789abcdef01234567";
    assert_eq!(
        TreeOid::try_from(tree.to_owned()).unwrap().to_string(),
        tree
    );
    assert_eq!(
        CommitOid::try_from(tree.to_owned()).unwrap().to_string(),
        tree
    );
    let blob = "a".repeat(64);
    assert_eq!(BlobId::try_from(blob.clone()).unwrap().to_string(), blob);
    assert_eq!(DepsSetId::try_from(blob).unwrap().as_str().len(), 64);
    for raw in [
        "ABCDEF0123456789abcdef0123456789abcdef01",
        &"a".repeat(64),
        "abc",
        "",
    ] {
        assert!(TreeOid::try_from(raw.to_owned()).is_err(), "{raw}");
        assert!(CommitOid::try_from(raw.to_owned()).is_err(), "{raw}");
    }
    assert!(BlobId::try_from(tree.to_owned()).is_err());
    assert!(DepsSetId::try_from("g".repeat(64)).is_err());
}

#[test]
fn repo_path_normalizes_nfc_and_rejects_non_relative() {
    let nfd = "caf\u{0065}\u{0301}/page.tsx";
    let path = RepoPath::try_from(nfd.to_owned()).unwrap();
    assert_eq!(path.as_str(), "caf\u{00e9}/page.tsx");
    assert_eq!(path.to_string(), "caf\u{00e9}/page.tsx");
    for raw in [
        "",
        "/",
        "/abs",
        "foo/",
        "foo//bar",
        ".",
        "..",
        "foo/../bar",
        "foo/./bar",
        "foo/.",
        ".git/../x",
    ] {
        assert!(
            RepoPath::try_from(raw.to_owned()).is_err(),
            "accepted {raw:?}"
        );
    }
    assert_eq!(
        RepoPath::try_from("src/app.tsx".to_owned())
            .unwrap()
            .as_str(),
        "src/app.tsx"
    );
}

#[test]
fn numeric_ids_reject_values_sqlite_cannot_store() {
    assert_eq!(CheckpointId::try_from(1_u64).unwrap().get(), 1);
    assert_eq!(ProposalId::try_from(0_u64).unwrap().get(), 0);
    let too_big = (i64::MAX as u64) + 1;
    assert_eq!(
        CheckpointId::try_from(too_big).unwrap_err().code,
        strata_types::Code::InvalidInput
    );
    assert_eq!(
        ProposalId::try_from(too_big).unwrap_err().code,
        strata_types::Code::InvalidInput
    );
    assert_eq!(CheckpointId::from(7_i64).get(), 7);
    assert_eq!(CheckpointId::from(7_i64).to_string(), "7");
}

#[test]
fn repo_id_and_view_id_are_opaque_wrappers() {
    let id = RepoId::new("/Volumes/GWD/inter/tool/.git".into());
    assert_eq!(
        id.get(),
        std::path::Path::new("/Volumes/GWD/inter/tool/.git")
    );
    assert_eq!(id.to_string(), "/Volumes/GWD/inter/tool/.git");
    let view = ViewId::new("view-1".to_owned());
    assert_eq!(view.get(), "view-1");
    assert_eq!(view.to_string(), "view-1");
}

#[test]
fn ids_round_trip_through_json() {
    let name = name("tool").unwrap();
    let json = serde_json::to_string(&name).unwrap();
    assert_eq!(json, "\"tool\"");
    let back: RepoName = serde_json::from_str(&json).unwrap();
    assert_eq!(back, name);
    let bad = serde_json::from_str::<RepoName>("\"..\"");
    assert!(bad.is_err());
    let id = CheckpointId::try_from(3_u64).unwrap();
    assert_eq!(serde_json::to_string(&id).unwrap(), "3");
    assert!(serde_json::from_str::<CheckpointId>(&format!("{}", (i64::MAX as u64) + 1)).is_err());
}
