use serde::Serialize;

use crate::models::{MAX_AUTHOR_CHARS, MAX_BODY_CHARS, MAX_URL_CHARS};

/// Why a request was refused, as a code a client can switch on.
///
/// `message` in an error response is English prose for whoever is reading the
/// response by hand; a client that shows its own wording — in its own language —
/// matches on this instead, which is why the names are part of the API and do
/// not change once published.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    NameTooLong,
    LinkTooLong,
    LinkNotAUrl,
    LinkNotHttp,
    MessageEmpty,
    MessageTooLong,
    /// The body was not JSON, or not the JSON this endpoint expects.
    MalformedJson,
    /// The request came without `Content-Type: application/json`.
    UnsupportedContentType,
    /// The body could not be read off the connection at all.
    UnreadableBody,
    /// Something broke on this side. The cause stays in the log.
    Internal,
}

impl Reason {
    pub fn message(self) -> String {
        match self {
            Self::NameTooLong => format!("The name is longer than {MAX_AUTHOR_CHARS} characters."),
            Self::LinkTooLong => format!("The link is longer than {MAX_URL_CHARS} characters."),
            Self::LinkNotAUrl => "The link is not a URL.".to_owned(),
            Self::LinkNotHttp => "The link has to be http or https.".to_owned(),
            Self::MessageEmpty => "The message is empty.".to_owned(),
            Self::MessageTooLong => {
                format!("The message is longer than {MAX_BODY_CHARS} characters.")
            }
            Self::MalformedJson => {
                "The request body is not the JSON this endpoint expects.".to_owned()
            }
            Self::UnsupportedContentType => {
                "The request needs Content-Type: application/json.".to_owned()
            }
            Self::UnreadableBody => "The request body could not be read.".to_owned(),
            Self::Internal => "The ashtray could not be reached. Try again.".to_owned(),
        }
    }
}
