//! Chosen publish text is immutable at insertion; generated trailers carry identity.
use crate::{Ctx, Result};
use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use strata_types::{CheckpointReason, Code, ReleaseGroup, RepoName, StrataError};

#[derive(Clone, Debug, Default)]
pub struct Messages {
    pub message: Option<String>,
    pub message_for: BTreeMap<RepoName, String>,
}
impl Messages {
    pub fn validate(&self, group: &ReleaseGroup) -> Result<()> {
        if let Some(m) = &self.message {
            validate(m)?;
        }
        for (repo, m) in &self.message_for {
            validate(m)?;
            if !group.batches.iter().any(|b| &b.repo == repo) {
                return Err(StrataError::new(
                    Code::InvalidInput,
                    format!("--message-for {repo} is not in this publish"),
                )
                .with_detail(json!({"reason":"message_repo", "repo":repo})));
            }
        }
        Ok(())
    }
}

pub fn validate(message: &str) -> Result<()> {
    let reserved = message.lines().enumerate().find(|(_, l)| {
        l.split_once(':').is_some_and(|(key, _)| {
            let key: String = key
                .chars()
                .filter(|c| !c.is_whitespace())
                .flat_map(char::to_lowercase)
                .collect();
            matches!(
                key.as_str(),
                "strata-publish" | "strata-sessions" | "sessions" | "paths"
            )
        })
    });
    if message.len() > 65536
        || message.lines().next().is_none_or(|l| l.trim().is_empty())
        || message
            .chars()
            .any(|c| c < '\u{20}' && c != '\n' && c != '\t')
        || reserved.is_some()
    {
        let mut detail = json!({"reason":"message"});
        if let Some((index, line)) = reserved {
            detail["line"] = json!(line);
            detail["line_number"] = json!(index + 1);
        }
        return Err(StrataError::new(Code::InvalidInput, "publish message needs a nonempty subject, at most 64 KiB, no control characters or reserved trailers")
            .with_detail(detail));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Flag,
    Checkpoints,
    Legacy,
}
#[derive(Clone, Debug)]
pub struct Chosen {
    pub text: Option<String>,
    pub source: Source,
}
impl Chosen {
    pub fn subject(&self, id: u64, repo: &RepoName) -> String {
        self.text
            .as_ref()
            .and_then(|m| m.lines().next())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("strata publish {id} ({repo})"))
    }
    pub fn render(&self, group: &ReleaseGroup, repo: &RepoName, paths: usize) -> String {
        let text = self
            .text
            .clone()
            .unwrap_or_else(|| self.subject(group.id, repo));
        let sessions: BTreeSet<_> = group
            .batches
            .iter()
            .filter(|b| &b.repo == repo)
            .map(|b| b.session.as_str())
            .collect();
        // A complete user trailer paragraph stays in the final block. A mixed
        // paragraph remains prose, separated from the generated block.
        let text = text.trim_end();
        let mut trailer_paragraph = true;
        let mut separated = false;
        for line in text.lines().rev() {
            if line.trim_matches([' ', '\t']).is_empty() {
                separated = true;
                break;
            }
            trailer_paragraph &= is_trailer(line);
        }
        let separator = if separated && trailer_paragraph {
            "\n"
        } else {
            "\n\n"
        };
        format!(
            "{text}{separator}sessions: {}\npaths: {paths}\nStrata-Publish: {} ({repo})\n",
            sessions.into_iter().collect::<Vec<_>>().join(", "),
            group.id
        )
    }
}
fn is_trailer(line: &str) -> bool {
    line.split_once(':').is_some_and(|(k, _)| {
        let k = k.trim_end_matches([' ', '\t']);
        !k.is_empty() && k.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
    })
}
fn short_subject(s: &str) -> String {
    if s.chars().count() <= 100 {
        return s.to_string();
    }
    let mut prefix: String = s.chars().take(99).collect();
    if let Some(i) = prefix.rfind(char::is_whitespace) {
        prefix.truncate(i);
    }
    format!("{}…", prefix.trim_end())
}

