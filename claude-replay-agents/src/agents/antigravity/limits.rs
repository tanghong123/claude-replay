//! **Antigravity's account and usage limits** (#s43): what a status-line payload of the
//! Antigravity CLI (`agy`) says of the subscription and of who is signed in.
//!
//! `agy` runs a status-line command as Claude Code does, from `statusLine` in its own
//! `settings.json`, and hands it a JSON payload. Its `quota` names buckets by pool and window:
//! `gemini-5h` and `gemini-weekly` for Google's models, `3p-5h` and `3p-weekly` for the
//! third-party models it also serves. Each bucket is a `remaining_fraction`, a `reset_time`
//! (RFC 3339) and a `reset_in_seconds`. Only the Gemini pool is read: that is the Google AI
//! subscription's own allowance, the owner's call (2026-10-07); the third-party pool is not
//! wanted.
//!
//! The payload also names the account (`email`, `plan_tier`), which nothing on disk does: the
//! sign-in lives with `agy`, not in a file beside its settings, so the account comes from the
//! payload ([`status_line_account`]) rather than from configuration.
//!
//! `agy` is written in Go and its fields are `omitempty`: a bucket with nothing left carries
//! no `remaining_fraction` at all, and one resetting now no `reset_in_seconds`. A bucket that
//! is THERE without them is exhausted, not unknown; only a bucket that is absent says nothing.

use claude_replay_engine::seam::{
    epoch_secs, AgentAccount, RateLimitWindow, RateLimits, StatusLineHook,
};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The session window's length, in minutes: five hours.
const SESSION_MINUTES: u64 = 5 * 60;
/// The weekly window's length, in minutes: seven days.
const WEEK_MINUTES: u64 = 7 * 24 * 60;

/// The folder `agy` keeps its settings in: `~/.gemini/antigravity-cli`.
fn config_home() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".gemini").join("antigravity-cli")
}

/// Whether `payload` is the Antigravity CLI's.
fn is_antigravity(payload: &Value) -> bool {
    payload.get("product").and_then(Value::as_str) == Some("antigravity")
}

/// The limits in one status-line payload, at `now` (epoch seconds, for a bucket that gives only
/// a relative reset): `gemini-5h` as the primary window and `gemini-weekly` as the secondary
/// one, each used percentage being what is not remaining. `None` when the payload is not
/// Antigravity's or names neither Gemini bucket.
pub fn status_line_limits_at(payload: &Value, now: i64) -> Option<RateLimits> {
    if !is_antigravity(payload) {
        return None;
    }
    let quota = payload.get("quota")?;
    let window = |key: &str, minutes: u64| {
        let w = quota.get(key)?.as_object()?;
        // omitempty: no `remaining_fraction` is none left.
        let remaining = w
            .get("remaining_fraction")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        let resets_at = w
            .get("reset_time")
            .and_then(Value::as_str)
            .and_then(epoch_secs)
            .map(|t| t.round() as i64)
            // omitempty: no `reset_in_seconds` is a reset now.
            .or_else(|| {
                Some(
                    now + w
                        .get("reset_in_seconds")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                )
            });
        Some(RateLimitWindow {
            used_percent: (1.0 - remaining) * 100.0,
            window_minutes: minutes,
            resets_at,
        })
    };
    let primary = window("gemini-5h", SESSION_MINUTES);
    let secondary = window("gemini-weekly", WEEK_MINUTES);
    if primary.is_none() && secondary.is_none() {
        return None;
    }
    Some(RateLimits {
        primary,
        secondary,
        plan_type: plan(payload),
        reached: None,
        observed_at: None,
    })
}

/// [`status_line_limits_at`] now.
pub fn status_line_limits(payload: &Value) -> Option<RateLimits> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    status_line_limits_at(payload, now)
}

/// The plan, as the payload names it (`Google AI Pro`).
fn plan(payload: &Value) -> Option<String> {
    payload
        .get("plan_tier")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
}

/// The account a status-line payload names: its email, which is also its id (the payload
/// carries no other), and its plan. `None` when the payload is not Antigravity's or names no
/// email.
pub fn status_line_account(payload: &Value) -> Option<AgentAccount> {
    if !is_antigravity(payload) {
        return None;
    }
    let email = payload
        .get("email")
        .and_then(Value::as_str)
        .filter(|e| !e.is_empty())?;
    Some(AgentAccount {
        id: email.to_string(),
        email: Some(email.to_string()),
        name: None,
        tier: plan(payload),
        signed_in_at: None,
    })
}

