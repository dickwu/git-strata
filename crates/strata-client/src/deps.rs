//! Additive deps_list enrichment. The transport remains lossless for unknown fields.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DepsList {
    pub proposals: Vec<DepsProposal>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DepsProposal {
    pub id: u64,
    pub repo: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changes: Option<Changes>,
    #[serde(flatten)]
    pub fields: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changes {
    pub summary: String,
    pub packages: Vec<PackageChange>,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageChange {
    pub name: String,
    pub from: Option<String>,
    pub to: Option<String>,
    pub kind: String,
    pub direct: bool,
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn enrichment_is_optional_and_unknown_proposal_fields_round_trip() {
        for changes in [
            None,
            Some(
                json!({"summary":"+zod 3", "packages":[{"name":"zod","from":null,"to":"3","kind":"added","direct":true}], "truncated":false,"since":"session_base"}),
            ),
        ] {
            let mut p = json!({"id":75,"repo":"tool","origin":{"kind":"Main"},"future":true});
            if let Some(changes) = changes {
                p["changes"] = changes;
            }
            let wire = json!({"proposals":[p]});
            let list: DepsList = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(serde_json::to_value(list).unwrap(), wire);
        }
    }
}
