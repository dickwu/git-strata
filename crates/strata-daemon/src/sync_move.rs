/// Replaced by part `sync`. Kept separate so the read-only dispatcher and
/// CLI can land without advertising support for sync moves yet.
pub fn run(
    _daemon: &crate::startup::Daemon<'_>,
    _trackers: &crate::handlers::Trackers<'_>,
    _params: &crate::rpc::SyncParams,
) -> crate::Result<serde_json::Value> {
    Err(strata_types::StrataError::new(
        strata_types::Code::Unsupported,
        "sync moves are not implemented by this daemon",
    )
    .with_detail(serde_json::json!({ "reason": "not_implemented" })))
}
