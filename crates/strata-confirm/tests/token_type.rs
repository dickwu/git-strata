//! confirm.md §7 `token_has_no_clone_copy_or_serde` (C1, C5).

use static_assertions::{assert_impl_all, assert_not_impl_any};
use strata_confirm::Confirmer;
use strata_types::ConfirmToken;

// A token can only be moved by value, once: no copies, no serialisation.
assert_not_impl_any!(ConfirmToken: Clone, Copy, serde::Serialize, serde::de::DeserializeOwned);

// The daemon shares one issuer across threads.
assert_impl_all!(Confirmer: Send, Sync);

// LAContext is not Send, so a Send + Sync Confirmer cannot keep one between prompts:
// every prompt makes a fresh context (C1).
assert_not_impl_any!(objc2_local_authentication::LAContext: Send, Sync);

#[test]
fn token_has_no_clone_copy_or_serde() {
    // The assertions above are checked at compile time; this test exists so the
    // name shows up in the test report.
    fn takes_by_value(t: ConfirmToken) -> u128 {
        t.nonce
    }
    let t = ConfirmToken {
        action: strata_types::Action::DepsApprove {
            proposal: strata_types::ProposalId::from(1i64),
        },
        issued: std::time::Instant::now(),
        nonce: 7,
    };
    assert_eq!(takes_by_value(t), 7);
}
