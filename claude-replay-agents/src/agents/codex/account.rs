//! **Codex's signed-in account** (#s14): who Codex on this machine is signed in as, so a
//! consumer can file each rate-limit reading under the account it belongs to.
//!
//! Codex keeps its login in `auth.json` under its home (`$CODEX_HOME`, else `~/.codex`). A
//! ChatGPT sign-in (`auth_mode: "chatgpt"`) carries an OpenID `id_token`, a JWT whose claims
//! name the person and the subscription. This module decodes those claims and nothing else: no
//! signature check, since this is identity for display and never authorisation, and no other
//! token is read into a typed value. Nothing here returns, logs or caches a token.
//!
//! The id is `<chatgpt_account_id>:<chatgpt_user_id>`. In a Team or Business workspace the
//! account id is the WORKSPACE, shared by everyone in it, while limits are per user; and one
//! person can belong to several workspaces. So neither half alone names one person's one
//! subscription, and the pair does.

use claude_replay_engine::seam::AgentAccount;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The claim that holds ChatGPT's account and plan facts.
const OPENAI_AUTH: &str = "https://api.openai.com/auth";

/// The file Codex keeps its login in.
pub fn auth_file() -> PathBuf {
    super::discover::codex_home().join("auth.json")
}

/// The account `file` names, or `None` for an API-key sign-in, a missing or unreadable file, or
/// an id token that cannot be decoded.
pub fn account_in(file: &Path) -> Option<AgentAccount> {
    // Only what is read: the access and refresh tokens are never deserialized into a field, and
    // neither struct derives `Debug`, so nothing here can print a token.
    #[derive(serde::Deserialize)]
    struct Auth {
        auth_mode: Option<String>,
        tokens: Option<Tokens>,
    }
    #[derive(serde::Deserialize)]
    struct Tokens {
        id_token: Option<String>,
        account_id: Option<String>,
    }
    let auth: Auth = serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
    if auth
        .auth_mode
        .as_deref()
        .is_some_and(|mode| mode.to_ascii_lowercase().contains("api"))
    {
        return None;
    }
    let tokens = auth.tokens?;
    let claims = jwt_claims(tokens.id_token.as_deref()?)?;
    let text = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let openai = claims.get(OPENAI_AUTH);
    let field = |key: &str| text(openai.and_then(|a| a.get(key)));
    let user = field("chatgpt_user_id")
        .or_else(|| field("user_id"))
        .or_else(|| text(claims.get("sub")))?;
    let workspace = field("chatgpt_account_id").or(tokens.account_id.filter(|a| !a.is_empty()));
    Some(AgentAccount {
        id: match workspace {
            Some(workspace) => format!("{workspace}:{user}"),
            None => user,
        },
        email: text(claims.get("email")),
        name: text(claims.get("name")),
        tier: field("chatgpt_plan_type"),
        // The login's own time: a token refresh keeps it, a new `codex login` moves it.
        signed_in_at: claims.get("auth_time").and_then(Value::as_i64),
    })
}

/// The account Codex on this machine is signed in as.
pub fn signed_in_account() -> Option<AgentAccount> {
    account_in(&auth_file())
}

/// The claims of a JWT: its middle segment, base64url-decoded, as a JSON object.
fn jwt_claims(token: &str) -> Option<Value> {
    let mut parts = token.split('.');
    let (_header, payload) = (parts.next()?, parts.next()?);
    parts.next()?; // a signature segment, even an empty one
    let claims: Value = serde_json::from_slice(&base64url_decode(payload)?).ok()?;
    claims.is_object().then_some(claims)
}

