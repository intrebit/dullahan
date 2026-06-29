use crate::config::EmailConfig;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::json;
use thiserror::Error;
use tokio::time::{error::Elapsed, timeout};

const RESEND_API_BASE: &str = "https://api.resend.com";

#[derive(Debug, Error)]
pub enum EmailError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Resend API {status}: {message}")]
    Api { status: u16, message: String },
    #[error("email send timed out")]
    Timeout(#[from] Elapsed),
}

#[derive(Clone, Debug)]
pub struct Mailer {
    http: reqwest::Client,
    config: EmailConfig,
}

impl Mailer {
    pub fn new(config: EmailConfig) -> Self {
        Self {
            http: reqwest::Client::new(),
            config,
        }
    }

    fn build_from_header(&self, display_name: Option<&str>) -> String {
        let name = display_name
            .map(sanitize_display_name)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| sanitize_display_name(&self.config.from_name));
        if name.is_empty() {
            self.config.from.clone()
        } else {
            format!("{name} <{}>", self.config.from)
        }
    }

    pub async fn send_html(
        &self,
        to: &str,
        subject: &str,
        html: &str,
        from_display: Option<&str>,
        reply_to: Option<&str>,
    ) -> Result<(), EmailError> {
        let mut payload = json!({
            "from": self.build_from_header(from_display),
            "to": [to],
            "subject": subject,
            "html": html,
        });
        attach_reply_to(&mut payload, reply_to);
        self.dispatch(payload).await
    }

    pub async fn send_with_attachment(
        &self,
        to: &str,
        subject: &str,
        body_text: &str,
        attachment: (&str, &[u8]),
        from_display: Option<&str>,
        reply_to: Option<&str>,
    ) -> Result<(), EmailError> {
        let (filename, bytes) = attachment;
        let mut payload = json!({
            "from": self.build_from_header(from_display),
            "to": [to],
            "subject": subject,
            "text": body_text,
            "attachments": [{
                "filename": filename,
                "content": BASE64.encode(bytes),
            }],
        });
        attach_reply_to(&mut payload, reply_to);
        self.dispatch(payload).await
    }

    pub async fn send_contact(
        &self,
        to: &str,
        name: &str,
        reply_email: &str,
        message: &str,
    ) -> Result<(), EmailError> {
        let payload = json!({
            "from": self.build_from_header(None),
            "to": [to],
            "reply_to": reply_email,
            "subject": format!("New contact form submission from {name}"),
            "text": message,
        });
        self.dispatch(payload).await
    }

    pub async fn verify_api_key(&self) -> Result<bool, EmailError> {
        let req = self
            .http
            .get(format!("{RESEND_API_BASE}/domains"))
            .bearer_auth(&self.config.resend_api_key);
        let resp = timeout(self.config.timeout, req.send()).await??;
        Ok(resp.status().is_success())
    }

    async fn dispatch(&self, payload: serde_json::Value) -> Result<(), EmailError> {
        let req = self
            .http
            .post(format!("{RESEND_API_BASE}/emails"))
            .bearer_auth(&self.config.resend_api_key)
            .json(&payload);
        let resp = timeout(self.config.timeout, req.send()).await??;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let status_code = status.as_u16();
        let body = resp.text().await.unwrap_or_default();
        let message = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
            .unwrap_or(body);
        Err(EmailError::Api {
            status: status_code,
            message,
        })
    }
}

fn attach_reply_to(payload: &mut serde_json::Value, reply_to: Option<&str>) {
    let Some(addr) = reply_to else { return };
    let trimmed = addr.trim();
    if trimmed.is_empty() {
        return;
    }
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("reply_to".to_string(), json!(trimmed));
    }
}

fn sanitize_display_name(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '<' | '>' | '"' | '\r' | '\n' | ','))
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::sanitize_display_name;

    #[test]
    fn sanitize_strips_brackets_quotes_commas_crlf() {
        assert_eq!(sanitize_display_name("Foo Co."), "Foo Co.");
        assert_eq!(sanitize_display_name("Foo \"Co\""), "Foo Co");
        assert_eq!(sanitize_display_name("Foo, Inc <evil>"), "Foo Inc evil");
        assert_eq!(
            sanitize_display_name("legit\r\nBcc: leak@x.com"),
            "legitBcc: leak@x.com"
        );
        assert_eq!(sanitize_display_name(""), "");
        assert_eq!(sanitize_display_name("   spaces   "), "spaces");
    }
}
