use serde_json::json;
use strata_types::*;

#[test]
fn upstream_optional_fields_omit_and_round_trip() {
    let head = "a".repeat(40);
    let row: RepoUpstream = serde_json::from_value(json!({
        "following": true, "capped": false, "unchecked": true,
        "diverged": false, "base_head": head
    }))
    .unwrap();
    let value = serde_json::to_value(&row).unwrap();
    assert!(value.get("behind").is_none());
    for key in [
        "main",
        "deps",
        "auto",
        "unpublished",
        "fix",
        "commits",
        "base_only",
        "conflicts",
    ] {
        assert!(value.get(key).is_none(), "{key}");
    }
    let mut row = row;
    row.behind = Some(0);
    row.unpublished = Some(false);
    row.deps = Some(UpstreamDeps {
        proposal: ProposalId::try_from(75u64).unwrap(),
        mode: SyncDeps::Apply,
        state: "ready".into(),
        from: DepsSetId::try_from("a".repeat(64)).unwrap(),
        to: DepsSetId::try_from("b".repeat(64)).unwrap(),
        summary: None,
    });
    let upstream = Upstream {
        as_of: None,
        clean: false,
        repos: [(RepoName::try_from("tool".to_string()).unwrap(), row)]
            .into_iter()
            .collect(),
    };
    let value = serde_json::to_value(&upstream).unwrap();
    assert!(value.get("as_of").is_none());
    assert_eq!(value["repos"]["tool"]["behind"], 0);
    assert_eq!(value["repos"]["tool"]["unpublished"], false);
    assert_eq!(value["repos"]["tool"]["deps"]["mode"], "apply");
    assert_eq!(value["repos"]["tool"]["deps"]["proposal"], 75);
    assert!(value["repos"]["tool"]["deps"].get("summary").is_none());
    assert_eq!(serde_json::from_value::<Upstream>(value).unwrap(), upstream);
}
