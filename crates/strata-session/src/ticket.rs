//! Ticket labels (§5.1 step 1): a free-form label, never an identity.

use serde_json::json;
use strata_types::{Code, StrataError};

use crate::Result;

/// `^[sS]-[A-Za-z2-7]{8}$`: what a session id looks like, case-insensitively.
pub(crate) fn looks_like_session_id(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && (b[0] == b's' || b[0] == b'S')
        && b[1] == b'-'
        && b[2..].iter().all(|c| c.is_ascii_alphabetic() || (b'2'..=b'7').contains(c))
}

/// `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`, and not of the session-id form.
pub(crate) fn validate(ticket: &str) -> Result<()> {
    let b = ticket.as_bytes();
    let ok = !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_alphanumeric()
        && b[1..].iter().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'));
    if !ok {
        return Err(StrataError::new(
            Code::InvalidInput,
            "a ticket is 1-64 characters of [A-Za-z0-9._-] starting with a letter or digit",
        )
        .with_detail(json!({ "field": "ticket", "value": ticket })));
    }
    if looks_like_session_id(ticket) {
        return Err(StrataError::new(
            Code::ReservedName,
            format!("ticket {ticket:?} has the form of a session id"),
        )
        .with_detail(json!({ "field": "ticket", "value": ticket })));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms() {
        for ok in ["9246", "gwd-9246", "a.b_c-d", "s-k3q7xm2", "s-k3q7xm2a1", "s_k3q7xm2a"] {
            assert!(validate(ok).is_ok(), "{ok}");
        }
        let long = "a".repeat(65);
        for bad in ["../foreign", "", long.as_str(), "a b", "a:b", ".a", "-a"] {
            assert_eq!(validate(bad).unwrap_err().code, Code::InvalidInput, "{bad}");
        }
        assert!(validate(&"a".repeat(64)).is_ok());
        for r in ["s-k3q7xm2a", "S-K3Q7XM2A", "s-abcdefgh"] {
            assert_eq!(validate(r).unwrap_err().code, Code::ReservedName, "{r}");
        }
        // 0, 1, 8, 9 are outside the base32 alphabet: not an id form.
        assert!(validate("s-k3q7xm29").is_ok());
    }

}
