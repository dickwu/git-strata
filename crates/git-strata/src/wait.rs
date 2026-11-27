//! Status/sync I/O deadlines, stderr progress and per-RPC refusal retry.
//! A timeout is terminal. The read-only diagnostic probe has a separate deadline.
use crate::{
    Result,
    args::WaitArgs,
    ctx::Ctx,
    rpc::{CallError, Kind},
};
use serde_json::{Value, json};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use strata_types::{Code, StrataError};

pub fn budget(args: &WaitArgs, default_secs: u64) -> Result<Option<Duration>> {
    let seconds = match args.timeout {
        Some(v) => v,
        None => match std::env::var("GIT_STRATA_TIMEOUT") {
            Ok(v) => v.parse().map_err(|_| invalid_timeout())?,
            Err(std::env::VarError::NotPresent) => default_secs as f64,
            Err(_) => return Err(invalid_timeout()),
        },
    };
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(invalid_timeout());
    }
    if seconds == 0.0 {
        return Ok(None);
    }
    Duration::try_from_secs_f64(seconds)
        .map(Some)
        .map_err(|_| invalid_timeout())
}
fn invalid_timeout() -> StrataError {
    StrataError::new(
        Code::InvalidInput,
        "timeout must be finite, non-negative seconds (0 disables the deadline)",
    )
    .with_detail(json!({ "reason": "timeout" }))
}

/// Parser fallbacks share one I/O deadline. A certified new attempt gets
/// another full status/sync window; diagnostic and hook deadlines are fixed.
pub struct Deadline {
    started: Instant,
    end: std::cell::Cell<Option<Instant>>,
    per_attempt: Option<Duration>,
}

impl Deadline {
    fn new(budget: Option<Duration>, per_attempt: Option<Duration>) -> Result<Self> {
        let started = Instant::now();
        let end = budget
            .map(|b| started.checked_add(b).ok_or_else(invalid_timeout))
            .transpose()?;
        Ok(Self {
            started,
            end: std::cell::Cell::new(end),
            per_attempt,
        })
    }

    pub(crate) fn fixed(budget: Duration) -> Result<Self> {
        Self::new(Some(budget), None)
    }

    pub(crate) fn remaining(&self) -> Option<Duration> {
        self.end
            .get()
            .map(|end| end.saturating_duration_since(Instant::now()))
    }

    fn fixed_end(&self) -> Option<Instant> {
        if self.per_attempt.is_some() {
            None
        } else {
            self.end.get()
        }
    }

    fn next_attempt(&self) -> Result<()> {
        if let Some(window) = self.per_attempt {
            let end = Instant::now()
                .checked_add(window)
                .ok_or_else(invalid_timeout)?;
            self.end.set(Some(end));
        }
        Ok(())
    }
}

pub fn deadline(args: &WaitArgs, default_secs: u64) -> Result<Deadline> {
    let budget = budget(args, default_secs)?;
    Deadline::new(budget, budget)
}

pub fn call(
    ctx: &Ctx,
    method: &str,
    params: Value,
    kind: Kind,
    args: &WaitArgs,
    default_secs: u64,
) -> std::result::Result<Value, CallError> {
    let deadline = deadline(args, default_secs).map_err(CallError::Failed)?;
    let session = params["session"].as_str().map(str::to_owned);
    with_progress(ctx, method, session, args, &deadline, || {
        ctx.call_until(method, params, kind, Some(&deadline))
    })
}

