//! Codex live rate limits from the ChatGPT backend.
//!
//! The Codex adapter already lifts `rate_limits` out of `~/.codex/sessions`, but
//! those lines only appear while a turn is running, so the panel can show a window
//! that has already reset. This probe asks the backend directly; the log stays the
//! authority for spend and this is the freshness backstop.
//!
//! Request and field names are `nicezic/lean-quota-monitor:src/providers/openai.ts:142-150`
//! (`docs/provider-acquisition-spec.md:100-160` documents the same call), cross-checked
//! against the live answer captured in the ignored test below:
//!
//! ```text
//! GET https://chatgpt.com/backend-api/wham/usage
//! Authorization: Bearer <~/.codex/auth.json tokens.access_token>
//! chatgpt-account-id: <~/.codex/auth.json tokens.account_id>   # when present
//! ```
//!
//! Nothing here logs in or refreshes the token: an expired `access_token` answers
//! 401 and yields no row.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use usage_core::QuotaSample;

use crate::http::get_json;
use crate::QuotaProbe;

const ENDPOINT: &str = "https://chatgpt.com/backend-api/wham/usage";
const RESET_CREDITS_ENDPOINT: &str = "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits";
const USER_AGENT: &str = concat!("kimo-token/", env!("CARGO_PKG_VERSION"));
const RESET_CREDITS_TIMEOUT: Duration = Duration::from_secs(4);
const RESET_CREDITS_DIR: &str = "codex-reset-credits";
const RESET_CREDITS_HEADERS: [(&str, &str); 4] = [
    ("Accept", "application/json"),
    ("User-Agent", USER_AGENT),
    ("OpenAI-Beta", "codex-1"),
    ("originator", "Codex Desktop"),
];

