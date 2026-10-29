use axum::http::StatusCode;
use serde::{Serialize, Serializer};

/// Why a request was refused, as a code a client can switch on.
///
/// `message` in an error response is English prose for whoever is reading the
/// response by hand; a client that shows its own wording — in its own language —
/// matches on this instead, which is why the names are part of the API and do
/// not change once published. Only the name goes over the wire; the values the
/// variants carry are there for the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    NameTooLong {
        max: usize,
    },
    LinkTooLong {
        max: usize,
    },
    LinkNotAUrl,
    LinkNotHttp,
    MessageEmpty,
    MessageTooLong {
        max: usize,
    },
    /// The body was not JSON, or not the JSON this endpoint expects.
    MalformedJson,
    /// The query string did not parse into what this endpoint expects.
    MalformedQuery,
    /// The request came without `Content-Type: application/json`.
    UnsupportedContentType,
    /// The body could not be read off the connection at all.
    UnreadableBody,
    /// The body is over the configured size limit.
    BodyTooLarge,
    /// The client spent its budget for this endpoint.
    RateLimited {
        retry_after_secs: u64,
    },
    /// The handler did not answer within the request timeout.
    Timeout,
    /// The endpoint needs a token and did not get a valid one.
    Unauthorized,
    /// Nothing is routed at that path.
    NotFound,
    /// The path exists, but not for this method.
    MethodNotAllowed,
    /// A layer refused the request with a status none of the above covers.
    Refused(StatusCode),
    /// Something broke on this side. The cause stays in the log.
    Internal,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NameTooLong { .. } => "name_too_long",
            Self::LinkTooLong { .. } => "link_too_long",
            Self::LinkNotAUrl => "link_not_a_url",
            Self::LinkNotHttp => "link_not_http",
            Self::MessageEmpty => "message_empty",
            Self::MessageTooLong { .. } => "message_too_long",
            Self::MalformedJson => "malformed_json",
            Self::MalformedQuery => "malformed_query",
            Self::UnsupportedContentType => "unsupported_content_type",
            Self::UnreadableBody => "unreadable_body",
            Self::BodyTooLarge => "body_too_large",
            Self::RateLimited { .. } => "rate_limited",
            Self::Timeout => "timeout",
            Self::Unauthorized => "unauthorized",
            Self::NotFound => "not_found",
            Self::MethodNotAllowed => "method_not_allowed",
            Self::Refused(_) => "refused",
            Self::Internal => "internal",
        }
    }

    pub fn message(self) -> String {
        match self {
            Self::NameTooLong { max } => format!("The name is longer than {max} characters."),
            Self::LinkTooLong { max } => format!("The link is longer than {max} characters."),
            Self::LinkNotAUrl => "The link is not a URL.".to_owned(),
            Self::LinkNotHttp => "The link has to be http or https.".to_owned(),
            Self::MessageEmpty => "The message is empty.".to_owned(),
            Self::MessageTooLong { max } => format!("The message is longer than {max} characters."),
            Self::MalformedJson => {
                "The request body is not the JSON this endpoint expects.".to_owned()
            }
            Self::MalformedQuery => {
                "The query string is not what this endpoint expects.".to_owned()
            }
            Self::UnsupportedContentType => {
                "The request needs Content-Type: application/json.".to_owned()
            }
            Self::UnreadableBody => "The request body could not be read.".to_owned(),
            Self::BodyTooLarge => "The request is too large.".to_owned(),
            Self::RateLimited { retry_after_secs } => {
                format!("Too many requests. Try again in {retry_after_secs} seconds.")
            }
            Self::Timeout => "The ashtray took too long to answer. Try again.".to_owned(),
            Self::Unauthorized => "This needs a valid token.".to_owned(),
            Self::NotFound => "Nothing lives at this address.".to_owned(),
            Self::MethodNotAllowed => "This address does not take that method.".to_owned(),
            Self::Refused(status) => {
                format!("{}.", status.canonical_reason().unwrap_or("Request failed"))
            }
            Self::Internal => "Something broke inside the ashtray. Try again.".to_owned(),
        }
    }
}

impl Serialize for Reason {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