/// Where `agy` takes a status-line command: `statusLine` in its `settings.json`. The entry
/// keeps `agy`'s own status line, with the command's output beneath it
/// (`stack_with_default`), so registering one costs the person nothing `agy` showed.
pub fn status_line_hook() -> StatusLineHook {
    StatusLineHook {
        settings: config_home().join("settings.json"),
        pointer: "/statusLine",
        entry: |command| serde_json::json!({"type": "command", "command": command, "stack_with_default": true}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A payload shaped as `agy` 1.3.1 hands its status line one: synthetic values throughout.
    fn payload(quota: Value) -> Value {
        serde_json::json!({
            "session_id": "11111111-2222-3333-4444-555555555555",
            "product": "antigravity",
            "model": {"id": "Gemini 3.8 Flash (High)", "display_name": "Gemini 3.8 Flash (High)"},
            "plan_tier": "Google AI Pro",
            "email": "someone@example.test",
            "quota": quota,
        })
    }

    const NOW: i64 = 1_790_000_000;

    #[test]
    fn the_gemini_buckets_are_the_two_windows_and_the_third_party_ones_are_not_read() {
        let p = payload(serde_json::json!({
            "3p-5h": {"remaining_fraction": 0.1, "reset_time": "2026-10-08T05:45:00Z", "reset_in_seconds": 100},
            "3p-weekly": {"remaining_fraction": 0.2, "reset_time": "2026-10-15T00:45:00Z", "reset_in_seconds": 200},
            "gemini-5h": {"remaining_fraction": 0.75, "reset_time": "2026-10-08T05:38:25Z", "reset_in_seconds": 17581},
            "gemini-weekly": {"remaining_fraction": 0.9, "reset_time": "2026-10-15T00:38:25Z", "reset_in_seconds": 604381}
        }));
        let limits = status_line_limits_at(&p, NOW).unwrap();
        let primary = limits.primary.unwrap();
        assert!((primary.used_percent - 25.0).abs() < 1e-9, "{primary:?}");
        assert_eq!(primary.window_minutes, 300);
        assert_eq!(primary.resets_at, Some(1_791_437_905)); // 2026-10-08T05:38:25Z
        let secondary = limits.secondary.unwrap();
        assert!(
            (secondary.used_percent - 10.0).abs() < 1e-9,
            "{secondary:?}"
        );
        assert_eq!(secondary.window_minutes, 10_080);
        assert_eq!(secondary.resets_at, Some(1_792_024_705)); // 2026-10-15T00:38:25Z
        assert_eq!(limits.plan_type.as_deref(), Some("Google AI Pro"));
        assert_eq!(limits.observed_at, None);
    }

    /// Go's omitempty: a bucket that is there without `remaining_fraction` has none left, and
    /// without a reset time or `reset_in_seconds` resets now. Only an absent bucket is unknown.
    #[test]
    fn a_bucket_without_a_remaining_fraction_is_used_up() {
        let p = payload(serde_json::json!({
            "gemini-5h": {"reset_in_seconds": 600},
            "gemini-weekly": {"remaining_fraction": 0.5}
        }));
        let limits = status_line_limits_at(&p, NOW).unwrap();
        let primary = limits.primary.unwrap();
        assert_eq!(primary.used_percent, 100.0);
        assert_eq!(primary.resets_at, Some(NOW + 600));
        let secondary = limits.secondary.unwrap();
        assert_eq!(secondary.used_percent, 50.0);
        assert_eq!(secondary.resets_at, Some(NOW));

        let only_weekly = payload(serde_json::json!({"gemini-weekly": {"remaining_fraction": 1}}));
        let limits = status_line_limits_at(&only_weekly, NOW).unwrap();
        assert_eq!(limits.primary, None, "an absent bucket says nothing");
        assert_eq!(limits.secondary.unwrap().used_percent, 0.0);
    }

    #[test]
    fn no_gemini_bucket_or_another_product_is_no_reading() {
        let third_party_only = payload(serde_json::json!({"3p-5h": {"remaining_fraction": 0.5}}));
        assert_eq!(status_line_limits_at(&third_party_only, NOW), None);
        assert_eq!(status_line_limits_at(&payload(Value::Null), NOW), None);
        let mut other = payload(serde_json::json!({"gemini-5h": {"remaining_fraction": 0.5}}));
        other["product"] = Value::from("something-else");
        assert_eq!(status_line_limits_at(&other, NOW), None);
        assert_eq!(status_line_account(&other), None);
    }

    #[test]
    fn the_account_is_the_payloads_email_and_plan() {
        let account = status_line_account(&payload(Value::Null)).unwrap();
        assert_eq!(account.id, "someone@example.test");
        assert_eq!(account.email.as_deref(), Some("someone@example.test"));
        assert_eq!(account.tier.as_deref(), Some("Google AI Pro"));
        let mut signed_out = payload(Value::Null);
        signed_out["email"] = Value::from("");
        assert_eq!(status_line_account(&signed_out), None);
    }

    #[test]
    fn the_hook_keeps_agys_own_status_line() {
        let hook = status_line_hook();
        assert!(hook
            .settings
            .ends_with(".gemini/antigravity-cli/settings.json"));
        assert_eq!(hook.pointer, "/statusLine");
        assert_eq!(
            (hook.entry)("am statusline --agent antigravity"),
            serde_json::json!({
                "type": "command",
                "command": "am statusline --agent antigravity",
                "stack_with_default": true
            })
        );
    }
}