#[derive(Deserialize, Serialize)]
pub struct CodexResetCredit {
    pub granted_at_ms: i64,
    pub expires_at_ms: Option<i64>,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CodexResetCredits {
    SignInRequired { cards: Option<Vec<CodexResetCredit>> },
    Unavailable { cards: Option<Vec<CodexResetCredit>> },
    Ready { cards: Vec<CodexResetCredit> },
}

enum ResetCreditsError {
    SignInRequired,
    Unavailable,
}

#[derive(Deserialize)]
struct ResetCreditIdentity {
    sub: String,
}

#[derive(Deserialize)]
struct ResetCreditsResponse {
    credits: Vec<ResetCreditResponse>,
    #[serde(rename = "available_count")]
    _available_count: u64,
}

#[derive(Deserialize)]
struct ResetCreditResponse {
    id: String,
    status: String,
    granted_at: String,
    expires_at: Option<String>,
}

// 手动刷新时重新读取当前登录账号并查询最新卡片。
pub fn codex_reset_credits() -> CodexResetCredits {
    let Some((token, account)) = credential() else {
        return CodexResetCredits::SignInRequired { cards: None };
    };
    let path = reset_credits_cache_path(&token, account.as_deref());
    // 仅成功响应替换磁盘快照，成功返回零张也算一次更新。
    match request_reset_credits(&token, account.as_deref()) {
        Ok(cards) => {
            if let Some(path) = path.as_deref() {
                let _ = save_reset_credits(path, &cards);
            }
            CodexResetCredits::Ready { cards }
        }
        Err(error) => {
            let cards = path.as_deref().and_then(read_reset_credits);
            match error {
                ResetCreditsError::SignInRequired => CodexResetCredits::SignInRequired { cards },
                ResetCreditsError::Unavailable => CodexResetCredits::Unavailable { cards },
            }
        }
    }
}

// 仅从磁盘恢复当前账号的快照，启动应用时无需联网。
pub fn cached_codex_reset_credits() -> Option<Vec<CodexResetCredit>> {
    let (token, account) = credential()?;
    let path = reset_credits_cache_path(&token, account.as_deref())?;
    read_reset_credits(&path)
}

// 用 OAuth 主体和账号区分快照，令牌轮换时仍能恢复同一账号的数据。
fn reset_credits_cache_key(token: &str, account: Option<&str>) -> String {
    let subject = token.split('.').nth(1)
        .and_then(|segment| URL_SAFE_NO_PAD.decode(segment).ok())
        .and_then(|claims| serde_json::from_slice::<ResetCreditIdentity>(&claims).ok())
        .filter(|claims| !claims.sub.is_empty());
    let mut hash = Sha256::new();
    let identity = match subject.as_ref() {
        Some(claims) => {
            hash.update(b"subject");
            claims.sub.as_str()
        }
        None => {
            hash.update(b"token");
            token
        }
    };
    hash.update(identity.len().to_le_bytes());
    hash.update(identity);
    hash.update(account.unwrap_or_default());
    format!("{:x}", hash.finalize())
}

// 文件名仅包含账号指纹，快照仅包含卡片显示所需的日期。
fn reset_credits_cache_path(token: &str, account: Option<&str>) -> Option<PathBuf> {
    let key = reset_credits_cache_key(token, account);
    Some(usage_core::paths::data_dir()?.join(RESET_CREDITS_DIR).join(format!("{key}.json")))
}

// 读取完整快照，忽略缺失或损坏的缓存。
fn read_reset_credits(path: &Path) -> Option<Vec<CodexResetCredit>> {
    serde_json::from_reader(fs::File::open(path).ok()?).ok()
}

// 在同一目录写入临时文件并原子替换，避免部分写入损坏旧快照。
fn save_reset_credits(path: &Path, cards: &[CodexResetCredit]) -> std::io::Result<()> {
    let dir = path.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    fs::create_dir_all(dir)?;
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    serde_json::to_writer(file.as_file_mut(), cards)?;
    file.persist(path)?;
    Ok(())
}

// 将读取结果统一转成面板状态，避免向前端传输账户信息和响应原文。
fn request_reset_credits(token: &str, account: Option<&str>) -> Result<Vec<CodexResetCredit>, ResetCreditsError> {
    let payload = fetch_reset_credit_payload(token, account)?;
    available_reset_credits(payload, usage_core::report::now_ms()).ok_or(ResetCreditsError::Unavailable)
}

// macOS 菜单栏进程读取系统代理和证书信任设置，禁止重定向携带凭据。
#[cfg(target_os = "macos")]
fn fetch_reset_credit_payload(
    token: &str,
    account: Option<&str>,
) -> Result<ResetCreditsResponse, ResetCreditsError> {
    use ResetCreditsError::{SignInRequired, Unavailable};
    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .timeout(RESET_CREDITS_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| Unavailable)?;
    let mut request = client.get(RESET_CREDITS_ENDPOINT).bearer_auth(token);
    for (name, value) in RESET_CREDITS_HEADERS {
        request = request.header(name, value);
    }
    if let Some(id) = account {
        request = request.header("ChatGPT-Account-ID", id);
    }
    let response = request.send().map_err(|_| Unavailable)?;
    match response.status().as_u16() {
        200 => response.json().map_err(|_| Unavailable),
        401 => Err(SignInRequired),
        _ => Err(Unavailable),
    }
}

// 其他平台沿用现有配额客户端的 TLS 设置，仅允许固定的 HTTPS 请求。
#[cfg(not(target_os = "macos"))]
fn fetch_reset_credit_payload(
    token: &str,
    account: Option<&str>,
) -> Result<ResetCreditsResponse, ResetCreditsError> {
    use ResetCreditsError::{SignInRequired, Unavailable};
    let agent = crate::http::agent_builder()
        .https_only(true)
        .redirects(0)
        .timeout(RESET_CREDITS_TIMEOUT)
        .timeout_connect(RESET_CREDITS_TIMEOUT)
        .build();
    let mut request = agent
        .get(RESET_CREDITS_ENDPOINT)
        .set("Authorization", &format!("Bearer {token}"));
    for (name, value) in RESET_CREDITS_HEADERS {
        request = request.set(name, value);
    }
    if let Some(id) = account {
        request = request.set("ChatGPT-Account-ID", id);
    }
    match request.call() {
        Ok(response) if response.status() == 200 => response.into_json().map_err(|_| Unavailable),
        Err(ureq::Error::Status(401, _)) => Err(SignInRequired),
        _ => Err(Unavailable),
    }
}

// 保留可用且未过期的卡，已知到期时间最早的排在前面。
fn available_reset_credits(payload: ResetCreditsResponse, now_ms: i64) -> Option<Vec<CodexResetCredit>> {
    let mut ids = HashSet::new();
    let mut cards = Vec::new();
    for row in payload.credits.into_iter().filter(|row| row.status == "available") {
        let expires_at_ms = match row.expires_at.as_deref() {
            Some(value) => Some(reset_credit_date(value)?),
            None => None,
        };
        if expires_at_ms.is_some_and(|expires| expires <= now_ms) || !ids.insert(row.id) {
            continue;
        }
        cards.push(CodexResetCredit {
            granted_at_ms: reset_credit_date(&row.granted_at)?,
            expires_at_ms,
        });
    }
    cards.sort_by_key(|card| (card.expires_at_ms.unwrap_or(i64::MAX), card.granted_at_ms));
    Some(cards)
}

// API 返回带时区的 ISO 8601 字符串，统一转换为毫秒时间戳。
fn reset_credit_date(raw: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|date| date.timestamp_millis())
}

