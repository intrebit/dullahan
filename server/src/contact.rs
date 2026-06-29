use crate::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ContactPayload {
    pub email: String,
    pub name: String,
    pub message: String,
}

pub async fn submit(
    State(state): State<AppState>,
    Json(payload): Json<ContactPayload>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    let email = payload.email.trim();
    let name = payload.name.trim();
    let message = payload.message.trim();

    if !email_looks_valid(email) {
        return bad_request("Please enter a valid email");
    }
    if name.chars().count() > 80 {
        return bad_request("Name can't have more than 80 characters");
    }
    let msg_chars = message.chars().count();
    if !(10..=2000).contains(&msg_chars) {
        return bad_request("Message must be between 10 and 2000 characters");
    }

    let mailer = state
        .mailer
        .as_ref()
        .ok_or_else(|| service_unavailable("email transport not configured"))?;
    let to = state
        .config
        .contact_to
        .as_deref()
        .ok_or_else(|| service_unavailable("contact recipient not configured"))?;

    if let Err(err) = mailer.send_contact(to, name, email, message).await {
        tracing::error!(error = %err, "contact email send failed");
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorBody {
                message: "Failed to send message. Please try again later.".into(),
            }),
        ));
    }

    Ok(StatusCode::CREATED)
}

#[derive(serde::Serialize)]
pub struct ErrorBody {
    pub message: String,
}

fn bad_request(msg: &str) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    Err((
        StatusCode::BAD_REQUEST,
        Json(ErrorBody {
            message: msg.into(),
        }),
    ))
}

fn service_unavailable(msg: &str) -> (StatusCode, Json<ErrorBody>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ErrorBody {
            message: msg.into(),
        }),
    )
}

fn email_looks_valid(s: &str) -> bool {
    if s.is_empty() || s.len() > 254 || s.contains(char::is_whitespace) {
        return false;
    }
    let mut parts = s.splitn(2, '@');
    let local = parts.next().unwrap_or("");
    let domain = parts.next().unwrap_or("");
    if local.is_empty() || domain.is_empty() {
        return false;
    }
    if !domain.contains('.') || domain.starts_with('.') || domain.ends_with('.') {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::email_looks_valid;

    #[test]
    fn accepts_normal() {
        assert!(email_looks_valid("a@b.co"));
        assert!(email_looks_valid("first.last+tag@example.com"));
    }

    #[test]
    fn rejects_garbage() {
        assert!(!email_looks_valid(""));
        assert!(!email_looks_valid("noatsign"));
        assert!(!email_looks_valid("@nolocal.com"));
        assert!(!email_looks_valid("nolocal@"));
        assert!(!email_looks_valid("a@b"));
        assert!(!email_looks_valid("a @b.co"));
        assert!(!email_looks_valid("a@.b"));
        assert!(!email_looks_valid("a@b."));
    }
}
