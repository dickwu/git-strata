use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Code, StrataError};
use crate::ids::{RepoName, RepoPath};
use crate::model::PublishTarget;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub paths: Paths,
    #[serde(rename = "repo")]
    pub repos: std::collections::BTreeMap<RepoName, RepoConfig>,
    /// `[status]` (config.md §2.5, status-perf D7). Omitted when default,
    /// so a config without the table writes back without it.
    #[serde(default, skip_serializing_if = "StatusConfig::is_default")]
    pub status: StatusConfig,
    /// `[dev_ports]`: the port blocks leased to session workspaces for dev
    /// servers (config.md §2.6). Optional; absent → defaults.
    #[serde(default)]
    pub dev_ports: DevPorts,
}

/// `[status]`: how `status` runs (status-perf D7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusConfig {
    /// How many workspaces `status` evaluates at once, across all requests
    /// of one process (1..=16, V13).
    #[serde(default = "StatusConfig::default_parallel")]
    pub parallel: u32,
}

impl Default for StatusConfig {
    fn default() -> Self {
        Self { parallel: Self::default_parallel() }
    }
}

impl StatusConfig {
    pub const MAX_PARALLEL: u32 = 16;

    pub fn default_parallel() -> u32 {
        4
    }

    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// `[dev_ports]` (config.md §2.6): `range = "LO-HI"` (inclusive) is split
/// into 4-aligned blocks of [`DevPorts::BLOCK`] ports; a block holding any
/// port of `exclude` is never leased.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevPorts {
    #[serde(default = "DevPorts::default_range")]
    pub range: String,
    #[serde(default)]
    pub exclude: Vec<u16>,
}

impl Default for DevPorts {
    fn default() -> Self {
        Self { range: Self::default_range(), exclude: Vec::new() }
    }
}

/// Ranges a dev-port range must not overlap: common dev-server defaults
/// (3000–3999 for Next, the Hub on 3000; 4001 for git-strata's own desktop
/// GUI; 5173 for Vite; 8080) and tauri-plugin-connector's default ranges
/// (bridge 9300–9400, git-strata 9300; WS/MCP 9555–9656, which holds the
/// GIT STRATA app's 9555/9556 and the Hub's pinned WS 9640–9649 / MCP
/// 9650–9654).
const DEV_PORTS_FORBIDDEN: &[(u16, u16)] = &[(3000, 3999), (4001, 4001), (5173, 5173), (8080, 8080), (9300, 9400), (9555, 9656)];

impl DevPorts {
    /// Ports per lease: web, connector, connector_mcp, aux.
    pub const BLOCK: u16 = 4;

    pub fn default_range() -> String {
        "20000-29999".to_string()
    }

    fn bad(field: &str, rule: &str, message: String) -> StrataError {
        violation(Code::InvalidInput, None, field, None, rule, &message)
    }

    /// `(lo, hi)` of `range`, inclusive; the P1/P2 rules.
    pub fn bounds(&self) -> Result<(u16, u16), StrataError> {
        let field = "dev_ports.range";
        let parsed = self.range.split_once('-').and_then(|(a, b)| Some((a.trim().parse::<u16>().ok()?, b.trim().parse::<u16>().ok()?)));
        let Some((lo, hi)) = parsed.filter(|(lo, hi)| lo <= hi) else {
            return Err(Self::bad(field, "P1", format!("dev_ports.range {:?} is not \"LO-HI\" with LO <= HI", self.range)));
        };
        if lo < 1024 || hi > 49151 {
            return Err(Self::bad(field, "P2", format!("dev_ports.range {lo}-{hi} must lie within 1024-49151 (below macOS's ephemeral ports)")));
        }
        if let Some((a, b)) = DEV_PORTS_FORBIDDEN.iter().find(|(a, b)| lo <= *b && *a <= hi) {
            return Err(Self::bad(field, "P2", format!("dev_ports.range {lo}-{hi} overlaps {a}-{b}, a default dev-server or connector port range")));
        }
        Ok((lo, hi))
    }