pub struct CodexQuota;

impl QuotaProbe for CodexQuota {
    fn tool(&self) -> &'static str {
        "codex"
    }

    fn fetch(&self) -> Vec<QuotaSample> {
        live().unwrap_or_default()
    }
}

fn live() -> Option<Vec<QuotaSample>> {
    let (token, account_id) = credential()?;
    let mut headers = vec![
        ("authorization", format!("Bearer {token}")),
        ("accept", "application/json".into()),
        ("user-agent", USER_AGENT.into()),
    ];
    if let Some(id) = account_id.as_ref() {
        headers.push(("chatgpt-account-id", id.clone()));
    }
    let borrowed: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let body = get_json(ENDPOINT, &borrowed)?;
    let identity = account_id.as_deref().unwrap_or(token.as_str());
    let mut samples = samples_from(&body);
    for sample in &mut samples {
        sample.id = Some(super::account_window_id(identity, sample.window_minutes));
    }
    Some(samples)
}

/// `reset_at` is unix **seconds**, `used_percent` is already 0-100, and the window
/// length arrives as `limit_window_seconds` (18 000 = 5 h, 604 800 = 1 week).
/// Unknown bucket names are kept: the container is scanned rather than hard-coded to
/// `primary_window`/`secondary_window`, because which slot holds which window is not
/// stable (`nicezic/lean-quota-monitor:docs/provider-acquisition-spec.md:186-190`).
pub(crate) fn samples_from(body: &Value) -> Vec<QuotaSample> {
    // `rate_limit` is what the endpoint sends today; `rate_limits` is the name the
    // app-server RPC and older notes use, so both are accepted.
    let group = body
        .get("rate_limit")
        .or_else(|| body.get("rate_limits"))
        .and_then(Value::as_object)
        .or_else(|| body.as_object());
    let Some(group) = group else { return Vec::new() };

    let mut out = Vec::new();
    for (key, window) in group {
        let Some(window) = window.as_object() else { continue };
        let Some(used_percent) = window.get("used_percent").and_then(Value::as_f64) else { continue };
        let Some(seconds) = window.get("limit_window_seconds").and_then(Value::as_i64) else { continue };
        if seconds <= 0 {
            continue;
        }
        // Codex currently exposes the five-hour and weekly token windows. A
        // stale/extra 30-day bucket is not a Codex window and must not be
        // presented as one just because the wire payload contains a duration.
        let minutes = seconds / 60;
        if minutes >= 43_200 {
            continue;
        }
        out.push(QuotaSample {
            used_percent: used_percent.clamp(0.0, 100.0),
            window_minutes: minutes,
            resets_at_ms: unix_ms(window.get("reset_at").unwrap_or(&Value::Null)),
            label: Some(label_for(key, minutes)),
            id: None,
        });
    }
    out.sort_by_key(|s| s.window_minutes);
    out
}

/// Seconds or milliseconds, whichever arrived.
fn unix_ms(value: &Value) -> i64 {
    match value.as_i64().or_else(|| value.as_f64().map(|f| f as i64)) {
        Some(n) if n > 0 => {
            if n < 10_000_000_000 {
                n * 1000
            } else {
                n
            }
        }
        _ => 0,
    }
}

fn label_for(key: &str, minutes: i64) -> String {
    match minutes {
        300 => "5 小时".into(),
        1_440 => "天".into(),
        10_080 => "周".into(),
        43_200 => "月".into(),
        other => format!("{key} · {other} 分钟"),
    }
}

/// `$CODEX_HOME/auth.json` else `~/.codex/auth.json`; `tokens` holds the OAuth
/// envelope the CLI wrote at `codex login`
/// (`nicezic/lean-quota-monitor:src/providers/openai.ts:128-140`).
fn credential() -> Option<(String, Option<String>)> {
    let root = std::env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".codex")))?;
    token_in(&std::fs::read_to_string(root.join("auth.json")).ok()?)
}