/// Timeouts report the whole operation, including any legacy parameter fallback.
pub(crate) fn with_progress(
    ctx: &Ctx,
    method: &str,
    session: Option<String>,
    args: &WaitArgs,
    deadline: &Deadline,
    invoke: impl FnOnce() -> std::result::Result<Value, CallError>,
) -> std::result::Result<Value, CallError> {
    let _progress = if args.progress || crate::tty::stderr_is_tty() {
        Progress::start(ctx.clone(), method.to_string(), session.clone())
    } else {
        None
    };
    match invoke() {
        Err(CallError::Timeout {
            method,
            phase,
            kind,
            sent,
            ..
        }) => {
            let mut error = CallError::Timeout {
                method,
                waited: deadline.started.elapsed(),
                phase,
                kind,
                sent,
            }
            .into_error();
            let detail = error.detail.as_mut().expect("timeout detail");
            if error.code == Code::Unknown {
                let command = session
                    .as_ref()
                    .map(|s| format!("git-strata status --session {s}"))
                    .unwrap_or_else(|| "git-strata status".into());
                detail["fix"] = json!(format!(
                    "{command} shows the result when it finishes; do not retry, gc, or rm"
                ));
            }
            match ctx.call_within(
                "daemon_status",
                json!({}),
                Kind::Read,
                Duration::from_secs(2),
            ) {
                Ok(v) => {
                    for key in ["holds", "queues"] {
                        if let Some(value) = v.get(key) {
                            detail[key] = value.clone();
                        }
                    }
                    if let Some(s) = session.as_ref()
                        && let Some(queue) = v["queues"]
                            .as_array()
                            .and_then(|q| q.iter().find(|q| q["session"].as_str() == Some(s)))
                    {
                        detail["session"] = json!({ "id": queue["session"], "in_flight": queue["holder"],
                            "since": queue["since"], "queued": queue["queued"] });
                    }
                }
                Err(_) => detail["daemon_status"] = json!("unresponsive"),
            }
            Err(CallError::Failed(error))
        }
        other => other,
    }
}

/// new-lock §4.1: `new`/`add`'s retry budget when neither `--timeout` nor
/// `GIT_STRATA_TIMEOUT` is given (0 there: unlimited).
pub const RETRY_DEFAULT_SECS: u64 = 600;
const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(10);
const RETRY_FIX: &str = "no session or workspace was created or changed (fetch records and tracking refs may have); rerun, or pass --timeout 0 to keep retrying";

/// Retry one RPC after an eligible pre-dispatch refusal. The retry budget is
/// checked between attempts. Status and sync also pass their I/O deadline.
pub(crate) struct RetryBudget {
    started: Instant,
    limit: Option<Duration>,
    next_delay: Duration,
    retries: u64,
}

impl RetryBudget {
    pub(crate) fn new(args: &WaitArgs) -> Result<Self> {
        Ok(Self::with_limit(budget(args, RETRY_DEFAULT_SECS)?))
    }

    fn with_limit(limit: Option<Duration>) -> Self {
        Self {
            started: Instant::now(),
            limit,
            next_delay: RETRY_FIRST,
            retries: 0,
        }
    }

    pub(crate) fn run<T>(
        &mut self,
        method: &str,
        args: &WaitArgs,
        deadline: Option<&Deadline>,
        attempt: impl FnMut() -> std::result::Result<T, CallError>,
    ) -> std::result::Result<T, CallError> {
        let show = args.progress || crate::tty::stderr_is_tty();
        let clock = Wall(self.started);
        self.run_with_clock(
            method,
            args.startup,
            deadline,
            &clock,
            &mut |line| {
                if show {
                    eprintln!("{line}");
                }
            },
            attempt,
        )
    }
}

trait Clock {
    fn elapsed(&self) -> Duration;
    fn sleep(&self, d: Duration);
}

struct Wall(Instant);
impl Clock for Wall {
    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }
    fn sleep(&self, d: Duration) {
        // A signal during rm's backoff belongs to the next (unsent) request.
        let end = Instant::now() + d;
        while !crate::interrupt::STOP.load(std::sync::atomic::Ordering::SeqCst) {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            std::thread::sleep(left.min(Duration::from_millis(100)));
        }
    }
}

