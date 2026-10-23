//! The in-process registry of minted tokens (confirm.md §4.1, §5.5, §5.8).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::json;
use strata_types::{Action, ConfirmToken, StrataError};

use crate::confirmer::not_confirmed;
use crate::{RECORD_RETENTION, TOKEN_TTL};

pub(crate) struct Record {
    action: Action,
    issued: Instant,
    issued_mono_ns: u64,
    spent: bool,
}

#[derive(Default)]
pub(crate) struct Registry {
    map: Mutex<HashMap<u128, Record>>,
}

fn sweep(map: &mut HashMap<u128, Record>, now_mono: u64) {
    let keep = RECORD_RETENTION.as_nanos() as u64;
    map.retain(|_, r| now_mono.saturating_sub(r.issued_mono_ns) < keep);
}

/// 16 bytes from `arc4random_buf` (cannot fail on macOS).
fn random_u128() -> u128 {
    let mut buf = [0u8; 16];
    // SAFETY: `buf` is a valid writable buffer of the given length.
    unsafe { libc::arc4random_buf(buf.as_mut_ptr().cast(), buf.len()) };
    u128::from_le_bytes(buf)
}

impl Registry {
    /// §5.2 step 9: register a freshly authenticated action and return its token.
    pub(crate) fn mint(
        &self,
        action: Action,
        issued: Instant,
        issued_mono_ns: u64,
    ) -> Result<ConfirmToken, StrataError> {
        let Ok(mut map) = self.map.lock() else {
            return Err(internal(&action));
        };
        sweep(&mut map, issued_mono_ns);
        let nonce = loop {
            let n = random_u128();
            if !map.contains_key(&n) {
                break n;
            }
        };
        map.insert(
            nonce,
            Record {
                action: action.clone(),
                issued,
                issued_mono_ns,
                spent: false,
            },
        );
        Ok(ConfirmToken {
            action,
            issued,
            nonce,
        })
    }

    /// §5.5: redeem a token. Any lookup that finds the nonce spends it.
    pub(crate) fn redeem(
        &self,
        token: &ConfirmToken,
        expected: &Action,
        now: Instant,
        now_mono: u64,
    ) -> Result<(), StrataError> {
        let Ok(mut map) = self.map.lock() else {
            return Err(not_confirmed(
                "internal",
                "the confirm registry is unusable; restart the daemon",
                json!({}),
            ));
        };
        sweep(&mut map, now_mono);
        let Some(rec) = map.get_mut(&token.nonce) else {
            return Err(not_confirmed(
                "unknown",
                "this confirmation was not issued by this daemon, or is long gone",
                json!({}),
            ));
        };
        if rec.spent {
            return Err(not_confirmed(
                "used",
                "this confirmation was already used",
                json!({}),
            ));
        }
        rec.spent = true;
        if token.action != rec.action || token.issued != rec.issued {
            return Err(not_confirmed(
                "tampered",
                "this confirmation was changed after it was issued",
                json!({}),
            ));
        }
        if rec.action != *expected {
            return Err(not_confirmed(
                "mismatch",
                "the confirmed action is not the one being performed",
                json!({ "expected": expected, "confirmed": rec.action }),
            ));
        }
        let age_i = now.saturating_duration_since(rec.issued);
        let age = if now_mono < rec.issued_mono_ns {
            // The clock went backwards; never expected. Treat as expired.
            None
        } else {
            Some(age_i.max(Duration::from_nanos(now_mono - rec.issued_mono_ns)))
        };
        match age {
            Some(a) if a < TOKEN_TTL => Ok(()),
            a => Err(not_confirmed(
                "expired",
                "this confirmation expired; confirm again",
                json!({ "age_ms": a.unwrap_or(age_i).as_millis() as u64 }),
            )),
        }
    }

    pub(crate) fn count(&self) -> usize {
        match self.map.lock() {
            Ok(m) => m.len(),
            Err(p) => p.into_inner().len(),
        }
    }
}

fn internal(action: &Action) -> StrataError {
    not_confirmed(
        "internal",
        "the confirm registry is unusable; restart the daemon",
        json!({ "action": action }),
    )
}
