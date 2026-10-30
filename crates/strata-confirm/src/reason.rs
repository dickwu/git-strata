//! The text on the system prompt (confirm.md §5.6).

use std::fmt::Write as _;

use strata_types::Action;

/// The text the system puts into `"<process>" is trying to <reason>.`
///
/// Only identifiers from the `Action` go in, never free text an agent wrote. Printable ASCII
/// (0x20..=0x7E) is kept as is, `\` becomes `\\`, every other character becomes `\u{XXXX}`
/// (upper-case hex). Nothing is truncated. The result is never empty.
pub fn prompt_reason(action: &Action) -> String {
    // No wildcard arm on purpose: a new Action variant must get its own line on the prompt.
    match action {
        Action::PushDeploy { repo, branch, url } => {
            format!(
                "push {} to origin/{} at {}",
                escape(repo.as_str()),
                escape(branch),
                escape(&strata_types::display_url(url))
            )
        }
        Action::DepsApprove { proposal } => {
            format!(
                "approve dependency proposal {}",
                escape(&proposal.to_string())
            )
        }
        Action::RemoveDiscard { session, kept_trees } => {
            let mut s = format!(
                "discard the unpublished changes of session {} and remove it",
                escape(session.as_str())
            );
            for k in kept_trees {
                match k.count {
                    0 => {
                        let _ = write!(s, "; also discards the kept pre-move tree {}", escape(&k.path));
                    }
                    n => {
                        let files = if n == 1 { "file" } else { "files" };
                        let _ = write!(
                            s,
                            "; also discards {n} late-written {files} in the kept pre-move tree {}",
                            escape(&k.path)
                        );
                    }
                }
            }
            s
        }
        Action::ConfigRelax { repo } => {
            format!(
                "apply relaxed push settings for {}",
                escape(&repo.to_string())
            )
        }
    }
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ' '..='~' => out.push(c),
            _ => {
                let _ = write!(out, "\\u{{{:X}}}", c as u32);
            }
        }
    }
    out
}