impl RetryBudget {
    fn run_with_clock<T>(
        &mut self,
        method: &str,
        wait_startup: bool,
        deadline: Option<&Deadline>,
        clock: &dyn Clock,
        note: &mut dyn FnMut(String),
        mut attempt: impl FnMut() -> std::result::Result<T, CallError>,
    ) -> std::result::Result<T, CallError> {
        let until = deadline.and_then(Deadline::fixed_end);
        loop {
            let e = match attempt() {
                Err(CallError::Failed(e)) => e,
                other => return other,
            };
            let waited = clock.elapsed();
            if crate::interrupt::STOP.load(std::sync::atomic::Ordering::SeqCst)
                || !should_retry(method, &e, wait_startup)
            {
                if let Some(line) = holder_line(method, waited, &e) {
                    note(format!("{line}; not retried"));
                }
                return Err(CallError::Failed(e));
            }
            let mut left = self.limit.map(|b| b.saturating_sub(waited));
            let call_left = until.map(|end| end.saturating_duration_since(Instant::now()));
            if let Some(time) = call_left {
                left = Some(left.unwrap_or(time).min(time));
            }
            if left == Some(Duration::ZERO) {
                if let Some(line) = holder_line(method, waited, &e) {
                    note(format!("{line}; giving up"));
                }
                return Err(CallError::Failed(spent(e, self.retries, waited)));
            }
            if let Some(line) = holder_line(method, waited, &e) {
                note(format!("{line}; retrying"));
            }
            clock.sleep(left.map_or(self.next_delay, |l| l.min(self.next_delay)));
            // Diagnostic probes and the Stop hook have a fixed wall-clock limit.
            if until.is_some_and(|end| Instant::now() >= end) {
                return Err(CallError::Failed(spent(e, self.retries, clock.elapsed())));
            }
            if let Some(limit) = deadline {
                limit.next_attempt().map_err(CallError::Failed)?;
            }
            self.next_delay = (self.next_delay * 2).min(RETRY_MAX);
            self.retries += 1;
        }
    }
}

/// new-lock §4.1: only the daemon's certification makes a refusal
/// replay-safe; a bare `Busy lock_wait` (and every legacy daemon's) is not.
fn should_retry(method: &str, e: &StrataError, wait_startup: bool) -> bool {
    if e.code != Code::Busy {
        return false;
    }
    let Some(detail) = &e.detail else {
        return false;
    };
    match detail["reason"].as_str() {
        Some("daemon_down") => false,
        Some("starting" | "store_unavailable") => wait_startup,
        Some("upgrading") => detail["retry_safe"] == true,
        _ => matches!(method, "new" | "add") && detail["retry_safe"] == true,
    }
}

/// The last certified refusal once the budget is spent (new-lock §4.3).
fn spent(mut e: StrataError, retries: u64, waited: Duration) -> StrataError {
    if let Some(d) = e.detail.as_mut().and_then(Value::as_object_mut) {
        d.insert("retries".into(), json!(retries));
        d.insert("waited_ms".into(), json!(waited.as_millis() as u64));
        let fix = if d.get("reason").is_some_and(|v| v == "lock_wait") {
            RETRY_FIX
        } else {
            "this RPC was refused before it ran; rerun the command, or use --timeout 0 to keep waiting"
        };
        d.insert("fix".into(), json!(fix));
    }
    e
}

