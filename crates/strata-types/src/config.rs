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

    pub fn needs_human_confirm(&self) -> bool {
        self.publish.confirm || self.deploy_on_push
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
        }
        Ok(())
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
    if cfg.deploy_on_push && !cfg.publish.confirm {
        return Err(violation(
            Code::InvalidInput,
            Some(repo),
            "deploy_on_push",
            None,
            "V5",
            "deploy_on_push requires publish.confirm",
        ));
    }
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
