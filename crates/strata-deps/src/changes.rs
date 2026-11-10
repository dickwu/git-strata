//! Read-only package attribution (main-sync §7.2). No checkout, install, or network.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

use crate::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use strata_gitio::{Git, TreeItem};
use strata_types::{Code, CommitOid, RepoConfig, RepoPath, StrataError};

const BLOB_LIMIT: u64 = 16 * 1024 * 1024;
const RESPONSE_BUDGET: Duration = Duration::from_millis(200);
const WORKER_BUDGET: Duration = Duration::from_secs(5);
const PACKAGE_LIMIT: usize = 50;
const CACHE_LIMIT: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changes {
    pub summary: String,
    pub packages: Vec<PackageChange>,
    pub truncated: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageChange {
    pub name: String,
    pub from: Option<String>,
    pub to: Option<String>,
    pub kind: ChangeKind,
    pub direct: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Removed,
    Changed,
}

#[derive(Clone, Default)]
struct Package {
    versions: BTreeSet<String>,
    direct: bool,
}
type Packages = BTreeMap<String, Package>;
// File names identify parsers; companion manifests affect directness and fallback specs.
type CacheKey = Vec<(String, Option<String>, Option<String>)>;
static CACHE: OnceLock<Mutex<VecDeque<(CacheKey, Changes)>>> = OnceLock::new();
static RUNNING: AtomicBool = AtomicBool::new(false);

fn unavailable(reason: &str) -> StrataError {
    StrataError::new(Code::Busy, "package summary is not available yet")
        .with_detail(serde_json::json!({"reason": reason}))
}
fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| unavailable("changes_budget"))
}

/// Compare committed install inputs. One bounded worker prevents concurrent slow parsers
/// or storage from accumulating detached work. A timeout/occupied slot is best-effort
/// failure: the caller omits enrichment. The worker stops between reads/parsers, and each
/// Git read has the remaining worker deadline. The 200 ms response wait is separate
/// so slower work can finish within 5 s and populate the cache for the next poll.
pub fn changes(
    git: &Git,
    cfg: &RepoConfig,
    from_head: &CommitOid,
    to_head: &CommitOid,
) -> Result<Changes> {
    if cfg.install_inputs.is_empty() || from_head == to_head {
        return Ok(finish(Vec::new(), &[], false));
    }
    let (git, cfg, from, to) = (git.clone(), cfg.clone(), from_head.clone(), to_head.clone());
    within_budget(RESPONSE_BUDGET, move |deadline| {
        compare(&git, &cfg, &from, &to, deadline)
    })
}

