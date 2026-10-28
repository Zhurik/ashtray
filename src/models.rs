use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::error::ApiError;

pub const MAX_AUTHOR_CHARS: usize = 40;
pub const MAX_URL_CHARS: usize = 200;
pub const MAX_BODY_CHARS: usize = 800;

pub const DEFAULT_PER_PAGE: u32 = 20;
pub const MAX_PER_PAGE: u32 = 100;

/// How long each field may be and how large a page may get. The constants
/// above are the defaults; the environment can move every one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryLimits {
    pub author_chars: usize,
    pub url_chars: usize,
    pub body_chars: usize,
    pub default_per_page: u32,
    pub max_per_page: u32,
}

impl Default for EntryLimits {
    fn default() -> Self {
        Self {
            author_chars: MAX_AUTHOR_CHARS,
            url_chars: MAX_URL_CHARS,
            body_chars: MAX_BODY_CHARS,
            default_per_page: DEFAULT_PER_PAGE,
            max_per_page: MAX_PER_PAGE,
        }
    }
}

/// One entry as it is stored and as it is handed back.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Entry {
    pub id: i64,
    pub author: Option<String>,
    pub url: Option<String>,
    pub body: String,
    pub created_at: String,
}

/// What a visitor sends. Everything but the body is optional.
#[derive(Debug, Deserialize)]
pub struct NewEntry {
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    pub body: String,
}

/// A `NewEntry` that has been trimmed and checked.
#[derive(Debug)]
pub struct ValidEntry {
    pub author: Option<String>,
    pub url: Option<String>,
    pub body: String,
}

impl NewEntry {
    pub fn validate(self, limits: &EntryLimits) -> Result<ValidEntry, ApiError> {
        let author = optional(self.author);
        if let Some(author) = &author
            && author.chars().count() > limits.author_chars
        {
            return Err(ApiError::Invalid(format!(
                "The name is longer than {} characters.",
                limits.author_chars
            )));
        }

        let url = match optional(self.url) {
            None => None,
            Some(url) => Some(validate_url(url, limits.url_chars)?),
        };

        let body = optional(self.body)
            .ok_or_else(|| ApiError::Invalid("The message is empty.".to_owned()))?;
        if body.chars().count() > limits.body_chars {
            return Err(ApiError::Invalid(format!(
                "The message is longer than {} characters.",
                limits.body_chars
            )));
        }

        Ok(ValidEntry { author, url, body })
    }
}

fn optional(value: impl Into<Option<String>>) -> Option<String> {
    value
        .into()
        .map(|raw| raw.trim().to_owned())
        .filter(|trimmed| !trimmed.is_empty())
}

fn validate_url(url: String, max_chars: usize) -> Result<String, ApiError> {
    if url.chars().count() > max_chars {
        return Err(ApiError::Invalid(format!(
            "The link is longer than {max_chars} characters."
        )));
    }

    let parsed = url::Url::parse(&url)
        .map_err(|_| ApiError::Invalid("The link is not a URL.".to_owned()))?;

    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(ApiError::Invalid(
            "The link has to be http or https.".to_owned(),
        ));
    }

    Ok(url)
}

/// Query string of the list endpoint.
#[derive(Debug, Deserialize)]
pub struct Pagination {
    #[serde(default)]
    pub page: Option<u32>,
    #[serde(default)]
    pub per_page: Option<u32>,
}

impl Pagination {
    /// Out-of-range values are clamped rather than rejected: a visitor walking
    /// off the end of the list should get an empty page, not an error.
    pub fn page(&self) -> u32 {
        self.page.unwrap_or(1).max(1)
    }

    pub fn per_page(&self, limits: &EntryLimits) -> u32 {
        self.per_page
            .unwrap_or(limits.default_per_page)
            .clamp(1, limits.max_per_page)
    }

    pub fn offset(&self, limits: &EntryLimits) -> i64 {
        i64::from(self.page() - 1) * i64::from(self.per_page(limits))
    }
}

/// One page of entries plus what the client needs to ask for the next one.
#[derive(Debug, Serialize)]
pub struct EntryPage {
    pub items: Vec<Entry>,
    pub page: u32,
    pub per_page: u32,
    pub total: i64,
    pub total_pages: u32,
}
