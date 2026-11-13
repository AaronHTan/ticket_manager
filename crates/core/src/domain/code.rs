use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Names a source repository known to the handler, such as `"web-app"`.
///
/// This is a configuration key, not a URL. The remote it points to is
/// looked up in the repository directory, so tickets can never make the
/// service clone arbitrary locations.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RepoId(String);

impl RepoId {
    /// Maximum length of a repository id.
    pub const MAX_LEN: usize = 128;

    /// Creates a repository id. Allowed characters: ASCII alphanumerics, `-`, `_`, `.`.
    /// It may not start with `.` or `-`.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let trimmed = value.trim();
        let valid_chars = trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if trimmed.is_empty()
            || trimmed.len() > Self::MAX_LEN
            || !valid_chars
            || trimmed.starts_with(['.', '-'])
        {
            return Err(Error::Validation(format!(
                "repository id `{trimmed}` must be 1-{} characters of [A-Za-z0-9._-], \
                 not starting with `.` or `-`",
                Self::MAX_LEN
            )));
        }
        Ok(Self(trimmed.to_owned()))
    }

    /// Returns the id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RepoId {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::new(value)
    }
}

impl From<RepoId> for String {
    fn from(value: RepoId) -> Self {
        value.0
    }
}

impl FromStr for RepoId {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::new(s)
    }
}

impl fmt::Display for RepoId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Hints from the reporter about which code a ticket concerns.
///
/// Every field is optional and none is trusted: the repository must still
/// resolve through configuration, and paths are only ever read relative to a
/// checked-out workspace.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CodeContext {
    /// The repository the problem is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<RepoId>,
    /// Branch, tag or commit where the problem was seen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    /// Repository-relative paths thought to be involved.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// Released version where the problem was seen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl CodeContext {
    /// Maximum number of paths.
    pub const MAX_PATHS: usize = 64;
    /// Maximum length of a path, ref or version, in bytes.
    pub const MAX_FIELD_LEN: usize = 512;

    /// Checks sizes and rejects refs and paths that could escape a workspace
    /// or be read as command-line options.
    pub fn validate(&self) -> Result<()> {
        if self.paths.len() > Self::MAX_PATHS {
            return Err(Error::Validation(format!(
                "code context allows at most {} paths",
                Self::MAX_PATHS
            )));
        }
        for path in &self.paths {
            validate_relative_path(path)?;
        }
        if let Some(git_ref) = &self.git_ref {
            check_len("git ref", git_ref, Self::MAX_FIELD_LEN)?;
            if git_ref.starts_with('-') || git_ref.chars().any(|c| c.is_control() || c == ' ') {
                return Err(Error::Validation(format!("invalid git ref `{git_ref}`")));
            }
        }
        if let Some(version) = &self.version {
            check_len("version", version, Self::MAX_FIELD_LEN)?;
        }
        Ok(())
    }

    /// True if no hint is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

fn validate_relative_path(path: &str) -> Result<()> {
    check_len("path", path, CodeContext::MAX_FIELD_LEN)?;
    let escapes = path.is_empty()
        || path.starts_with(['/', '\\', '-'])
        || path.contains(':')
        || path.chars().any(char::is_control)
        || path.split(['/', '\\']).any(|part| part == "..");
    if escapes {
        return Err(Error::Validation(format!(
            "path `{path}` must be relative to the repository root and stay inside it"
        )));
    }
    Ok(())
}

pub(crate) fn check_len(what: &str, value: &str, max: usize) -> Result<()> {
    if value.len() > max {
        return Err(Error::Validation(format!("{what} exceeds {max} bytes")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_id_rejects_invalid_values() {
        assert!(RepoId::new("web-app_2.0").is_ok());
        assert!(RepoId::new("").is_err());
        assert!(RepoId::new("../etc").is_err());
        assert!(RepoId::new("a/b").is_err());
        assert!(RepoId::new(".hidden").is_err());
        assert!(RepoId::new("-rf").is_err());
        assert!(RepoId::new("r".repeat(129)).is_err());
        assert!(serde_json::from_str::<RepoId>("\"bad id\"").is_err());
    }

    #[test]
    fn code_context_rejects_escaping_paths_and_option_like_refs() {
        let ok = CodeContext {
            repository: Some(RepoId::new("web-app").unwrap()),
            git_ref: Some("release/1.2".into()),
            paths: vec!["src/login.rs".into(), "docs/a..b.md".into()],
            version: Some("1.2.3".into()),
        };
        assert!(ok.validate().is_ok());

        for path in ["../secret", "src/../../x", "/etc/passwd", "C:\\x", "-x", ""] {
            let ctx = CodeContext {
                paths: vec![path.into()],
                ..CodeContext::default()
            };
            assert!(ctx.validate().is_err(), "{path} should be rejected");
        }
        for git_ref in ["--upload-pack=evil", "a b", "a\nb"] {
            let ctx = CodeContext {
                git_ref: Some(git_ref.into()),
                ..CodeContext::default()
            };
            assert!(ctx.validate().is_err(), "{git_ref} should be rejected");
        }
        let too_many = CodeContext {
            paths: vec!["a".into(); CodeContext::MAX_PATHS + 1],
            ..CodeContext::default()
        };
        assert!(too_many.validate().is_err());
    }

    #[test]
    fn empty_code_context_serialises_compactly() {
        assert!(CodeContext::default().is_empty());
        assert_eq!(
            serde_json::to_string(&CodeContext::default()).unwrap(),
            "{}"
        );
    }
}