fn within_budget(
    budget: Duration,
    work: impl FnOnce(Instant) -> Result<Changes> + Send + 'static,
) -> Result<Changes> {
    let response_deadline = Instant::now() + budget;
    let worker_deadline = Instant::now() + WORKER_BUDGET;
    if RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return Err(unavailable("changes_busy"));
    }
    struct Slot;
    impl Drop for Slot {
        fn drop(&mut self) {
            RUNNING.store(false, Ordering::Release);
        }
    }
    let slot = Slot;
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("deps-summary".into())
        .spawn(move || {
            let result = work(worker_deadline);
            drop(slot);
            let _ = tx.send(result);
        })
        .map_err(|_| unavailable("changes_worker"))?;
    rx.recv_timeout(response_deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| unavailable("changes_budget"))?
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}
fn companion(path: &str) -> Option<String> {
    let manifest = match basename(path) {
        "bun.lock" | "bun.lockb" | "package-lock.json" | "npm-shrinkwrap.json" => "package.json",
        "composer.lock" => "composer.json",
        "pubspec.lock" => "pubspec.yaml",
        "Cargo.lock" => "Cargo.toml",
        _ => return None,
    };
    Some(match path.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/{manifest}"),
        None => manifest.into(),
    })
}
fn oid(item: Option<&TreeItem>) -> Option<String> {
    item.map(|i| format!("{}:{}", i.mode, i.oid))
}
fn bytes(git: &Git, item: Option<&TreeItem>, deadline: Instant) -> Result<Option<Vec<u8>>> {
    let Some(item) = item else {
        return Ok(Some(Vec::new()));
    };
    if item.kind != "blob" || !matches!(item.mode, 0o100644 | 0o100755) || item.size > BLOB_LIMIT {
        return Ok(None);
    }
    let b = git.object_bytes_within(&item.oid, remaining(deadline)?)?;
    remaining(deadline)?;
    Ok((b.len() as u64 <= BLOB_LIMIT).then_some(b))
}
fn compare(
    git: &Git,
    cfg: &RepoConfig,
    from: &CommitOid,
    to: &CommitOid,
    deadline: Instant,
) -> Result<Changes> {
    let mut a = git.commit_items_within(from, &cfg.install_inputs, remaining(deadline)?)?;
    let mut b = git.commit_items_within(to, &cfg.install_inputs, remaining(deadline)?)?;
    let changed: BTreeSet<String> = a
        .keys()
        .chain(b.keys())
        .filter(|p| {
            (a.get(*p).is_some_and(|i| i.kind != "tree")
                || b.get(*p).is_some_and(|i| i.kind != "tree"))
                && oid(a.get(*p)) != oid(b.get(*p))
        })
        .cloned()
        .collect();
    let companions: BTreeSet<String> = changed.iter().filter_map(|p| companion(p)).collect();
    let paths: Vec<RepoPath> = companions
        .iter()
        .map(|p| RepoPath::try_from(p.clone()))
        .collect::<Result<_>>()?;
    if !paths.is_empty() {
        a.extend(git.commit_items_within(from, &paths, remaining(deadline)?)?);
        b.extend(git.commit_items_within(to, &paths, remaining(deadline)?)?);
    }
    let key: CacheKey = changed
        .iter()
        .chain(companions.iter())
        .map(|p| (p.clone(), oid(a.get(p)), oid(b.get(p))))
        .collect();
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some((_, result)) = cache
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .find(|(k, _)| *k == key)
    {
        return Ok(result.clone());
    }
    let mut rows = Vec::new();
    let mut fallbacks = Vec::new();
    let mut covered = BTreeSet::new();
    let mut truncated = false;
    // Locks before manifests: one package change, with the resolved version when available.
    let mut files: Vec<_> = changed.into_iter().collect();
    files.sort_by_key(|p| (companion(p).is_none(), p.clone()));
    for path in files {
        remaining(deadline)?;
        if covered.contains(&path) {
            continue;
        }
        let manifest = companion(&path);
        let old_manifest = manifest
            .as_ref()
            .map(|p| read_packages(git, p, a.get(p), deadline))
            .transpose()?
            .flatten();
        let new_manifest = manifest
            .as_ref()
            .map(|p| read_packages(git, p, b.get(p), deadline))
            .transpose()?
            .flatten();
        let old = read_packages(git, &path, a.get(&path), deadline)?;
        let new = read_packages(git, &path, b.get(&path), deadline)?;
        let parsed_lock = old.is_some() && new.is_some();
        let pair = if parsed_lock {
            old.zip(new)
        } else {
            old_manifest
                .as_ref()
                .zip(new_manifest.as_ref())
                .map(|(a, b)| (a.clone(), b.clone()))
        };
        if let Some((mut old, mut new)) = pair {
            for (packages, manifest) in [
                (&mut old, old_manifest.as_ref()),
                (&mut new, new_manifest.as_ref()),
            ] {
                if let Some(manifest) = manifest {
                    for (name, p) in packages.iter_mut() {
                        p.direct |= manifest.contains_key(name);
                    }
                }
            }
            let diff = diff_packages(&old, &new);
            // An unreadable lock with unchanged specs must still say that the file changed.
            if !parsed_lock && diff.is_empty() {
                fallbacks.push(format!("{path} changed (no parser)"));
            }
            if let Some(manifest) = manifest {
                covered.insert(manifest);
            }
            rows.extend(diff);
        } else {
            fallbacks.push(format!("{path} changed (no parser)"));
        }
        truncated |= [a.get(&path), b.get(&path)]
            .into_iter()
            .flatten()
            .any(|i| i.size > BLOB_LIMIT);
    }
    remaining(deadline)?;
    // Multiple lockfiles can describe the same change. Keep distinct versions, merge directness.
    rows.sort_by(|a, b| (&a.name, &a.from, &a.to).cmp(&(&b.name, &b.from, &b.to)));
    rows.dedup_by(|a, b| {
        if a.name == b.name && a.from == b.from && a.to == b.to {
            b.direct |= a.direct;
            true
        } else {
            false
        }
    });
    let result = finish(rows, &fallbacks, truncated);
    // Keep very large names/specs out of the cache as well as bounding entry count.
    let cache_bytes = key
        .iter()
        .map(|(p, a, b)| {
            p.len() + a.as_ref().map_or(0, String::len) + b.as_ref().map_or(0, String::len)
        })
        .sum::<usize>()
        + result
            .packages
            .iter()
            .map(|p| {
                p.name.len()
                    + p.from.as_ref().map_or(0, String::len)
                    + p.to.as_ref().map_or(0, String::len)
            })
            .sum::<usize>();
    if cache_bytes <= 256 * 1024 {
        let mut cache = cache.lock().unwrap_or_else(|p| p.into_inner());
        if cache.len() >= CACHE_LIMIT {
            cache.pop_front();
        }
        cache.push_back((key, result.clone()));
    }
    Ok(result)
}
fn read_packages(
    git: &Git,
    path: &str,
    item: Option<&TreeItem>,
    deadline: Instant,
) -> Result<Option<Packages>> {
    if item.is_none() {
        return Ok(Some(Packages::new()));
    }
    if !matches!(
        basename(path),
        "bun.lock"
            | "package-lock.json"
            | "npm-shrinkwrap.json"
            | "composer.lock"
            | "Cargo.lock"
            | "pubspec.lock"
            | "package.json"
            | "composer.json"
            | "Cargo.toml"
            | "pubspec.yaml"
    ) {
        return Ok(None);
    }
    let result = bytes(git, item, deadline)?.and_then(|bytes| parse(basename(path), &bytes));
    remaining(deadline)?;
    Ok(result)
}
fn add(out: &mut Packages, name: &str, version: &str, direct: bool) {
    let p = out.entry(name.to_owned()).or_default();
    p.versions.insert(version.to_owned());
    p.direct |= direct;
}
fn diff_packages(a: &Packages, b: &Packages) -> Vec<PackageChange> {
    a.keys()
        .chain(b.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|name| {
            let versions = |p: Option<&Package>| {
                p.map(|p| p.versions.iter().cloned().collect::<Vec<_>>().join(", "))
            };
            let from = versions(a.get(name));
            let to = versions(b.get(name));
            (from != to).then(|| PackageChange {
                name: name.clone(),
                kind: match (&from, &to) {
                    (None, _) => ChangeKind::Added,
                    (_, None) => ChangeKind::Removed,
                    _ => ChangeKind::Changed,
                },
                from,
                to,
                direct: a.get(name).is_some_and(|p| p.direct)
                    || b.get(name).is_some_and(|p| p.direct),
            })
        })
        .collect()
}