pub(crate) fn token_in(text: &str) -> Option<(String, Option<String>)> {
    let body: Value = serde_json::from_str(text).ok()?;
    let tokens = body.get("tokens")?.as_object()?;
    let token = tokens.get("access_token")?.as_str()?.trim();
    if token.is_empty() {
        return None;
    }
    let account = tokens.get("account_id").and_then(Value::as_str).map(str::trim).filter(|a| !a.is_empty()).map(str::to_string);
    Some((token.to_string(), account))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 同一账号换令牌后可恢复，切换用户或工作区会读取另一份快照。
    #[test]
    fn reset_credit_cache_keys_are_stable_and_account_scoped() {
        let subject_a = URL_SAFE_NO_PAD.encode(br#"{"sub":"user-a"}"#);
        let subject_b = URL_SAFE_NO_PAD.encode(br#"{"sub":"user-b"}"#);
        let old = format!("header.{subject_a}.old");
        let rotated = format!("header.{subject_a}.new");
        let other = format!("header.{subject_b}.new");
        let key = reset_credits_cache_key(&old, Some("workspace"));
        assert_eq!(key, reset_credits_cache_key(&rotated, Some("workspace")));
        assert_ne!(key, reset_credits_cache_key(&other, Some("workspace")));
        assert_ne!(key, reset_credits_cache_key(&old, Some("other-workspace")));
        assert_ne!(reset_credits_cache_key("opaque-a", None), reset_credits_cache_key("opaque-b", None));
    }

    // 快照能跨读取保留，零张卡的成功结果可替换旧列表。
    #[test]
    fn reset_credit_snapshot_round_trips_and_accepts_empty_results() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot").join("credits.json");
        assert!(read_reset_credits(&path).is_none());
        let card = CodexResetCredit { granted_at_ms: 1000, expires_at_ms: Some(2000) };
        save_reset_credits(&path, &[card]).unwrap();
        let restored = read_reset_credits(&path).unwrap();
        assert_eq!(restored[0].expires_at_ms, Some(2000));
        save_reset_credits(&path, &[]).unwrap();
        assert!(read_reset_credits(&path).unwrap().is_empty());
    }

    // 覆盖到期边界、重复记录、无到期时间及两种时间格式。
    #[test]
    fn reset_credits_show_only_available_cards_in_expiry_order() {
        let payload = serde_json::from_str(r#"{
            "available_count": 5,
            "credits": [
                {"id":"later","status":"available","granted_at":"2026-11-01T01:30:00Z","expires_at":"2026-11-18T13:30:00Z"},
                {"id":"open","status":"available","granted_at":"2026-11-02T01:30:00Z","expires_at":null},
                {"id":"used","status":"redeemed","granted_at":"2026-11-01T01:30:00Z","expires_at":"2026-11-12T13:30:00Z"},
                {"id":"expired","status":"available","granted_at":"2026-11-01T01:30:00Z","expires_at":"2026-11-11T12:00:00Z"},
                {"id":"soon","status":"available","granted_at":"2026-11-03T01:30:00Z","expires_at":"2026-11-11T21:30:00.125+08:00"},
                {"id":"soon","status":"available","granted_at":"2026-11-03T01:30:00Z","expires_at":"2026-11-11T21:30:00.125+08:00"}
            ]
        }"#).unwrap();
        let now = reset_credit_date("2026-11-11T12:00:00Z").unwrap();
        let cards = available_reset_credits(payload, now).unwrap();
        assert_eq!(cards.len(), 3);
        assert_eq!(cards[0].expires_at_ms, reset_credit_date("2026-11-11T13:30:00.125Z"));
        assert_eq!(cards[1].expires_at_ms, reset_credit_date("2026-11-18T13:30:00Z"));
        assert_eq!(cards[2].expires_at_ms, None);
        assert_eq!(cards[2].granted_at_ms, reset_credit_date("2026-11-02T01:30:00Z").unwrap());
    }

    /// Captured 200 body from `GET /backend-api/wham/usage` (account identifiers
    /// faked, every field name and unit is the real thing).
    const OK_SAMPLE: &str = r#"{
      "user_id": "user-FAKE0000",
      "account_id": "00000000-0000-0000-0000-000000000000",
      "email": "someone@example.com",
      "plan_type": "plus",
      "rate_limit": {
        "allowed": true,
        "limit_reached": false,
        "primary_window": {
          "used_percent": 21, "limit_window_seconds": 18000,
          "reset_after_seconds": 11655, "reset_at": 1790188292
        },
        "secondary_window": {
          "used_percent": 22, "limit_window_seconds": 604800,
          "reset_after_seconds": 562367, "reset_at": 1790739004
        }
      },
      "code_review_rate_limit": null,
      "additional_rate_limits": null,
      "credits": {"has_credits": false, "unlimited": false, "balance": "0"},
      "rate_limit_reset_credits": {"available_count": 1}
    }"#;

    #[test]
    fn both_windows_come_back_shortest_first() {
        let body: Value = serde_json::from_str(OK_SAMPLE).unwrap();
        let s = samples_from(&body);
        assert_eq!(
            s.iter().map(|x| (x.window_minutes, x.used_percent)).collect::<Vec<_>>(),
            vec![(300, 21.0), (10_080, 22.0)],
            "primary/secondary are not assumed to be 5h/week: the duration decides"
        );
        assert_eq!(s[0].label.as_deref(), Some("5 小时"));
        assert_eq!(s[1].label.as_deref(), Some("周"));
    }

    #[test]
    fn reset_at_is_unix_seconds_promoted_to_milliseconds() {
        let body: Value = serde_json::from_str(OK_SAMPLE).unwrap();
        let s = samples_from(&body);
        assert_eq!(s[0].resets_at_ms, 1_790_188_292_000, "×1000, not passed through");
        assert_eq!(s[1].resets_at_ms, 1_790_739_004_000);
    }

    #[test]
    fn null_and_unknown_buckets_are_skipped_not_faked() {
        let body: Value = serde_json::from_str(OK_SAMPLE).unwrap();
        assert_eq!(samples_from(&body).len(), 2, "the sibling null rate limits add nothing");
        let odd: Value = serde_json::from_str(
            r#"{"rate_limit":{"primary_window":{"used_percent":5,"limit_window_seconds":900,"reset_at":1790188292},
                 "allowed":true,"limit_reached":false,"whatever":{"used_percent":9}}}"#,
        )
        .unwrap();
        let s = samples_from(&odd);
        assert_eq!(s.len(), 1, "a window with no duration cannot be labelled, and `allowed` is not one");
        assert_eq!(s[0].window_minutes, 15);
        assert_eq!(s[0].label.as_deref(), Some("primary_window · 15 分钟"));
    }

    #[test]
    fn used_percent_is_clamped_and_junk_yields_nothing() {
        let body: Value = serde_json::from_str(
            r#"{"rate_limits":{"secondary_window":{"used_percent":140.5,"limit_window_seconds":604800}}}"#,
        )
        .unwrap();
        let s = samples_from(&body);
        assert_eq!(s[0].used_percent, 100.0);
        assert_eq!(s[0].resets_at_ms, 0, "no reset known, not a fake one");
        assert!(samples_from(&Value::Null).is_empty());
        assert!(samples_from(&serde_json::from_str(r#"{"rate_limit":null}"#).unwrap()).is_empty());
        assert!(samples_from(&serde_json::from_str(r#"{"rate_limit":{}}"#).unwrap()).is_empty());
    }

    #[test]
    fn a_monthly_bucket_is_not_a_codex_window() {
        let body: Value = serde_json::from_str(
            r#"{"rate_limit":{"primary_window":{"used_percent":12,"limit_window_seconds":18000,"reset_at":1790188292},"secondary_window":{"used_percent":99,"limit_window_seconds":2592000,"reset_at":1790739004}}}"#,
        )
        .unwrap();
        let samples = samples_from(&body);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].window_minutes, 300);
        assert!(!samples.iter().any(|s| s.window_minutes >= 43_200));
    }

    #[test]
    fn the_credential_needs_both_a_token_and_keeps_the_account_id() {
        let (token, account) = token_in(r#"{"tokens":{"access_token":"  eyJfake.jwt.sig  ","account_id":"acct-1"}}"#).unwrap();
        assert_eq!(token, "eyJfake.jwt.sig", "trimmed, never logged");
        assert_eq!(account.as_deref(), Some("acct-1"));
        assert!(token_in(r#"{"tokens":{"access_token":""}}"#).is_none(), "an empty token is not a login");
        assert!(token_in(r#"{"OPENAI_API_KEY":"sk-x"}"#).is_none(), "an api-key-only file has no rate window");
        assert!(token_in("not json").is_none());
        let (_, missing) = token_in(r#"{"tokens":{"access_token":"t"}}"#).unwrap();
        assert_eq!(missing, None, "the account header is optional");
    }

    #[test]
    #[ignore]
    fn live_usage_endpoint_answers_for_this_account() {
        let samples = CodexQuota.fetch();
        assert!(!samples.is_empty(), "no ~/.codex/auth.json tokens.access_token, or it expired (401)?");
        for s in &samples {
            println!("codex quota: {:.2}% window={} label={:?} resets_at_ms={}", s.used_percent, s.window_minutes, s.label, s.resets_at_ms);
        }
    }
}
