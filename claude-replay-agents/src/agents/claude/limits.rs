//! **Claude Code's account and usage limits** (#375): who is signed in, and what a
//! status-line payload says of the subscription's limits.
//!
//! Claude Code writes no limits into its transcripts. It hands them to the command it runs
//! for its status line instead: a `rate_limits` object, for subscribers, with a five-hour
//! session window and a seven-day window, each a used percentage and a reset time. The
//! account is named in Claude Code's own configuration file (`oauthAccount`), never with a
//! credential beside it in that object, and this module reads nothing else from the file.

use claude_replay_engine::seam::{AgentAccount, RateLimitWindow, RateLimits, StatusLineHook};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The session window's length, in minutes: five hours.
const SESSION_MINUTES: u64 = 5 * 60;
/// The weekly window's length, in minutes: seven days.
const WEEK_MINUTES: u64 = 7 * 24 * 60;

/// The folder Claude Code keeps its settings in: `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
fn config_home() -> PathBuf {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".claude")
}

/// The file that names the signed-in account: `.claude.json` in `$CLAUDE_CONFIG_DIR`, else in
/// the home directory (beside, not inside, `~/.claude`).
pub fn account_file() -> PathBuf {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir).join(".claude.json");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".claude.json")
}

/// The account `file` names, or `None` when it names none (signed out, an API-key setup) or
/// cannot be read.
pub fn account_in(file: &Path) -> Option<AgentAccount> {
    #[derive(serde::Deserialize)]
    struct Config {
        #[serde(rename = "oauthAccount")]
        oauth_account: Option<Account>,
    }
    #[derive(serde::Deserialize)]
    struct Account {
        #[serde(rename = "accountUuid")]
        account_uuid: Option<String>,
        #[serde(rename = "emailAddress")]
        email_address: Option<String>,
        #[serde(rename = "displayName")]
        display_name: Option<String>,
        #[serde(rename = "userRateLimitTier")]
        user_rate_limit_tier: Option<String>,
        #[serde(rename = "organizationRateLimitTier")]
        organization_rate_limit_tier: Option<String>,
    }
    let text = std::fs::read_to_string(file).ok()?;
    let account = serde_json::from_str::<Config>(&text).ok()?.oauth_account?;
    let id = account.account_uuid.filter(|id| !id.is_empty())?;
    Some(AgentAccount {
        id,
        email: account.email_address.filter(|e| !e.is_empty()),
        name: account.display_name.filter(|n| !n.is_empty()),
        tier: account
            .user_rate_limit_tier
            .or(account.organization_rate_limit_tier)
            .filter(|t| !t.is_empty()),
    })
}

/// The account Claude Code on this machine is signed in as.
pub fn signed_in_account() -> Option<AgentAccount> {
    account_in(&account_file())
}

/// The limits in one status-line payload: `rate_limits.five_hour` as the primary window and
/// `rate_limits.seven_day` as the secondary one. `None` when neither is there (not a
/// subscriber, or no answer from the API yet in this session).
pub fn status_line_limits(payload: &Value) -> Option<RateLimits> {
    let limits = payload.get("rate_limits")?;
    let window = |key: &str, minutes: u64| {
        let w = limits.get(key)?;
        Some(RateLimitWindow {
            used_percent: w.get("used_percentage")?.as_f64()?,
            window_minutes: minutes,
            resets_at: w.get("resets_at").and_then(Value::as_i64),
        })
    };
    let primary = window("five_hour", SESSION_MINUTES);
    let secondary = window("seven_day", WEEK_MINUTES);
    if primary.is_none() && secondary.is_none() {
        return None;
    }
    Some(RateLimits {
        primary,
        secondary,
        plan_type: None,
        reached: None,
    })
}

/// Where Claude Code takes a status-line command: `statusLine` in its `settings.json`.
pub fn status_line_hook() -> StatusLineHook {
    StatusLineHook {
        settings: config_home().join("settings.json"),
        pointer: "/statusLine",
        entry: |command| serde_json::json!({"type": "command", "command": command}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_subscriber_payload_gives_both_windows() {
        let payload = serde_json::json!({
            "session_id": "s",
            "rate_limits": {
                "five_hour": {"used_percentage": 34.5, "resets_at": 1_790_000_000},
                "seven_day": {"used_percentage": 61, "resets_at": 1_790_400_000}
            }
        });
        let limits = status_line_limits(&payload).unwrap();
        assert_eq!(
            limits.primary,
            Some(RateLimitWindow {
                used_percent: 34.5,
                window_minutes: 300,
                resets_at: Some(1_790_000_000)
            })
        );
        assert_eq!(
            limits
                .secondary
                .as_ref()
                .map(|w| (w.used_percent, w.window_minutes)),
            Some((61.0, 10_080))
        );
    }

    #[test]
    fn one_window_alone_is_kept_and_none_at_all_is_none() {
        let weekly = serde_json::json!({"rate_limits": {"seven_day": {"used_percentage": 12, "resets_at": 5}}});
        let limits = status_line_limits(&weekly).unwrap();
        assert!(limits.primary.is_none());
        assert_eq!(limits.secondary.unwrap().resets_at, Some(5));
        for payload in [
            serde_json::json!({"session_id": "s"}),
            serde_json::json!({"rate_limits": {}}),
            serde_json::json!({"rate_limits": {"spend_limit": {"used_percentage": 3}}}),
            serde_json::json!({"rate_limits": {"five_hour": {"resets_at": 5}}}),
        ] {
            assert_eq!(status_line_limits(&payload), None, "{payload}");
        }
    }

    #[test]
    fn the_account_is_read_from_the_config_file_and_nothing_else() {
        let dir = std::env::temp_dir().join(format!("cr-limits-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(".claude.json");
        std::fs::write(
            &file,
            r#"{"numStartups": 3, "oauthAccount": {"accountUuid": "00000000-0000-4000-8000-000000000001",
                "emailAddress": "someone@example.test", "displayName": "Someone",
                "userRateLimitTier": "default_claude_max_20x", "organizationRateLimitTier": "other"},
                "projects": {}}"#,
        )
        .unwrap();
        assert_eq!(
            account_in(&file),
            Some(AgentAccount {
                id: "00000000-0000-4000-8000-000000000001".into(),
                email: Some("someone@example.test".into()),
                name: Some("Someone".into()),
                tier: Some("default_claude_max_20x".into()),
            })
        );
        // signed out, or never signed in: no account
        std::fs::write(&file, r#"{"numStartups": 3}"#).unwrap();
        assert_eq!(account_in(&file), None);
        std::fs::write(
            &file,
            r#"{"oauthAccount": {"emailAddress": "x@example.test"}}"#,
        )
        .unwrap();
        assert_eq!(account_in(&file), None, "no id, no account");
        assert_eq!(account_in(&dir.join("missing.json")), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_hook_is_the_settings_status_line_entry() {
        let hook = status_line_hook();
        assert!(hook.settings.ends_with("settings.json"));
        assert_eq!(hook.pointer, "/statusLine");
        assert_eq!(
            (hook.entry)("tool status"),
            serde_json::json!({"type": "command", "command": "tool status"})
        );
    }
}