    /// The bases of every usable block, ascending.
    pub fn blocks(&self) -> Result<Vec<u16>, StrataError> {
        let (lo, hi) = self.bounds()?;
        let first = lo.div_ceil(Self::BLOCK) * Self::BLOCK;
        let mut v = Vec::new();
        let mut b = u32::from(first);
        while b + u32::from(Self::BLOCK) - 1 <= u32::from(hi) {
            let base = b as u16;
            if !self.exclude.iter().any(|p| (base..base + Self::BLOCK).contains(p)) {
                v.push(base);
            }
            b += u32::from(Self::BLOCK);
        }
        Ok(v)
    }

    /// P1–P3: a well-formed range outside the forbidden ranges that leaves
    /// at least one block; exclusions must be ports (not 0).
    pub fn validate(&self) -> Result<(), StrataError> {
        if let Some(i) = self.exclude.iter().position(|p| *p == 0) {
            return Err(violation(Code::InvalidInput, None, "dev_ports.exclude", Some(i), "P1", "dev_ports.exclude has port 0"));
        }
        if self.blocks()?.is_empty() {
            return Err(Self::bad("dev_ports.range", "P3", format!("dev_ports.range {:?} with its exclusions leaves no block of 4 ports", self.range)));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Paths {
    #[serde(default = "Paths::default_root")]
    pub root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    pub path: PathBuf,
    pub default_branch: String,
    #[serde(default = "RepoConfig::default_deps_dirs")]
    pub deps_dirs: Vec<RepoPath>,
    #[serde(default = "RepoConfig::default_seed_caches")]
    pub seed_caches: Vec<RepoPath>,
    #[serde(default)]
    pub seed_files: Option<Vec<RepoPath>>,
    #[serde(default)]
    pub install_inputs: Vec<RepoPath>,
    #[serde(default)]
    pub install: Option<String>,
    #[serde(default)]
    pub checks: Vec<String>,
    pub publish: PublishTarget,
    #[serde(default)]
    pub deploy_on_push: bool,
    #[serde(default = "RepoConfig::default_pin_unpushed")]
    pub pin_unpushed: bool,
    /// Repos that must sit at `../<name>` of the staged repo for its
    /// `checks` to be trustworthy (dev-link; config.md §2.3 item 5).
    /// Publish supplies each one: the layout session's pinned base when the
    /// session has the repo, else the repo's current main-tip base
    /// (publish.md D21). Empty by default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub check_siblings: Vec<RepoName>,
    /// `[repo.X.rm]` (rm-followups §1.8). Absent → defaults.
    #[serde(default, skip_serializing_if = "RmConfig::is_default")]
    pub rm: RmConfig,
    /// `[repo.X.rebase]` (rm-followups §1.3). Absent → defaults.
    #[serde(default, skip_serializing_if = "RebaseConfig::is_default")]
    pub rebase: RebaseConfig,
}

/// `[repo.X.rm]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RmConfig {
    /// Repo-relative globs (the `.lww` dialect, last match wins, `!`
    /// negates) of files a tool regenerates: their Modified/Added/Untracked
    /// edits do not block `rm` (they are checkpointed) and a conflict on
    /// them does not defer an auto-rebase. Pinned like `publish.confirm`:
    /// a looser list takes effect only after `config relax`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regenerable: Vec<String>,
}

impl RmConfig {
    pub fn is_default(&self) -> bool {
        self.regenerable.is_empty()
    }
}

/// What an automatic rebase does when the publish changed the install
/// inputs (rm-followups decision 1b).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DepsChanged {
    /// Move the source onto the new tree with the old deps set; the deps
    /// proposal stays Pending.
    #[default]
    MoveSource,
    /// Do not move (today's `Waiting`).
    Wait,
}

/// `[repo.X.rebase]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RebaseConfig {
    #[serde(default)]
    pub deps_changed: DepsChanged,
    /// The quiet window Q of the automatic rebase, in seconds.
    #[serde(default = "RebaseConfig::default_quiet_secs")]
    pub quiet_secs: u64,
}

