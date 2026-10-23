use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! codes {
    ($($name:ident),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum Code {
            $($name,)*
        }

        impl Code {
            pub const ALL: &'static [Code] = &[$(Code::$name),*];

            pub fn as_str(self) -> &'static str {
                match self {
                    $(Code::$name => stringify!($name),)*
                }
            }
        }
    };
}

codes! {
    Conflict, HeadMoved, Dirty, Busy, NoSuchPath, NoSuchBranch, NoSuchCommit,
    NoCommit, NoMergeBase, IsDirectory, NotDirectory, NotFile, Binary, NotText,
    NoMatch, AmbiguousMatch, Exists, NotEmpty, NameTaken, ReservedName, ReadOnly,
    TooLarge, InvalidInput,
    NoSuchSession, NoSuchRepo, NoSuchCheckpoint, NoSuchProposal, NoSuchView,
    NotConfirmed, NotTty, OpenFiles,
    CrossVolume, Drift, CheckFailed, CheckMutates, PushRefused, Unknown, Unsupported,
    Corrupt,
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrataError {
    pub code: Code,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

impl StrataError {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: serde_json::Value) -> Self {
        self.detail = Some(detail);
        self
    }
}

impl fmt::Display for StrataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for StrataError {}
