//! Email signatures. A member writes their own, which goes under what they
//! send from the inbox and under the quotes, invoices and signing requests
//! they send. The workspace's is used when a member has none, and under
//! emails rules send. Plain text, a few lines: mail apps turn addresses
//! and links in it into links themselves.

use core_domain::{Ctx, Permission, UserId};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

use super::{Change, begin, record};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

const MAX_CHARS: usize = 2000;
const MAX_LINES: usize = 20;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Signatures {
    /// Yours, in this workspace.
    pub mine: Option<String>,
    /// The workspace's: used when a member has none, and on emails rules send.
    pub workspace: Option<String>,
    /// You may change the workspace's.
    pub can_edit_workspace: bool,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SignatureBody {
    /// Plain text; empty or absent removes it.
    pub signature: Option<String>,
}

fn clean(s: Option<String>) -> ApiResult<Option<String>> {
    let Some(s) = s else { return Ok(None) };
    let s = s.replace("\r\n", "\n");
    let lines: Vec<&str> = s.lines().map(str::trim_end).collect();
    let s = lines.join("\n").trim_matches('\n').to_owned();
    if s.trim().is_empty() {
        return Ok(None);
    }
    if s.chars().count() > MAX_CHARS {
        return Err(ApiError::invalid(
            "signature",
            format!("must be at most {MAX_CHARS} characters"),
        ));
    }
    if s.lines().count() > MAX_LINES {
        return Err(ApiError::invalid(
            "signature",
            format!("must be at most {MAX_LINES} lines"),
        ));
    }
    Ok(Some(s))
}

fn member(ctx: &Ctx) -> ApiResult<UserId> {
    ctx.user_id().ok_or_else(|| {
        ApiError::forbidden(
            "member_required",
            "a signature belongs to a member, signed in",
        )
    })
}

pub async fn get(state: &AppState, ctx: &Ctx) -> ApiResult<Signatures> {
    ctx.authorize(Permission::WorkspaceRead)?;
    let mut tx = begin(state, ctx).await?;
    let mine = match ctx.user_id() {
        Some(u) => core_db::signatures::member(&mut tx, u).await?,
        None => None,
    };
    let workspace = core_db::signatures::workspace(&mut tx).await?;
    tx.commit().await?;
    Ok(Signatures {
        mine,
        workspace,
        can_edit_workspace: ctx.permissions.contains(&Permission::WorkspaceUpdate),
    })
}

/// Sets your own signature in this workspace.
pub async fn set_mine(state: &AppState, ctx: &Ctx, b: SignatureBody) -> ApiResult<Signatures> {
    ctx.authorize(Permission::WorkspaceRead)?;
    let user = member(ctx)?;
    let signature = clean(b.signature)?;
    let mut tx = begin(state, ctx).await?;
    core_db::signatures::set_member(&mut tx, user, signature.as_deref()).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "member.signature_changed",
            resource: "member",
            object_id: Some(user.0),
            before: None,
            after: Some(json!({ "has_signature": signature.is_some() })),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    get(state, ctx).await
}

/// Sets the workspace's signature.
pub async fn set_workspace(state: &AppState, ctx: &Ctx, b: SignatureBody) -> ApiResult<Signatures> {
    ctx.authorize(Permission::WorkspaceUpdate)?;
    let signature = clean(b.signature)?;
    let mut tx = begin(state, ctx).await?;
    core_db::signatures::set_workspace(&mut tx, signature.as_deref()).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "workspace.signature_changed",
            resource: "workspace",
            object_id: Some(ctx.workspace_id.0),
            before: None,
            after: Some(json!({ "signature": signature })),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    get(state, ctx).await
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Puts `signature` under a message, in its plain text (after the usual
/// "-- " line) and its HTML (inside the body, for a whole document).
pub(crate) fn append(text: &mut String, html: &mut String, signature: &str) {
    let trimmed = text.trim_end().len();
    text.truncate(trimmed);
    text.push_str("\n\n-- \n");
    text.push_str(signature);
    let block = format!(
        "<div style=\"margin-top:16px;color:#555555\">{}</div>",
        escape(signature).replace('\n', "<br>")
    );
    match html.to_ascii_lowercase().rfind("</body>") {
        Some(at) => html.insert_str(at, &block),
        None => html.push_str(&block),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_are_tidied_and_limited() {
        assert_eq!(clean(None).expect("clean"), None);
        assert_eq!(clean(Some(" \n \n".into())).expect("clean"), None);
        assert_eq!(
            clean(Some("\r\nSam Lee  \r\nThe Agency\r\n\r\n".into())).expect("clean"),
            Some("Sam Lee\nThe Agency".into())
        );
        assert!(clean(Some("x".repeat(2001))).is_err());
        assert!(clean(Some("x\n".repeat(21))).is_err());
    }

    #[test]
    fn a_signature_goes_under_the_text_and_inside_the_html() {
        let mut text = "Hello\n".to_owned();
        let mut html = "<p>Hello</p>".to_owned();
        append(&mut text, &mut html, "Sam <Lee>\n07700 900123");
        assert_eq!(text, "Hello\n\n-- \nSam <Lee>\n07700 900123");
        assert!(
            html.ends_with("Sam &lt;Lee&gt;<br>07700 900123</div>"),
            "{html}"
        );

        let mut text = String::new();
        let mut html = "<html><BODY><p>Hi</p></BODY></html>".to_owned();
        append(&mut text, &mut html, "Sam");
        assert!(html.ends_with("Sam</div></BODY></html>"), "{html}");
    }
}