impl Default for RebaseConfig {
    fn default() -> Self {
        RebaseConfig { deps_changed: DepsChanged::default(), quiet_secs: Self::default_quiet_secs() }
    }
}

impl RebaseConfig {
    pub const MAX_QUIET_SECS: u64 = 86_400;

    pub fn default_quiet_secs() -> u64 {
        120
    }

    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl Default for Paths {
    fn default() -> Self {
        Self {
            root: Self::default_root(),
        }
    }
}

impl Paths {
    pub fn default_root() -> PathBuf {
        PathBuf::from("/Volumes/GWD/inter/.strata")
    }

    pub fn store(&self) -> PathBuf {
        self.root.join("store")
    }

    pub fn bases(&self) -> PathBuf {
        self.root.join("bases")
    }

    pub fn deps(&self) -> PathBuf {
        self.root.join("deps")
    }

    pub fn sessions(&self) -> PathBuf {
        self.root.join("sessions")
    }
}

fn must_path(text: &str) -> RepoPath {
    RepoPath::try_from(text.to_owned()).expect("built-in repo path")
}

impl RepoConfig {
    pub fn default_deps_dirs() -> Vec<RepoPath> {
        vec![must_path("node_modules"), must_path("vendor")]
    }

    pub fn default_seed_caches() -> Vec<RepoPath> {
        vec![
            must_path("src-tauri/target"),
            must_path(".next/cache"),
            must_path("node_modules/.cache"),
        ]
    }

    pub fn default_seed_files() -> Vec<RepoPath> {
        vec![
            must_path("CLAUDE.md"),
            must_path(".env"),
            must_path(".env.local"),
            must_path(".claude/settings.local.json"),
        ]
    }

    pub fn seed_files(&self) -> (Vec<RepoPath>, bool) {
        match &self.seed_files {
            None => (Self::default_seed_files(), false),
            Some(files) => (files.clone(), true),
        }
    }

    pub fn default_pin_unpushed() -> bool {
        true
    }

    /// `publish.confirm` is the only gate; `deploy_on_push` is information
    /// (it names the action `PushDeploy` and shows in status), not a gate.
    pub fn needs_human_confirm(&self) -> bool {
        self.publish.confirm
    }

    /// `install` split by the §2.4 rules (the same splitter `validate_commands`
    /// uses): `Ok(None)` without `install`, `InvalidInput` (V9) when the line
    /// does not split into a non-empty argv. Used by deps (deps.md §4.3, §5.2).
    pub fn install_argv(&self) -> Result<Option<Vec<String>>, StrataError> {
        let Some(install) = &self.install else {
            return Ok(None);
        };
        check_argv(install, "install", None)?;
        split_command(install)
            .map(Some)
            .map_err(|()| StrataError::new(Code::InvalidInput, "install does not split"))
    }

