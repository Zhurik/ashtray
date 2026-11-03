//! Push notifications about new entries, through [ntfy](https://ntfy.sh).
//!
//! A notification is sent in the background once the entry is stored: the
//! visitor's request never waits on ntfy, and ntfy being down never fails a
//! write. A notification that cannot be delivered is logged and dropped, and
//! one still in flight when the process stops is lost.

use std::time::Duration;

use reqwest::header::{AUTHORIZATION, HeaderValue};
use url::Url;

use crate::config::NtfyConfig;
use crate::models::Entry;

/// How long one notification gets before it is given up on.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Sends one ntfy message per new entry to the configured topic.
#[derive(Debug, Clone)]
pub struct Notifier {
    client: reqwest::Client,
    topic_url: Url,
    authorization: Option<HeaderValue>,
}

/// One ntfy message, before it is turned into a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub title: String,
    pub tags: &'static str,
    pub body: String,
}

impl Message {
    pub fn for_entry(entry: &Entry) -> Self {
        let who = entry.author.as_deref().unwrap_or("someone anonymous");
        let (title, tags) = if entry.waitlisted {
            (
                format!("New guestbook entry from {who}, waiting for approval"),
                "speech_balloon,hourglass",
            )
        } else {
            (format!("New guestbook entry from {who}"), "speech_balloon")
        };

        let mut body = entry.body.clone();
        if let Some(url) = &entry.url {
            body.push_str("\n\n");
            body.push_str(url);
        }
        if entry.waitlisted {
            body.push_str(&format!(
                "\n\nUPDATE entries SET waitlisted = 0 WHERE id = {};",
                entry.id
            ));
        }

        Self { title, tags, body }
    }
}

impl Notifier {
    /// `None` when no topic is configured.
    pub fn new(config: &NtfyConfig) -> Result<Option<Self>, reqwest::Error> {
        let Some(topic_url) = &config.url else {
            return Ok(None);
        };
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .user_agent(concat!("ashtray/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let authorization = config.token.as_ref().map(|token| {
            let mut value = HeaderValue::from_str(&format!("Bearer {}", token.expose()))
                .expect("the token was checked to be a valid header value");
            value.set_sensitive(true);
            value
        });

        Ok(Some(Self {
            client,
            topic_url: topic_url.clone(),
            authorization,
        }))
    }

    /// Sends the notification for `entry` without waiting for it.
    pub fn entry_created(&self, entry: &Entry) {
        let notifier = self.clone();
        let id = entry.id;
        let message = Message::for_entry(entry);
        tokio::spawn(async move {
            match notifier.send(&message).await {
                Ok(()) => tracing::debug!(entry = id, "ntfy notified"),
                Err(cause) => tracing::warn!(entry = id, %cause, "cannot notify ntfy"),
            }
        });
    }

    /// Publishes `message` and waits for ntfy to accept it.
    ///
    /// Title and tags go in the query string rather than in headers: ntfy
    /// reads both, and only the query string carries a non-ASCII name as is.
    pub async fn send(&self, message: &Message) -> Result<(), reqwest::Error> {
        let mut url = self.topic_url.clone();
        url.query_pairs_mut()
            .append_pair("title", &message.title)
            .append_pair("tags", message.tags);

        let mut request = self.client.post(url).body(message.body.clone());
        if let Some(authorization) = &self.authorization {
            request = request.header(AUTHORIZATION, authorization.clone());
        }
        request.send().await?.error_for_status()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(author: Option<&str>, url: Option<&str>, waitlisted: bool) -> Entry {
        Entry {
            id: 7,
            author: author.map(str::to_owned),
            url: url.map(str::to_owned),
            body: "Hello from the webring.".to_owned(),
            created_at: "2026-11-02T12:00:00Z".to_owned(),
            waitlisted,
        }
    }

    #[test]
    fn a_shown_entry_is_announced_with_its_author_and_link() {
        let message = Message::for_entry(&entry(Some("marina"), Some("https://marina.dev"), false));

        assert_eq!(message.title, "New guestbook entry from marina");
        assert_eq!(message.tags, "speech_balloon");
        assert_eq!(
            message.body,
            "Hello from the webring.\n\nhttps://marina.dev"
        );
    }

    #[test]
    fn a_waitlisted_entry_says_how_to_let_it_through() {
        let message = Message::for_entry(&entry(None, None, true));

        assert_eq!(
            message.title,
            "New guestbook entry from someone anonymous, waiting for approval"
        );
        assert!(message.tags.contains("hourglass"));
        assert!(
            message
                .body
                .ends_with("UPDATE entries SET waitlisted = 0 WHERE id = 7;")
        );
    }
}