/// Specs can embed several URLs (Cargo tables and Pub mappings). Redact each
/// authority with the shared display policy, including URLs in names/fallbacks.
fn redact_urls(mut text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    while let Some(scheme) = text.find("://") {
        let start = scheme + 3;
        let end = text[start..]
            .find(|c: char| {
                c.is_whitespace() || matches!(c, '/' | '?' | '#' | '"' | '\\' | '<' | '>')
            })
            .map_or(text.len(), |i| start + i);
        // Advance only past the marker/userinfo so a later URL in a table is
        // still visited, even when the preceding URL has no path component.
        let next = text[start..end]
            .rfind('@')
            .map_or(start, |at| start + at + 1);
        out.push_str(&strata_types::display_url(&text[..next]));
        text = &text[next..];
    }
    out.push_str(text);
    out
}

fn finish(mut packages: Vec<PackageChange>, fallbacks: &[String], mut truncated: bool) -> Changes {
    // Sanitize before formatting, truncation, serialization, or cache insertion.
    for p in &mut packages {
        p.name = redact_urls(&p.name);
        p.from = p.from.as_deref().map(redact_urls);
        p.to = p.to.as_deref().map(redact_urls);
    }
    packages.sort_by(|a, b| {
        b.direct
            .cmp(&a.direct)
            .then(a.name.cmp(&b.name))
            .then(a.from.cmp(&b.from))
            .then(a.to.cmp(&b.to))
    });
    truncated |= packages.len() > PACKAGE_LIMIT;
    packages.truncate(PACKAGE_LIMIT);
    let mut phrases: Vec<String> = packages
        .iter()
        .map(|p| match p.kind {
            ChangeKind::Added => format!("+{} {}", p.name, p.to.as_deref().unwrap_or("")),
            ChangeKind::Removed => format!("−{} {}", p.name, p.from.as_deref().unwrap_or("")),
            ChangeKind::Changed => format!(
                "{} {}→{}",
                p.name,
                p.from.as_deref().unwrap_or(""),
                p.to.as_deref().unwrap_or("")
            ),
        })
        .collect();
    phrases.extend(fallbacks.iter().map(|s| redact_urls(s)));
    let summary = if phrases.is_empty() {
        "No package changes".into()
    } else {
        phrases
            .join(", ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let clipped = summary.chars().count() > 120;
    let summary = if clipped {
        summary
            .chars()
            .take(119)
            .chain(std::iter::once('…'))
            .collect()
    } else {
        summary
    };
    Changes {
        summary,
        packages,
        truncated: truncated || clipped,
    }
}

fn json_specs(v: &Value, sections: &[&str], out: &mut Packages) {
    for section in sections {
        if let Some(deps) = v.get(section).and_then(Value::as_object) {
            for (name, version) in deps {
                if let Some(version) = version.as_str() {
                    add(out, name, version, true);
                }
            }
        }
    }
}
const JS_DEPS: &[&str] = &[
    "dependencies",
    "devDependencies",
    "optionalDependencies",
    "peerDependencies",
];
fn parse(name: &str, bytes: &[u8]) -> Option<Packages> {
    let mut out = Packages::new();
    match name {
        "package.json" | "composer.json" => {
            let v: Value = serde_json::from_slice(bytes).ok()?;
            v.as_object()?;
            json_specs(
                &v,
                if name == "package.json" {
                    JS_DEPS
                } else {
                    &["require", "require-dev"]
                },
                &mut out,
            );
        }
        "package-lock.json" | "npm-shrinkwrap.json" => {
            let v: Value = serde_json::from_slice(bytes).ok()?;
            if !matches!(v["lockfileVersion"].as_u64(), Some(2 | 3)) {
                return None;
            }
            let packages = v["packages"].as_object()?;
            let mut direct = Packages::new();
            for (path, p) in packages {
                if !path.contains("node_modules/") {
                    json_specs(p, JS_DEPS, &mut direct);
                }
            }
            for (path, p) in packages {
                let Some((_, name)) = path.rsplit_once("node_modules/") else {
                    continue;
                };
                if let Some(version) = p["version"].as_str() {
                    add(&mut out, name, version, direct.contains_key(name));
                }
            }
        }
        "bun.lock" => {
            let v: Value = serde_json::from_slice(&jsonc(bytes)?).ok()?;
            let mut direct = Packages::new();
            if let Some(workspaces) = v["workspaces"].as_object() {
                for p in workspaces.values() {
                    json_specs(p, JS_DEPS, &mut direct);
                }
            }
            for p in v["packages"].as_object()?.values() {
                let spec = p.as_array()?.first()?.as_str()?;
                // Skip a leading scope marker; URL userinfo belongs to the version.
                let at = spec.char_indices().skip(1).find(|(_, c)| *c == '@')?.0;
                let (name, version) = (&spec[..at], &spec[at + 1..]);
                add(&mut out, name, version, direct.contains_key(name));
            }
        }
        "composer.lock" => {
            let v: Value = serde_json::from_slice(bytes).ok()?;
            v["packages"].as_array()?;
            for section in ["packages", "packages-dev"] {
                if let Some(packages) = v[section].as_array() {
                    for p in packages {
                        add(&mut out, p["name"].as_str()?, p["version"].as_str()?, false);
                    }
                }
            }
        }
        "Cargo.lock" => {
            let v: toml::Value = toml::from_str(std::str::from_utf8(bytes).ok()?).ok()?;
            for p in v.get("package")?.as_array()? {
                add(
                    &mut out,
                    p.get("name")?.as_str()?,
                    p.get("version")?.as_str()?,
                    false,
                );
            }
        }
        "Cargo.toml" => {
            let v: toml::Value = toml::from_str(std::str::from_utf8(bytes).ok()?).ok()?;
            cargo_specs(&v, &mut out);
            if let Some(targets) = v.get("target").and_then(toml::Value::as_table) {
                for t in targets.values() {
                    cargo_specs(t, &mut out);
                }
            }
            if let Some(workspace) = v.get("workspace") {
                cargo_specs(workspace, &mut out);
            }
        }
        "pubspec.lock" | "pubspec.yaml" => {
            return pubspec(std::str::from_utf8(bytes).ok()?, name == "pubspec.lock");
        }
        _ => return None,
    }
    Some(out)
}
fn cargo_specs(v: &toml::Value, out: &mut Packages) {
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(deps) = v.get(section).and_then(toml::Value::as_table) {
            for (name, spec) in deps {
                let actual = spec
                    .get("package")
                    .and_then(toml::Value::as_str)
                    .unwrap_or(name);
                // Table specs retain git/path/features/workspace changes in manifest fallback.
                let version = spec.as_str().map(str::to_owned).unwrap_or_else(|| {
                    spec.as_table()
                        .filter(|t| t.len() == 1)
                        .and_then(|t| t.get("version"))
                        .and_then(toml::Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| spec.to_string())
                });
                add(out, actual, &version, true);
            }
        }
    }
}

/// JSONC comments and trailing commas, preserving string contents including URLs/escapes.
fn jsonc(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = bytes.to_vec();
    let (mut i, mut string) = (0, false);
    while i < out.len() {
        if out[i] == b'"' {
            string = !string;
            i += 1;
        } else if string && out[i] == b'\\' {
            i += 2;
        } else if !string && out.get(i..i + 2) == Some(b"//") {
            while i < out.len() && out[i] != b'\n' {
                out[i] = b' ';
                i += 1;
            }
        } else if !string && out.get(i..i + 2) == Some(b"/*") {
            out[i..i + 2].fill(b' ');
            i += 2;
            while i + 1 < out.len() && &out[i..i + 2] != b"*/" {
                out[i] = b' ';
                i += 1;
            }
            if i + 1 == out.len() {
                return None;
            }
            out.get_mut(i..i + 2)?.fill(b' ');
            i += 2;
        } else {
            i += 1;
        }
    }
    if string {
        return None;
    }
    let (mut i, mut string) = (0, false);
    while i < out.len() {
        match out[i] {
            b'"' => string = !string,
            b'\\' if string => {
                i += 2;
                continue;
            }
            b',' if !string
                && out[i + 1..]
                    .iter()
                    .find(|b| !b.is_ascii_whitespace())
                    .is_some_and(|b| matches!(b, b'}' | b']')) =>
            {
                out[i] = b' ';
            }
            _ => {}
        }
        i += 1;
    }
    Some(out)
}
// Pub's generated YAML is a block mapping; quoted scalars and inline comments are
// handled without interpreting aliases or executing any YAML tags.
fn yaml_line(line: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;
    let mut previous_space = true;
    for (i, c) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' && quote == Some('"') {
            escaped = true;
            continue;
        }
        if Some(c) == quote {
            quote = None;
        } else if quote.is_none() && matches!(c, '\'' | '"') {
            quote = Some(c);
        } else if c == '#' && quote.is_none() && previous_space {
            return &line[..i];
        }
        previous_space = c.is_whitespace();
    }
    line
}
fn yaml_scalar(value: &str) -> Option<String> {
    let value = value.trim();
    if value.starts_with('"') {
        return serde_json::from_str(value).ok();
    }
    if let Some(value) = value.strip_prefix('\'') {
        return value.strip_suffix('\'').map(|v| v.replace("''", "'"));
    }
    if value.starts_with(['*', '&', '!']) {
        return None;
    }
    Some(value.to_owned())
}
/// Pub emits a fixed block mapping. Reject unsupported lock YAML (aliases, flow mappings)
/// instead of guessing; unknown formats retain the file-level fallback.
fn pubspec(text: &str, lock: bool) -> Option<Packages> {
    let mut out = Packages::new();
    let (mut section, mut name) = (String::new(), String::new());
    let mut fields = Vec::new();
    let mut direct = false;
    let mut seen = false;
    let flush = |out: &mut Packages, name: &str, fields: &mut Vec<String>, direct: bool| {
        if !name.is_empty() && !fields.is_empty() {
            add(out, name, &fields.join(" "), direct);
        }
        fields.clear();
    };
    for raw in text.lines() {
        let line = yaml_line(raw).trim_end();
        if line.trim().is_empty() || matches!(line.trim(), "---" | "...") {
            continue;
        }
        if line.contains('\t') {
            return None;
        }
        let indent = line.len() - line.trim_start().len();
        if indent > 0
            && !matches!(
                section.as_str(),
                "packages" | "dependencies" | "dev_dependencies" | "dependency_overrides"
            )
        {
            continue;
        }
        let (key, value) = line.trim().split_once(':')?;
        if indent == 0 {
            flush(&mut out, &name, &mut fields, direct);
            name.clear();
            section = key.to_owned();
            if !lock {
                seen = true;
            }
            if (lock && key == "packages")
                || (!lock
                    && matches!(
                        key,
                        "dependencies" | "dev_dependencies" | "dependency_overrides"
                    ))
            {
                seen = true;
                if !value.trim().is_empty() && value.trim() != "{}" {
                    return None;
                }
            }
        } else if (lock && section == "packages")
            || (!lock
                && matches!(
                    section.as_str(),
                    "dependencies" | "dev_dependencies" | "dependency_overrides"
                ))
        {
            if indent == 2 {
                flush(&mut out, &name, &mut fields, direct);
                name = yaml_scalar(key)?;
                direct = !lock;
                if !value.trim().is_empty() {
                    if lock || value.trim().starts_with(['*', '&']) {
                        return None;
                    }
                    fields.push(yaml_scalar(value)?);
                }
            } else if indent == 4 && lock {
                match key {
                    "dependency" => direct = yaml_scalar(value)?.starts_with("direct "),
                    "version" => fields.push(yaml_scalar(value)?),
                    _ => {}
                }
            } else if !lock {
                fields.push(format!("{key}:{}", yaml_scalar(value)?));
            }
        }
    }
    flush(&mut out, &name, &mut fields, direct);
    seen.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bun_package_names_and_url_versions_are_safe() {
        for (spec, name, version) in [
            ("plain@1.2.3", "plain", "1.2.3"),
            ("@scope/pkg@1.2.3", "@scope/pkg", "1.2.3"),
            (
                "plain@git+ssh://git@host/repo#ref",
                "plain",
                "git+ssh://***@host/repo#ref",
            ),
            (
                "@scope/pkg@https://user:token@host/pkg.tgz",
                "@scope/pkg",
                "https://***@host/pkg.tgz",
            ),
        ] {
            let input =
                serde_json::to_vec(&serde_json::json!({"packages":{"pkg":[spec]}})).unwrap();
            let parsed = parse("bun.lock", &input).unwrap();
            let changes = finish(diff_packages(&Packages::new(), &parsed), &[], false);
            assert_eq!(changes.packages[0].name, name);
            assert_eq!(changes.packages[0].to.as_deref(), Some(version));
            let wire = serde_json::to_string(&changes).unwrap();
            assert!(!wire.contains("user:token"));
            assert!(!wire.contains("git@host"));
        }
    }

    #[test]
    fn every_parser_redacts_url_credentials_in_summary_fields() {
        let url = "https://user:secret@host/pkg";
        let json = |v: Value| serde_json::to_vec(&v).unwrap();
        let cases = [
            ("package.json", json(serde_json::json!({"dependencies":{"pkg":url}}))),
            ("composer.json", json(serde_json::json!({"require":{"pkg":url}}))),
            ("package-lock.json", json(serde_json::json!({"lockfileVersion":3,"packages":{"node_modules/pkg":{"version":url,"resolved":url}}}))),
            ("npm-shrinkwrap.json", json(serde_json::json!({"lockfileVersion":2,"packages":{"node_modules/pkg":{"version":url,"resolved":url}}}))),
            ("bun.lock", json(serde_json::json!({"packages":{"pkg":[format!("pkg@{url}")]}}))),
            ("composer.lock", json(serde_json::json!({"packages":[{"name":"pkg","version":url}]}))),
            ("Cargo.lock", format!("[[package]]\nname = 'pkg'\nversion = '{url}'\nsource = 'git+{url}'\n").into_bytes()),
            ("Cargo.toml", format!("[dependencies]\npkg = {{ git = '{url}', registry = 'https://other:private@mirror/repo' }}\n").into_bytes()),
            ("pubspec.lock", format!("packages:\n  pkg:\n    version: '{url}'\n").into_bytes()),
            ("pubspec.yaml", format!("dependencies:\n  pkg:\n    git:\n      url: '{url}'\n    hosted: 'https://other:private@mirror/repo'\n").into_bytes()),
        ];
        for (file, input) in cases {
            let packages = parse(file, &input).unwrap();
            for rows in [
                diff_packages(&Packages::new(), &packages),
                diff_packages(&packages, &Packages::new()),
            ] {
                let changes = finish(rows, &[], false);
                let wire = serde_json::to_string(&changes).unwrap();
                assert!(!changes.packages.is_empty(), "{file}");
                assert!(!wire.contains("user:secret"), "{file}: {wire}");
                assert!(!wire.contains("other:private"), "{file}: {wire}");
                assert!(wire.contains("https://***@host/pkg"), "{file}: {wire}");
            }
        }
    }

    #[test]
    fn summary_redacts_names_and_fallbacks_before_clipping() {
        let changes = finish(
            vec![PackageChange {
                name: "https://user:secret@host/pkg".into(),
                from: Some("https://old:password@host/pkg".into()),
                to: Some("https://new:token@host/pkg".into()),
                kind: ChangeKind::Changed,
                direct: true,
            }],
            &["https://fallback:password@host/file changed (no parser)".into()],
            false,
        );
        let wire = serde_json::to_string(&changes).unwrap();
        for secret in [
            "user:secret",
            "old:password",
            "new:token",
            "fallback:password",
        ] {
            assert!(!wire.contains(secret), "{wire}");
        }
    }

    #[test]
    fn url_redaction_preserves_safe_specs_and_handles_embedded_authorities() {
        for (text, expected) in [
            ("@scope/pkg@^1.2", "@scope/pkg@^1.2"),
            ("https://host/pkg@ref", "https://host/pkg@ref"),
            ("https://user:p'ass@host", "https://***@host"),
            ("git+ssh://git@host/pkg", "git+ssh://***@host/pkg"),
            (
                "https://u%40ser:pass%3Aword@[::1]:8080/pkg",
                "https://***@[::1]:8080/pkg",
            ),
            (
                "git = 'https://host', registry = 'https://user:pass@mirror'",
                "git = 'https://host', registry = 'https://***@mirror'",
            ),
        ] {
            assert_eq!(redact_urls(text), expected);
        }
    }

    #[test]
    fn summary_caps_and_direct_first() {
        let rows = (0..70)
            .map(|i| PackageChange {
                name: format!("package-{i:02}"),
                from: None,
                to: Some("1.0".into()),
                kind: ChangeKind::Added,
                direct: i == 69,
            })
            .collect();
        let r = finish(rows, &[], false);
        assert_eq!(r.packages.len(), 50);
        assert_eq!(r.packages[0].name, "package-69");
        assert!(r.truncated);
        assert_eq!(r.summary.chars().count(), 120);
    }
    #[test]
    fn jsonc_keeps_comment_markers_inside_strings() {
        let v: Value = serde_json::from_slice(
            &jsonc(br#"{/*comment*/"url":"https://example/\"//", "a":[1,],}//tail"#).unwrap(),
        )
        .unwrap();
        assert_eq!(v["url"], "https://example/\"//");
        assert_eq!(v["a"], serde_json::json!([1]));
        assert!(jsonc(b"{/* unterminated").is_none());
    }
    #[test]
    fn parser_deadline_returns_without_waiting_and_bounds_background_work() {
        let (release, wait) = mpsc::channel();
        let (ended, done) = mpsc::channel();
        let start = Instant::now();
        let error = within_budget(Duration::from_millis(10), move |_| {
            wait.recv().unwrap();
            let _ = ended.send(());
            Ok(finish(Vec::new(), &[], false))
        })
        .unwrap_err();
        assert_eq!(error.detail.unwrap()["reason"], "changes_budget");
        assert!(start.elapsed() < Duration::from_millis(200));
        assert_eq!(
            within_budget(RESPONSE_BUDGET, |_| panic!("must not spawn another parser"))
                .unwrap_err()
                .detail
                .unwrap()["reason"],
            "changes_busy"
        );
        release.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(1)).unwrap();
    }
    #[test]
    fn unsupported_or_malformed_inputs_do_not_invent_packages() {
        for (name, bytes) in [
            ("bun.lockb", b"binary".as_slice()),
            ("pnpm-lock.yaml", b"resolution: https://user:token@host/pkg"),
            ("yarn.lock", b"resolved https://user:token@host/pkg"),
            (
                "package-lock.json",
                br#"{"lockfileVersion":1,"dependencies":{}}"#,
            ),
            ("Cargo.lock", b"not toml"),
            ("pubspec.lock", b"packages: *alias"),
        ] {
            assert!(parse(name, bytes).is_none(), "{name}");
        }
        assert!(
            parse("pubspec.lock", b"packages: {}\nsdks: {}\n")
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn zero_budget_never_reads() {
        assert!(remaining(Instant::now()).is_err());
    }
}
