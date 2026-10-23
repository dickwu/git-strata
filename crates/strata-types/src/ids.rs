use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::error::{Code, StrataError};

fn invalid(message: impl Into<String>) -> StrataError {
    StrataError::new(Code::InvalidInput, message)
}

fn hex_ok(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RepoName(String);

impl RepoName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RepoName {
    type Error = StrataError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() || value == "." || value == ".." {
            return Err(invalid("repo name is empty or reserved"));
        }
        if !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
        {
            return Err(invalid("repo name has illegal characters"));
        }
        Ok(Self(value))
    }
}

impl From<RepoName> for String {
    fn from(value: RepoName) -> Self {
        value.0
    }
}

impl std::fmt::Display for RepoName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RepoId(PathBuf);

impl RepoId {
    pub fn new(path: PathBuf) -> Self {
        Self(path)
    }

    pub fn get(&self) -> &Path {
        &self.0
    }
}

impl std::fmt::Display for RepoId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.display())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SessionId(String);

impl SessionId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for SessionId {
    type Error = StrataError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let Some(rest) = value.strip_prefix("s-") else {
            return Err(invalid("session id must start with s-"));
        };
        let ok = rest.len() == 8
            && rest
                .bytes()
                .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b));
        if !ok {
            return Err(invalid(
                "session id must be s- plus 8 lowercase base32 characters",
            ));
        }
        Ok(Self(value))
    }
}

impl From<SessionId> for String {
    fn from(value: SessionId) -> Self {
        value.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

macro_rules! hex_id {
    ($name:ident, $len:literal, $what:literal) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = StrataError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                if !hex_ok(&value, $len) {
                    return Err(invalid(concat!($what, " has the wrong form")));
                }
                Ok(Self(value))
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

hex_id!(TreeOid, 40, "tree oid");
hex_id!(CommitOid, 40, "commit oid");
hex_id!(BlobId, 64, "blob id");
hex_id!(DepsSetId, 64, "deps set id");

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RepoPath(String);

impl RepoPath {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RepoPath {
    type Error = StrataError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let normalized: String = value.nfc().collect();
        if normalized.is_empty()
            || normalized.starts_with('/')
            || normalized.ends_with('/')
            || normalized.contains("//")
        {
            return Err(invalid(
                "repo path must be relative, without empty segments",
            ));
        }
        for segment in normalized.split('/') {
            if segment.is_empty() || segment == "." || segment == ".." {
                return Err(invalid("repo path contains an empty, '.' or '..' segment"));
            }
        }
        Ok(Self(normalized))
    }
}

impl From<RepoPath> for String {
    fn from(value: RepoPath) -> Self {
        value.0
    }
}

impl std::fmt::Display for RepoPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

macro_rules! num_id {
    ($name:ident, $what:literal) => {
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(try_from = "u64", into = "u64")]
        pub struct $name(u64);

        impl $name {
            pub fn get(self) -> u64 {
                self.0
            }
        }

        impl TryFrom<u64> for $name {
            type Error = StrataError;

            fn try_from(value: u64) -> Result<Self, Self::Error> {
                if value > i64::MAX as u64 {
                    return Err(invalid(concat!($what, " exceeds i64::MAX")));
                }
                Ok(Self(value))
            }
        }

        impl From<$name> for u64 {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl From<i64> for $name {
            fn from(value: i64) -> Self {
                Self(value as u64)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

num_id!(CheckpointId, "checkpoint id");
num_id!(ProposalId, "proposal id");

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ViewId(String);

impl ViewId {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn get(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ViewId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