    pub fn validate_commands(&self) -> Result<(), StrataError> {
        if let Some(install) = &self.install {
            check_argv(install, "install", None)?;
        }
        for (index, check) in self.checks.iter().enumerate() {
            check_argv(check, "checks", Some(index))?;
        }
        for (index, check) in self.checks.iter().enumerate() {
            if command_mutates(check) {
                return Err(violation(
                    Code::CheckMutates,
                    None,
                    "checks",
                    Some(index),
                    "V10",
                    "check command would modify files",
                ));
            }
        }
        Ok(())
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, StrataError> {
        let value: toml::Value = toml::from_str(text).map_err(toml_error)?;
        let order = repo_order(&value)?;
        let config: Config = toml::from_str(text).map_err(toml_error)?;
        config.validate_ordered(&order)?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), StrataError> {
        let order: Vec<RepoName> = self.repos.keys().cloned().collect();
        self.validate_ordered(&order)
    }

    pub fn repo(&self, name: &RepoName) -> Result<&RepoConfig, StrataError> {
        self.repos
            .get(name)
            .ok_or_else(|| StrataError::new(Code::NoSuchRepo, format!("no repo named {name}")))
    }

    fn validate_ordered(&self, order: &[RepoName]) -> Result<(), StrataError> {
        if !is_canonical_absolute(&self.paths.root) {
            return Err(violation(
                Code::InvalidInput,
                None,
                "root",
                None,
                "V2",
                "paths.root is not an absolute canonical path",
            ));
        }
        if self.repos.is_empty() {
            return Err(violation(
                Code::InvalidInput,
                None,
                "repo",
                None,
                "V1",
                "repo table is empty",
            ));
        }
        let names: Vec<RepoName> = if order.is_empty() {
            self.repos.keys().cloned().collect()
        } else {
            order.to_vec()
        };
        for name in &names {
            let Some(cfg) = self.repos.get(name) else {
                continue;
            };
            validate_repo(name, cfg, &self.paths.root)?;
            validate_check_siblings(name, cfg, &self.repos)?;
            validate_rm_rebase(name, cfg)?;
        }
        if !(1..=StatusConfig::MAX_PARALLEL).contains(&self.status.parallel) {
            return Err(violation(
                Code::InvalidInput,
                None,
                "parallel",
                None,
                "V13",
                "status.parallel is not between 1 and 16",
            ));
        }
        self.dev_ports.validate()
    }
}

fn repo_order(value: &toml::Value) -> Result<Vec<RepoName>, StrataError> {
    let Some(table) = value.as_table() else {
        return Ok(Vec::new());
    };
    let Some(repos) = table.get("repo").and_then(toml::Value::as_table) else {
        return Ok(Vec::new());
    };
    let mut order = Vec::new();
    for key in repos.keys() {
        let name = RepoName::try_from(key.clone()).map_err(|_| {
            violation(
                Code::InvalidInput,
                Some(key),
                "repo",
                None,
                "V1",
                "illegal repo name",
            )
        })?;
        order.push(name);
    }
    Ok(order)
}

fn validate_repo(name: &RepoName, cfg: &RepoConfig, root: &Path) -> Result<(), StrataError> {
    let repo = name.as_str();
    let basename = cfg.path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if basename != repo {
        return Err(violation(
            Code::InvalidInput,
            Some(repo),
            "path",
            None,
            "V1",
            "repo name must equal the last path component",
        ));
    }
    if !is_canonical_absolute(&cfg.path) {
        return Err(violation(
            Code::InvalidInput,
            Some(repo),
            "path",
            None,
            "V2",
            "repo path is not an absolute canonical path",
        ));
    }
    if is_within(&cfg.path, root) || is_within(root, &cfg.path) {
        return Err(violation(
            Code::InvalidInput,
            Some(repo),
            "path",
            None,
            "V3",
            "paths.root and the checkout must not contain each other",
        ));
    }
    if !valid_branch(&cfg.default_branch) {
        return Err(violation(
            Code::InvalidInput,
            Some(repo),
            "default_branch",
            None,
            "V4",
            "default_branch is not a branch short name",
        ));
    }
    if !valid_branch(&cfg.publish.branch) {
        return Err(violation(
            Code::InvalidInput,
            Some(repo),
            "publish.branch",
            None,
            "V4",
            "publish.branch is not a branch short name",
        ));
    }
    // V5 (deploy_on_push requires publish.confirm) was dropped on 2026-10-26:
    // `publish.confirm` is the single, explicit gate (config.md V5).
    let (seeds, _) = cfg.seed_files();
    let lists = [
        ("deps_dirs", cfg.deps_dirs.as_slice()),
        ("seed_caches", cfg.seed_caches.as_slice()),
        ("seed_files", seeds.as_slice()),
        ("install_inputs", cfg.install_inputs.as_slice()),
    ];
    for (field, list) in lists {
        check_list(repo, field, list)?;
    }
    check_across(
        repo,
        &cfg.deps_dirs,
        &cfg.seed_caches,
        &seeds,
        &cfg.install_inputs,
    )?;
    let has_deps = !cfg.deps_dirs.is_empty();
    let has_install = cfg.install.is_some();
    let has_inputs = !cfg.install_inputs.is_empty();
    if has_deps != (has_install && has_inputs) || (!has_deps && (has_install || has_inputs)) {
        return Err(violation(
            Code::InvalidInput,
            Some(repo),
            "deps_dirs",
            None,
            "V8",
            "deps_dirs, install and install_inputs must be present together",
        ));
    }
    if has_deps && (cfg.publish.branch != cfg.default_branch || !cfg.pin_unpushed) {
        return Err(violation(
            Code::InvalidInput,
            Some(repo),
            "pin_unpushed",
            None,
            "design-6.7",
            "a repo with deps_dirs must publish the default branch and pin_unpushed",
        ));
    }
    cfg.validate_commands().map_err(|err| stamp_repo(err, repo))
}

fn check_list(repo: &str, field: &str, list: &[RepoPath]) -> Result<(), StrataError> {
    for (index, path) in list.iter().enumerate() {
        if is_git_path(path.as_str()) {
            return Err(violation(
                Code::InvalidInput,
                Some(repo),
                field,
                Some(index),
                "V6",
                "path list entry must not be .git",
            ));
        }
        for earlier in &list[..index] {
            if earlier.as_str() == path.as_str() || nests(earlier.as_str(), path.as_str()) {
                return Err(violation(
                    Code::InvalidInput,
                    Some(repo),
                    field,
                    Some(index),
                    "V6",
                    "path list has a duplicate or nested entry",
                ));
            }
        }
    }
    Ok(())
}

fn check_across(
    repo: &str,
    deps: &[RepoPath],
    caches: &[RepoPath],
    seeds: &[RepoPath],
    inputs: &[RepoPath],
) -> Result<(), StrataError> {
    let groups: [(&str, &[RepoPath]); 4] = [
        ("deps_dirs", deps),
        ("seed_caches", caches),
        ("seed_files", seeds),
        ("install_inputs", inputs),
    ];
    for (i, (field_a, list_a)) in groups.iter().enumerate() {
        for (field_b, list_b) in groups.iter().skip(i + 1) {
            for (index, path) in list_b.iter().enumerate() {
                for other in *list_a {
                    if allowed_seed_cache(field_a, other.as_str(), field_b, path.as_str()) {
                        continue;
                    }
                    if other.as_str() == path.as_str() || nests(other.as_str(), path.as_str()) {
                        return Err(violation(
                            Code::InvalidInput,
                            Some(repo),
                            field_b,
                            Some(index),
                            "V7",
                            "path lists overlap",
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn allowed_seed_cache(field_a: &str, a: &str, field_b: &str, b: &str) -> bool {
    let (cache, dep) = if field_a == "deps_dirs" && field_b == "seed_caches" {
        (b, a)
    } else if field_a == "seed_caches" && field_b == "deps_dirs" {
        (a, b)
    } else {
        return false;
    };
    cache != dep && cache.starts_with(dep) && cache.as_bytes().get(dep.len()) == Some(&b'/')
}

fn is_git_path(path: &str) -> bool {
    path == ".git" || path.starts_with(".git/")
}

fn nests(a: &str, b: &str) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    long != short && long.starts_with(short) && long.as_bytes().get(short.len()) == Some(&b'/')
}

fn stamp_repo(mut err: StrataError, repo: &str) -> StrataError {
    let mut detail = err.detail.take().unwrap_or_else(|| serde_json::json!({}));
    if let Some(obj) = detail.as_object_mut() {
        obj.insert(
            "repo".to_owned(),
            serde_json::Value::String(repo.to_owned()),
        );
    }
    err.detail = Some(detail);
    err
}

/// V12: every `check_siblings` entry is another configured repo, listed once.
fn validate_check_siblings(
    name: &RepoName,
    cfg: &RepoConfig,
    repos: &std::collections::BTreeMap<RepoName, RepoConfig>,
) -> Result<(), StrataError> {
    let repo = name.as_str();
    for (i, s) in cfg.check_siblings.iter().enumerate() {
        let message = if s == name {
            "check_siblings names the repo itself"
        } else if !repos.contains_key(s) {
            "check_siblings names a repo that is not configured"
        } else if cfg.check_siblings[..i].contains(s) {
            "check_siblings lists a repo twice"
        } else {
            continue;
        };
        return Err(violation(Code::InvalidInput, Some(repo), "check_siblings", Some(i), "V12", message));
    }
    Ok(())
}

/// V14: `rm.regenerable` patterns are repo-relative globs: not absolute,
/// no `..` component, nothing that could match under `.git`, one line each,
/// not a `.lww` comment or include. V15: `rebase.quiet_secs` ≤ one day.
fn validate_rm_rebase(name: &RepoName, cfg: &RepoConfig) -> Result<(), StrataError> {
    let repo = name.as_str();
    for (i, pat) in cfg.rm.regenerable.iter().enumerate() {
        if let Some(message) = regenerable_problem(pat) {
            return Err(violation(Code::InvalidInput, Some(repo), "rm.regenerable", Some(i), "V14", message));
        }
    }
    if cfg.rebase.quiet_secs > RebaseConfig::MAX_QUIET_SECS {
        return Err(violation(
            Code::InvalidInput,
            Some(repo),
            "rebase.quiet_secs",
            None,
            "V15",
            "rebase.quiet_secs is more than 86400 (one day)",
        ));
    }
    Ok(())
}

/// Why one `rm.regenerable` pattern is rejected, if it is.
pub fn regenerable_problem(pat: &str) -> Option<&'static str> {
    let body = pat.strip_prefix('!').unwrap_or(pat);
    if body.trim().is_empty() {
        return Some("rm.regenerable has an empty pattern");
    }
    if pat.contains('\n') || pat.contains('\r') || pat.contains('\0') {
        return Some("rm.regenerable patterns are one line each");
    }
    if pat.trim() != pat || body.trim() != body {
        return Some("rm.regenerable patterns have no leading or trailing blanks");
    }
    if pat.starts_with('#') || pat.starts_with("%include") {
        return Some("rm.regenerable patterns are globs, not .lww comments or includes");
    }
    if body.starts_with('/') {
        return Some("rm.regenerable patterns are repo-relative, not absolute");
    }
    let comps: Vec<&str> = body.split('/').collect();
    if comps.contains(&"..") {
        return Some("rm.regenerable patterns may not contain ..");
    }
    // `.git` itself, or a name glob that could stretch to `.git` (`*`,
    // `?git`, `.g*`, ...), case-folded as APFS is. A `**` component is
    // allowed (`generated/**`), but not a pattern of wildcards only.
    let could_be_git = |c: &str| {
        let c = c.to_lowercase();
        c != "**" && (c == ".git" || git_glob(&c.chars().collect::<Vec<_>>(), &".git".chars().collect::<Vec<_>>()))
    };
    if comps.iter().any(|c| could_be_git(c)) {
        return Some("rm.regenerable patterns may not match .git or anything under it");
    }
    if comps.iter().all(|c| c.chars().all(|ch| matches!(ch, '*' | '?'))) {
        return Some("rm.regenerable patterns must name something (not wildcards only)");
    }
    None
}

/// Whether one glob component (`*`, `?`, literals; `\\` escapes) matches `t`.
fn git_glob(p: &[char], t: &[char]) -> bool {
    match p.split_first() {
        None => t.is_empty(),
        Some(('*', rest)) => (0..=t.len()).any(|k| git_glob(rest, &t[k..])),
        Some(('?', rest)) => !t.is_empty() && git_glob(rest, &t[1..]),
        Some(('\\', rest)) if !rest.is_empty() => t.first() == Some(&rest[0]) && git_glob(&rest[1..], &t[1..]),
        Some((c, rest)) => t.first() == Some(c) && git_glob(rest, &t[1..]),
    }
}

fn violation(
    code: Code,
    repo: Option<&str>,
    field: &str,
    index: Option<usize>,
    rule: &str,
    message: &str,
) -> StrataError {
    StrataError::new(code, message).with_detail(serde_json::json!({
        "repo": repo,
        "field": field,
        "index": index,
        "rule": rule,
    }))
}

fn toml_error(err: toml::de::Error) -> StrataError {
    let message = err.to_string();
    let rule = if message.contains("unknown field") {
        "V11"
    } else {
        "toml"
    };
    StrataError::new(Code::InvalidInput, "config could not be parsed").with_detail(
        serde_json::json!({
            "repo": null,
            "field": null,
            "index": null,
            "rule": rule,
            "message": message,
        }),
    )
}

fn is_canonical_absolute(path: &Path) -> bool {
    let Some(text) = path.to_str() else {
        return false;
    };
    if !path.is_absolute() || text.ends_with('/') || text.contains("//") || text.starts_with('~') {
        return false;
    }
    path.components()
        .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn is_within(inner: &Path, outer: &Path) -> bool {
    inner.starts_with(outer) && inner != outer
}

fn valid_branch(name: &str) -> bool {
    if name.is_empty()
        || name == "@"
        || name == "HEAD"
        || name.starts_with('-')
        || name.starts_with('/')
        || name.ends_with('/')
        || name.starts_with("refs/")
        || name.contains("..")
        || name.contains("@{")
        || name.contains("//")
    {
        return false;
    }
    if name.chars().any(|c| {
        c.is_ascii_control() || matches!(c, ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\')
    }) {
        return false;
    }
    name.split('/').all(|part| {
        !part.is_empty()
            && !part.starts_with('.')
            && !part.ends_with('.')
            && !part.ends_with(".lock")
    })
}

fn check_argv(line: &str, field: &str, index: Option<usize>) -> Result<(), StrataError> {
    match split_command(line) {
        Ok(argv) if !argv.is_empty() => Ok(()),
        _ => Err(violation(
            Code::InvalidInput,
            None,
            field,
            index,
            "V9",
            "command line is empty, unquoted-metacharacter, or not splittable",
        )),
    }
}

fn command_mutates(line: &str) -> bool {
    contains_word(line, "--write")
        || (contains_word(line, "cs-fix") && !contains_word(line, "--dry-run"))
}

fn contains_word(line: &str, word: &str) -> bool {
    let bytes = line.as_bytes();
    let needle = word.as_bytes();
    if needle.is_empty() || bytes.len() < needle.len() {
        return false;
    }
    let mut start = 0;
    while start + needle.len() <= bytes.len() {
        if &bytes[start..start + needle.len()] == needle {
            let before_ok = start == 0 || !is_word_byte(bytes[start - 1]);
            let after = start + needle.len();
            let after_ok = after == bytes.len() || !is_word_byte(bytes[after]);
            if before_ok && after_ok {
                return true;
            }
        }
        start += 1;
    }
    false
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

fn split_command(line: &str) -> Result<Vec<String>, ()> {
    if line.is_empty() {
        return Err(());
    }
    let mut args = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();
    let mut quoting: Option<char> = None;
    let mut token = false;
    while let Some(ch) = chars.next() {
        if let Some(quote) = quoting {
            if ch == quote {
                quoting = None;
                token = true;
                continue;
            }
            if quote == '"' && ch == '\\' {
                let Some(escaped) = chars.next() else {
                    return Err(());
                };
                current.push(escaped);
                token = true;
                continue;
            }
            current.push(ch);
            token = true;
            continue;
        }
        match ch {
            '\\' => {
                let Some(escaped) = chars.next() else {
                    return Err(());
                };
                current.push(escaped);
                token = true;
            }
            '\'' | '"' => {
                quoting = Some(ch);
                token = true;
            }
            '|' | '&' | ';' | '<' | '>' => return Err(()),
            ch if ch.is_whitespace() => {
                if token {
                    args.push(std::mem::take(&mut current));
                    token = false;
                }
            }
            _ => {
                current.push(ch);
                token = true;
            }
        }
    }
    if quoting.is_some() {
        return Err(());
    }
    if token {
        args.push(current);
    }
    if args.is_empty() {
        return Err(());
    }
    Ok(args)
}