/// base64url (RFC 4648 §5), padding optional. `None` on a character outside the alphabet.
fn base64url_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3 + 2);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let val = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            b'=' => continue,
            _ => return None,
        } as u32;
        acc = (acc << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64url(data: &[u8]) -> String {
        const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let n = chunk.iter().fold(0u32, |n, b| (n << 8) | *b as u32) << (8 * (3 - chunk.len()));
            for i in 0..=chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            }
        }
        out
    }

    /// An unsigned JWT carrying `claims`, the shape Codex's id token has.
    fn jwt(claims: Value) -> String {
        format!(
            "{}.{}.",
            b64url(br#"{"alg":"none","typ":"JWT"}"#),
            b64url(claims.to_string().as_bytes())
        )
    }

    fn claims(workspace: &str, user: &str) -> Value {
        serde_json::json!({
            "sub": "auth0|abc", "email": "someone@example.test", "name": "Someone",
            "auth_time": 1_790_000_000, "exp": 1_790_003_600,
            OPENAI_AUTH: {"chatgpt_account_id": workspace, "chatgpt_user_id": user,
                "chatgpt_plan_type": "team", "user_id": user}
        })
    }

    const ACCESS: &str = "ACCESS-TOKEN-must-never-appear";
    const REFRESH: &str = "REFRESH-TOKEN-must-never-appear";

    /// A ChatGPT sign-in's `auth.json`, in the measured shape (2026-10-06).
    fn auth_json(id_token: &str) -> String {
        serde_json::json!({
            "auth_mode": "chatgpt", "OPENAI_API_KEY": null,
            "tokens": {"id_token": id_token, "access_token": ACCESS, "refresh_token": REFRESH,
                "account_id": "ws-from-tokens"},
            "last_refresh": "2026-10-05T00:00:00Z"
        })
        .to_string()
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("cr-codex-account-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_chatgpt_sign_in_names_the_person_and_their_subscription() {
        let dir = scratch("named");
        let file = dir.join("auth.json");
        let token = jwt(claims("ws-1", "user-a"));
        std::fs::write(&file, auth_json(&token)).unwrap();
        let account = account_in(&file).unwrap();
        assert_eq!(
            account,
            AgentAccount {
                id: "ws-1:user-a".into(),
                email: Some("someone@example.test".into()),
                name: Some("Someone".into()),
                tier: Some("team".into()),
                signed_in_at: Some(1_790_000_000),
            }
        );
        // No token reaches the result or its Debug form.
        let shown = format!("{account:?}");
        for secret in [ACCESS, REFRESH, token.as_str()] {
            assert!(!shown.contains(secret), "a token leaked: {shown}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_id_tells_users_of_one_workspace_and_one_user_in_two_workspaces_apart() {
        let dir = scratch("ids");
        let file = dir.join("auth.json");
        let id_of = |workspace: &str, user: &str| {
            std::fs::write(&file, auth_json(&jwt(claims(workspace, user)))).unwrap();
            account_in(&file).unwrap().id
        };
        let a_in_1 = id_of("ws-1", "user-a");
        let b_in_1 = id_of("ws-1", "user-b");
        let a_in_2 = id_of("ws-2", "user-a");
        assert_ne!(a_in_1, b_in_1, "two users of one workspace");
        assert_ne!(a_in_1, a_in_2, "one user in two workspaces");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn no_account_for_an_api_key_a_missing_file_or_a_bad_token() {
        let dir = scratch("none");
        let file = dir.join("auth.json");
        std::fs::write(
            &file,
            r#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk-test-not-real","tokens":null}"#,
        )
        .unwrap();
        assert_eq!(
            account_in(&file),
            None,
            "an API-key sign-in names no account"
        );
        // An API-key mode even with a stray id token beside it.
        std::fs::write(
            &file,
            serde_json::json!({"auth_mode": "ApiKey", "tokens": {"id_token": jwt(claims("w", "u"))}})
                .to_string(),
        )
        .unwrap();
        assert_eq!(account_in(&file), None);
        assert_eq!(account_in(&dir.join("missing.json")), None);
        for bad in [
            "not-a-jwt",
            "a.b",
            "a.%%%.c",
            &format!("x.{}.", b64url(b"[1,2]")),
        ] {
            std::fs::write(&file, auth_json(bad)).unwrap();
            assert_eq!(account_in(&file), None, "{bad}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_workspace_falls_back_to_the_token_s_account_id() {
        let dir = scratch("fallback");
        let file = dir.join("auth.json");
        let token = jwt(
            serde_json::json!({"sub": "auth0|abc", OPENAI_AUTH: {"chatgpt_user_id": "user-a"}}),
        );
        std::fs::write(&file, auth_json(&token)).unwrap();
        let account = account_in(&file).unwrap();
        assert_eq!(account.id, "ws-from-tokens:user-a");
        assert_eq!(
            (account.email, account.tier, account.signed_in_at),
            (None, None, None)
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn base64url_reads_the_url_safe_alphabet_with_or_without_padding() {
        for data in [&b""[..], b"f", b"fo", b"foo", b"\xfb\xff\xfe"] {
            let enc = b64url(data);
            assert_eq!(base64url_decode(&enc).as_deref(), Some(data));
            assert_eq!(base64url_decode(&format!("{enc}==")).as_deref(), Some(data));
        }
        assert_eq!(
            base64url_decode("+/"),
            None,
            "the standard alphabet's 62 and 63 are refused"
        );
    }
}