/// new-lock §4.3: `waiting (45s): new; repo tool held by sync (session
/// s-uijvlo3l, request, 2m10s)`, from a `lock_wait` refusal's detail; `held
/// by (handing over)` when the lock was between holders. None for any other
/// refusal.
fn holder_line(method: &str, waited: Duration, e: &StrataError) -> Option<String> {
    let d = e.detail.as_ref()?;
    match d["reason"].as_str()? {
        "upgrading" | "starting" | "store_unavailable" => {
            return Some(format!(
                "waiting ({}s): {method}; daemon {}",
                waited.as_secs(),
                d["reason"].as_str()?
            ));
        }
        "lock_wait" => {}
        _ => return None,
    }
    let lock = match (d["lock"]["repo"].as_str(), d["lock"]["session"].as_str()) {
        (Some(r), _) => format!("repo {r}"),
        (None, Some(s)) => format!("session {s}"),
        (None, None) => "the lock".to_string(),
    };
    let holder = match d["holder"].as_str() {
        None => "(handing over)".to_string(),
        Some(h) => {
            let mut about = Vec::new();
            if let Some(s) = d["holder_session"].as_str() {
                about.push(format!("session {s}"));
            }
            if let Some(o) = d["holder_origin"].as_str() {
                about.push(o.to_string());
            }
            if let Some(ms) = d["held_ms"].as_u64() {
                about.push(span(Duration::from_millis(ms)));
            }
            if about.is_empty() {
                h.to_string()
            } else {
                format!("{h} ({})", about.join(", "))
            }
        }
    };
    Some(format!(
        "waiting ({}s): {method}; {lock} held by {holder}",
        waited.as_secs()
    ))
}

/// `45s`, `2m10s`, `1h05m`.
fn span(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m{:02}s", s / 60, s % 60),
        _ => format!("{}h{:02}m", s / 3600, s % 3600 / 60),
    }
}