pub fn choose(ctx: &Ctx, group: &ReleaseGroup, messages: &Messages) -> Result<Chosen> {
    if let Some(m) = messages
        .message_for
        .get(ctx.repo)
        .or(messages.message.as_ref())
    {
        validate(m)?;
        return Ok(Chosen {
            text: Some(m.clone()),
            source: Source::Flag,
        });
    }
    // Startup may have closed a committed row Unknown without an outcome.
    // Use the same audit recovery as execution, without reconciling the row.
    let rows: Vec<_> = ctx
        .store
        .publishes()?
        .into_iter()
        .filter(|r| {
            r.phase != strata_store::PublishPhase::Running
                && r.group.batches.iter().any(|b| &b.repo == ctx.repo)
                && crate::push::row_commit(ctx, r).is_some()
        })
        .collect();
    let mut by_session = Vec::new();
    let mut seen_sessions = BTreeSet::new();
    for batch in group.batches.iter().filter(|b| &b.repo == ctx.repo) {
        if !seen_sessions.insert(&batch.session) {
            continue;
        }
        let through = group
            .batches
            .iter()
            .filter(|b| b.session == batch.session && b.repo == batch.repo)
            .map(|b| u64::from(b.checkpoint))
            .max()
            .unwrap_or(0);
        let after = rows
            .iter()
            .flat_map(|r| &r.group.batches)
            .filter(|b| b.session == batch.session && b.repo == batch.repo)
            .map(|b| u64::from(b.checkpoint))
            .max()
            .unwrap_or(0);
        let mut texts = Vec::new();
        let mut latest = None;
        for cp in ctx.store.checkpoints(&batch.session, Some(ctx.repo))? {
            if u64::from(cp.id) <= after || u64::from(cp.id) > through {
                continue;
            }
            if let CheckpointReason::Manual { message } = cp.reason {
                if validate(&message).is_err() {
                    continue;
                }
                let first = message.lines().next().unwrap_or("").trim().to_string();
                if !first.is_empty() {
                    latest = Some(first.clone());
                    if !texts.contains(&first) {
                        texts.push(first);
                    }
                }
            }
        }
        if !texts.is_empty() {
            by_session.push((batch.session.clone(), texts, latest.expect("nonempty")));
        }
    }
    if by_session.is_empty() {
        return Ok(Chosen {
            text: None,
            source: Source::Legacy,
        });
    }
    let mut latest = Vec::new();
    for (_, _, m) in &by_session {
        if !latest.contains(m) {
            latest.push(m.clone());
        }
    }
    let mut text = short_subject(&latest.join("; "));
    text.push_str("\n\n");
    // Reserve the exact generated identity overhead plus a bounded omission line.
    let blank = Chosen {
        text: Some(String::new()),
        source: Source::Checkpoints,
    }
    .render(group, ctx.repo, usize::MAX)
    .len();
    let cap = (60 * 1024usize)
        .checked_sub(blank + 100)
        .filter(|cap| *cap >= text.len())
        .ok_or_else(|| {
            StrataError::new(
                Code::InvalidInput,
                "publish identity trailers exceed the checkpoint-message budget",
            )
            .with_detail(json!({"reason":"message_too_large"}))
        })?;
    let mut omitted = 0;
    for (session, messages, _) in &by_session {
        for m in messages {
            let line = if by_session.len() > 1 {
                format!("- {session}: {m}\n")
            } else {
                format!("- {m}\n")
            };
            if omitted == 0 && text.len() + line.len() <= cap {
                text.push_str(&line);
            } else {
                omitted += 1;
            }
        }
    }
    if omitted > 0 {
        text.push_str(&format!("- … ({omitted} more messages omitted)\n"));
    }
    Ok(Chosen {
        text: Some(text.trim_end().into()),
        source: Source::Checkpoints,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validation_rejects_reserved_and_framing_controls() {
        for m in [
            "",
            " \nbody",
            "title\n\n StRaTa-Publish : 9",
            "title\n\n SeSsIoNs : x",
            "title\n PATHS : 1",
            "a\u{1e}b",
            "a\0b",
        ] {
            assert!(validate(m).is_err(), "{m:?}");
        }
        for m in [
            "a\n\nb",
            "a\n\n---\ntext",
            "a\n\nCo-authored-by: somebody",
            "a\tb",
        ] {
            validate(m).unwrap();
        }
    }
    #[test]
    fn reserved_message_error_identifies_the_offending_line() {
        let error = validate("subject\n\npaths: see README").unwrap_err();
        assert_eq!(error.code, Code::InvalidInput);
        assert_eq!(
            error.detail.unwrap(),
            json!({
                "reason":"message", "line":"paths: see README", "line_number":3,
            })
        );
    }
    #[test]
    fn trailer_merge_requires_a_complete_last_paragraph() {
        let group = ReleaseGroup {
            id: 1,
            batches: vec![],
        };
        let repo = RepoName::try_from("app".to_string()).unwrap();
        let render = |t: &str| {
            Chosen {
                text: Some(t.into()),
                source: Source::Flag,
            }
            .render(&group, &repo, 22)
        };
        assert!(render("a\n\nCo-authored-by: b").contains("Co-authored-by: b\nsessions:"));
        assert!(render("a\n \nCo-authored-by: b").contains("Co-authored-by: b\nsessions:"));
        assert!(render("a\n\nCo-authored-by: b\nprose").contains("prose\n\nsessions:"));
        assert!(render("a\n\n Co-authored-by : b").contains(" b\n\nsessions:"));
        assert!(render("a\n\nCo-authored-by\u{a0}: b").contains(" b\n\nsessions:"));
        assert!(render("a\n\u{a0}\nCo-authored-by: b").contains(" b\n\nsessions:"));
        assert!(render("a").contains("paths: 22\nStrata-Publish: 1 (app)"));
    }
    #[test]
    fn subject_cap_is_unicode_safe() {
        let s = short_subject(&"résumé words ".repeat(30));
        assert!(s.chars().count() <= 100 && s.ends_with('…'));
    }
}
