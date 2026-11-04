//! Agent-safe config reload; the report is the standard envelope's data.
use crate::{
    Result,
    ctx::Ctx,
    envelope::{self, Envelope},
    rpc::{self, Kind},
};
use serde_json::{Value, json};
use strata_types::{Code, StrataError};

pub(crate) const OLD_DAEMON: &str =
    "this daemon loads config only at start; ask the owner to restart it";

pub(crate) fn supported(ctx: &Ctx) -> Result<bool> {
    let status = rpc::call_within(
        &ctx.paths.socket,
        "daemon_status",
        json!({}),
        Kind::Read,
        crate::agent::PROBE_DEADLINE,
    )
    .map_err(rpc::CallError::into_error)?;
    Ok(status["features"]
        .as_array()
        .is_some_and(|f| f.iter().any(|s| s == "config.reload")))
}

pub(crate) fn result(data: Value) -> (Envelope, i32) {
    match data["outcome"].as_str() {
        Some("applied" | "unchanged") => (Envelope::ok(data), 0),
        Some("rejected") => {
            let message = data["reasons"][0]["message"]
                .as_str()
                .unwrap_or("config.toml was rejected")
                .to_owned();
            let reason = data["reasons"][0]["detail"]["reason"]
                .as_str()
                .unwrap_or("config_rejected")
                .to_owned();
            (
                Envelope::err_with(
                    StrataError::new(Code::InvalidInput, message)
                        .with_detail(json!({"reason":reason})),
                    data,
                ),
                2,
            )
        }
        Some("pending") => (
            Envelope::err_with(
                StrataError::new(
                    Code::Busy,
                    "reload still running; rerun `git-strata config reload`",
                )
                .with_detail(json!({"reason":"reload_pending"})),
                data,
            ),
            3,
        ),
        _ => (
            Envelope::err_with(
                StrataError::new(Code::InvalidInput, "invalid config reload report")
                    .with_detail(json!({"reason":"invalid_reload_report"})),
                data,
            ),
            1,
        ),
    }
}

pub(crate) fn request(ctx: &Ctx) -> Result<(Envelope, i32)> {
    if !supported(ctx)? {
        return Ok((
            Envelope::err(
                StrataError::new(Code::Unsupported, OLD_DAEMON)
                    .with_detail(json!({"reason":"daemon_too_old"})),
            ),
            4,
        ));
    }
    Ok(result(ctx.call("config_reload", json!({}), Kind::Write)?))
}

pub(crate) fn emit(env: &Envelope, code: i32) -> i32 {
    if let Some(e) = &env.error {
        eprintln!("git-strata: {}", e.message);
    }
    if let Err(e) = envelope::emit(env, &mut std::io::stdout().lock()) {
        eprintln!("git-strata: stdout_write_failed: {e}");
        return 1;
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn report_envelope_and_exit_codes() {
        for (outcome, code) in [
            ("applied", 0),
            ("unchanged", 0),
            ("rejected", 2),
            ("pending", 3),
        ] {
            let (env, actual) = result(json!({"outcome":outcome,"repos":["new"]}));
            assert_eq!(actual, code);
            assert_eq!(env.ok, code == 0);
            assert_eq!(env.data["repos"][0], "new");
            assert_eq!(
                serde_json::from_str::<Value>(&env.line()).unwrap()["data"]["outcome"],
                outcome
            );
        }
    }

    #[test]
    fn rejection_reasons_and_unknown_reports_are_preserved() {
        for reason in ["parse", "restart_required", "stale_rows", "spawn_failed"] {
            let report = json!({"outcome":"rejected", "reasons":[{"message":"refused", "detail":{"reason":reason}}]});
            let (env, code) = result(report.clone());
            assert_eq!(code, 2);
            assert_eq!(env.error.unwrap().detail.unwrap()["reason"], reason);
            assert_eq!(env.data, report);
        }
        let report = json!({"outcome":"future_outcome", "generation":99});
        let (env, code) = result(report.clone());
        assert_eq!(code, 1);
        assert_eq!(env.data, report);
        assert_eq!(
            env.error.unwrap().detail.unwrap()["reason"],
            "invalid_reload_report"
        );
    }
}