struct Progress(mpsc::Sender<()>);
impl Progress {
    fn start(ctx: Ctx, method: String, session: Option<String>) -> Option<Self> {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("rpc-progress".into())
            .spawn(move || {
                let start = Instant::now();
                let interval = Duration::from_secs(15);
                let mut next = start + interval;
                loop {
                    if rx.recv_timeout(next.saturating_duration_since(Instant::now()))
                        != Err(mpsc::RecvTimeoutError::Timeout)
                    {
                        break;
                    }
                    let state = ctx
                        .call_within(
                            "daemon_status",
                            json!({}),
                            Kind::Read,
                            Duration::from_secs(2),
                        )
                        .ok();
                    if rx.try_recv() != Err(mpsc::TryRecvError::Empty) {
                        break;
                    }
                    eprintln!(
                        "{}",
                        line(&method, session.as_deref(), start.elapsed(), state.as_ref())
                    );
                    next += interval;
                }
            })
            .ok()?;
        Some(Self(tx))
    }
}
impl Drop for Progress {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

fn line(method: &str, session: Option<&str>, elapsed: Duration, state: Option<&Value>) -> String {
    let mut text = format!(
        "still waiting ({}s): {method}{}",
        elapsed.as_secs(),
        session.map(|s| format!(" {s}")).unwrap_or_default()
    );
    let Some(state) = state else {
        return format!("{text}; daemon status unresponsive");
    };
    if let Some(queue) = state["queues"]
        .as_array()
        .and_then(|q| q.iter().find(|q| q["session"].as_str() == session))
        && let Some(holder) = queue["holder"].as_str()
    {
        if holder == method {
            text.push_str(&format!("; session held by {holder}"));
        } else {
            text.push_str(&format!(" queued behind {holder}"));
        }
    }
    for hold in state["holds"].as_array().into_iter().flatten() {
        if let (Some(repo), Some(holder)) = (hold["repo"].as_str(), hold["method"].as_str()) {
            text.push_str(&format!("; {repo} held by {holder}"));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_accepts_old_and_new_daemon_shapes() {
        let old = json!({"holds":[{"repo":"r2", "method":"publish"}]});
        assert_eq!(
            line("sync", Some("s-ab12"), Duration::from_secs(45), Some(&old)),
            "still waiting (45s): sync s-ab12; r2 held by publish"
        );
        let new = json!({"queues":[{"session":"s-ab12", "holder":"old_tree"}], "holds":[]});
        assert!(
            line("sync", Some("s-ab12"), Duration::from_secs(45), Some(&new))
                .contains("queued behind old_tree")
        );
    }

    /// Virtual time: `sleep` and long attempts move it, nothing blocks.
    #[derive(Default)]
    struct Fake {
        now: std::cell::Cell<Duration>,
        slept: std::cell::RefCell<Vec<Duration>>,
    }
    impl Fake {
        fn advance(&self, d: Duration) {
            self.now.set(self.now.get() + d);
        }
    }
    impl Clock for Fake {
        fn elapsed(&self) -> Duration {
            self.now.get()
        }
        fn sleep(&self, d: Duration) {
            self.slept.borrow_mut().push(d);
            self.advance(d);
        }
    }

    fn lock_wait(certified: bool) -> CallError {
        let mut d = json!({
            "reason": "lock_wait", "lock": {"repo": "tool"}, "holder": "sync",
            "holder_session": "s-uijvlo3l", "holder_origin": "request",
            "holder_since": "2026-11-25T10:00:00Z", "held_ms": 130_000,
        });
        if certified {
            d["retry_safe"] = json!(true);
        }
        CallError::Failed(
            StrataError::new(Code::Busy, "waited 30000 ms before anything was written")
                .with_detail(d),
        )
    }

    fn retry_loop<T>(
        method: &str,
        wait_startup: bool,
        limit: Option<Duration>,
        deadline: Option<&Deadline>,
        clock: &dyn Clock,
        note: &mut dyn FnMut(String),
        attempt: impl FnMut() -> std::result::Result<T, CallError>,
    ) -> std::result::Result<T, CallError> {
        RetryBudget::with_limit(limit).run_with_clock(
            method,
            wait_startup,
            deadline,
            clock,
            note,
            attempt,
        )
    }

    #[test]
    fn u3_exact_boundary_retry_uses_the_virtual_clock() {
        for reason in ["starting", "store_unavailable", "upgrading"] {
            let clock = Fake::default();
            let result = retry_loop(
                "du",
                true,
                Some(Duration::from_millis(120)),
                None,
                &clock,
                &mut |_| {},
                || {
                    Err::<Value, _>(CallError::Failed(
                        StrataError::new(Code::Busy, reason).with_detail(
                            json!({"reason":reason, "retry_safe":reason == "upgrading"}),
                        ),
                    ))
                },
            );
            assert_eq!(failed(result).detail.unwrap()["retries"], 1);
            assert_eq!(*clock.slept.borrow(), [Duration::from_millis(120)]);
        }
    }

    #[test]
    fn u3_probe_and_parameter_variants_share_backoff_and_retry_count() {
        let clock = Fake::default();
        let mut budget = RetryBudget::with_limit(Some(Duration::from_secs(3)));
        let busy = || CallError::Failed(StrataError::upgrading("test", "park", 2000));
        for method in ["daemon_status", "sync"] {
            let mut attempts = [Err(busy()), Ok(())].into_iter();
            budget
                .run_with_clock(method, false, None, &clock, &mut |_| {}, || {
                    attempts.next().unwrap()
                })
                .unwrap();
        }
        let legacy = budget.run_with_clock("sync", false, None, &clock, &mut |_| {}, || {
            Err::<Value, _>(busy())
        });
        assert_eq!(failed(legacy).detail.unwrap()["retries"], 2);
        assert_eq!(*clock.slept.borrow(), [1, 2].map(Duration::from_secs));
    }

    fn run(
        budget: Option<Duration>,
        clock: &Fake,
        replies: Vec<std::result::Result<Value, CallError>>,
    ) -> (std::result::Result<Value, CallError>, usize, Vec<String>) {
        let mut replies = replies.into_iter();
        let mut sent = 0;
        let mut lines = Vec::new();
        let out = retry_loop(
            "new",
            false,
            budget,
            None,
            clock,
            &mut |l| lines.push(l),
            || {
                sent += 1;
                replies.next().expect("no more replies than attempts")
            },
        );
        (out, sent, lines)
    }

    fn failed(r: std::result::Result<Value, CallError>) -> StrataError {
        match r {
            Err(CallError::Failed(e)) => e,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// new-lock §8 `cli_retries_only_pre_write_lock_wait`: only `retry_safe`
    /// refusals are sent again; a bare `lock_wait` (a legacy daemon's, or
    /// one after a write), a fence refusal and `Unknown` come back at once.
    #[test]
    fn cli_retries_only_pre_write_lock_wait() {
        let clock = Fake::default();
        let (out, sent, _) = run(
            Some(Duration::from_secs(600)),
            &clock,
            vec![
                Err(lock_wait(true)),
                Err(lock_wait(true)),
                Ok(json!({"session": {}})),
            ],
        );
        assert!(out.is_ok());
        assert_eq!(sent, 3);
        assert_eq!(
            *clock.slept.borrow(),
            [Duration::from_secs(1), Duration::from_secs(2)]
        );

        let fenced =
            StrataError::new(Code::Busy, "fence").with_detail(json!({"reason": "pins_tightened"}));
        let unknown =
            StrataError::new(Code::Unknown, "lost").with_detail(json!({"outcome": "UNCERTAIN"}));
        for refusal in [
            lock_wait(false),
            CallError::Failed(fenced),
            CallError::Failed(unknown),
        ] {
            let clock = Fake::default();
            let (out, sent, _) = run(None, &clock, vec![Err(refusal)]);
            let e = failed(out);
            assert_eq!(sent, 1, "{e:?} is not replayed");
            assert!(clock.slept.borrow().is_empty());
            assert!(e.detail.unwrap().get("retries").is_none(), "returned as is");
        }
    }

    /// new-lock §8 `budget_checked_between_attempts_only`: an attempt is
    /// never cut short; the budget only decides whether another is sent.
    #[test]
    fn budget_checked_between_attempts_only() {
        // One attempt runs 20 s past a 5 s budget: no retry, and the last
        // refusal carries the retry count, the wait and the fix.
        let clock = Fake::default();
        let mut attempts = vec![Err(lock_wait(true))].into_iter();
        let mut lines = Vec::new();
        let out = retry_loop(
            "add",
            false,
            Some(Duration::from_secs(5)),
            None,
            &clock,
            &mut |l| lines.push(l),
            || {
                clock.advance(Duration::from_secs(20));
                attempts.next().unwrap()
            },
        );
        let e = failed(out);
        let d = e.detail.unwrap();
        assert_eq!(d["retries"], 0);
        assert_eq!(d["waited_ms"], 20_000);
        assert_eq!(d["fix"], RETRY_FIX);
        assert_eq!(d["retry_safe"], true, "the refusal itself is unchanged");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].ends_with("; giving up"), "{lines:?}");

        // Quick refusals: 1 s, 2 s, then the 2 s left of the budget; the
        // attempt at the deadline is the last.
        let clock = Fake::default();
        let (out, sent, lines) = run(
            Some(Duration::from_secs(5)),
            &clock,
            (0..4).map(|_| Err(lock_wait(true))).collect(),
        );
        let e = failed(out);
        assert_eq!(sent, 4);
        assert_eq!(*clock.slept.borrow(), [1, 2, 2].map(Duration::from_secs));
        assert_eq!(e.detail.unwrap()["retries"], 3);
        assert_eq!(
            lines.iter().filter(|l| l.ends_with("; retrying")).count(),
            3
        );
    }

    /// new-lock §8 `timeout_zero_unlimited`: no budget; the backoff stops
    /// growing at 10 s.
    #[test]
    fn timeout_zero_unlimited() {
        let args = WaitArgs {
            timeout: Some(0.0),
            ..WaitArgs::default()
        };
        assert_eq!(budget(&args, RETRY_DEFAULT_SECS).unwrap(), None);
        let none = WaitArgs::default();
        if std::env::var_os("GIT_STRATA_TIMEOUT").is_none() {
            assert_eq!(
                budget(&none, RETRY_DEFAULT_SECS).unwrap(),
                Some(Duration::from_secs(600))
            );
        }
        let clock = Fake::default();
        let mut replies: Vec<_> = (0..30).map(|_| Err(lock_wait(true))).collect();
        replies.push(Ok(json!({})));
        let (out, sent, _) = run(None, &clock, replies);
        assert!(out.is_ok());
        assert_eq!(sent, 31);
        let slept = clock.slept.borrow();
        assert_eq!(slept[..5], [1, 2, 4, 8, 10].map(Duration::from_secs));
        assert!(slept.iter().all(|d| *d <= RETRY_MAX));
    }

    #[test]
    fn u3_upgrade_backoff_is_capped_and_the_budget_only_bounds_retries() {
        let another_certified_reason = StrataError::new(Code::Busy, "refused")
            .with_detail(json!({"reason":"future_pre_write_refusal","retry_safe":true}));
        assert!(should_retry("new", &another_certified_reason, false));
        assert!(should_retry("add", &another_certified_reason, false));
        assert!(!should_retry("publish", &another_certified_reason, false));
        let clock = Fake::default();
        let mut calls = 0;
        let result = retry_loop("publish", false, None, None, &clock, &mut |_| {}, || {
            calls += 1;
            if calls == 7 {
                return Ok(());
            }
            Err(CallError::Failed(StrataError::upgrading(
                "h1", "park", 2000,
            )))
        });
        result.unwrap();
        assert_eq!(
            *clock.slept.borrow(),
            [1, 2, 4, 8, 10, 10].map(Duration::from_secs)
        );

        let clock = Fake::default();
        let result = retry_loop(
            "publish",
            false,
            Some(Duration::from_secs(2)),
            None,
            &clock,
            &mut |_| {},
            || {
                clock.advance(Duration::from_secs(30));
                Err::<(), _>(CallError::Failed(StrataError::upgrading(
                    "h1", "park", 2000,
                )))
            },
        );
        let error = result.unwrap_err().into_error();
        assert_eq!(error.detail.unwrap()["retries"], 0);
        assert!(clock.slept.borrow().is_empty());
    }

    /// new-lock §8 `progress_lines`: the holder comes from the refusal; a
    /// lock between holders reads `(handing over)`, a legacy detail shows
    /// the method alone, a session queue names the session.
    #[test]
    fn progress_lines() {
        let CallError::Failed(e) = lock_wait(true) else {
            unreachable!()
        };
        assert_eq!(
            holder_line("new", Duration::from_secs(45), &e).unwrap(),
            "waiting (45s): new; repo tool held by sync (session s-uijvlo3l, request, 2m10s)"
        );
        let between = StrataError::new(Code::Busy, "w").with_detail(json!({
            "reason": "lock_wait", "lock": {"repo": "tool"}, "holder": null, "holder_session": null,
        }));
        assert_eq!(
            holder_line("new", Duration::from_secs(3), &between).unwrap(),
            "waiting (3s): new; repo tool held by (handing over)"
        );
        let legacy = StrataError::new(Code::Busy, "w").with_detail(json!({
            "reason": "lock_wait", "lock": {"session": "s-ab12cd34"}, "holder": "checkpoint", "holder_session": null,
        }));
        assert_eq!(
            holder_line("add", Duration::from_secs(31), &legacy).unwrap(),
            "waiting (31s): add; session s-ab12cd34 held by checkpoint"
        );
        let fenced =
            StrataError::new(Code::Busy, "f").with_detail(json!({"reason": "pins_tightened"}));
        assert_eq!(holder_line("new", Duration::ZERO, &fenced), None);
        assert_eq!(span(Duration::from_secs(59)), "59s");
        assert_eq!(span(Duration::from_secs(3_900)), "1h05m");

        // Not retried, but still named.
        let clock = Fake::default();
        let (_, _, lines) = run(
            Some(Duration::from_secs(600)),
            &clock,
            vec![Err(lock_wait(false))],
        );
        assert_eq!(
            lines,
            [
                "waiting (0s): new; repo tool held by sync (session s-uijvlo3l, request, 2m10s); not retried"
            ]
        );
    }
}
