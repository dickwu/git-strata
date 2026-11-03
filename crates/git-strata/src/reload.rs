//! Agent-safe config reload; the report is the standard envelope's data.
use crate::{
    Result,
    ctx::Ctx,
    envelope::{self, Envelope},
    rpc::Kind,
};
use serde_json::{Value, json};
use strata_types::{Code, StrataError};

pub(crate) const OLD_DAEMON: &str =
    "this daemon loads config only at start; ask the owner to restart it";

pub(crate) fn supported(ctx: &Ctx) -> Result<bool> {
    let status = ctx.call("daemon_status", json!({}), Kind::Read)?;
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
            (
                Envelope::err_with(
                    StrataError::new(Code::InvalidInput, message)
                        .with_detail(json!({"reason":"config_rejected"})),
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
            Envelope::err(StrataError::new(
                Code::InvalidInput,
                "invalid config reload report",
            )),
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

pub(crate) fn run() -> i32 {
    let (env, code) = match Ctx::from_env().and_then(|ctx| request(&ctx)) {
        Ok(r) => r,
        Err(e) => (Envelope::err(e), 1),
    };
    if let Some(e) = &env.error {
        eprintln!("git-strata: {}", e.message);
    }
    if let Err(e) = envelope::emit(&env, &mut std::io::stdout().lock()) {
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
}
