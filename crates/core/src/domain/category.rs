use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// A normalised problem category, such as `"authentication"` or `"billing"`.
///
/// Stored trimmed and lower-cased so that categories coming from different
/// clients and tools compare equal.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Category(String);

impl Category {
    /// Maximum length of a category name.
    pub const MAX_LEN: usize = 64;

    /// Creates a category, normalising case and whitespace.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let normalised = value.into().trim().to_lowercase();
        if normalised.is_empty() || normalised.len() > Self::MAX_LEN {
            return Err(Error::Validation(format!(
                "category must be 1-{} characters",
                Self::MAX_LEN
            )));
        }
        Ok(Self(normalised))
    }

    /// Returns the category as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Category {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::new(value)
    }
}

impl From<Category> for String {
    fn from(value: Category) -> Self {
        value.0
    }
}

impl FromStr for Category {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::new(s)
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_are_normalised() {
        assert_eq!(
            Category::new("  Billing ").unwrap(),
            Category::new("billing").unwrap()
        );
        assert!(Category::new("   ").is_err());
    }
}
