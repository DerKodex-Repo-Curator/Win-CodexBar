use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use codexbar::core::{ProviderError, RateWindow, UsageSnapshot};
use codexbar::providers::claude::accounts::{self, ClaudeAccount};
use codexbar::{atomic_file, secure_file};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};

use crate::state::AppState;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeAccountWindow {
    pub used_percent: f64,
    pub resets_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeAccountUsage {
    pub five_hour: Option<ClaudeAccountWindow>,
    pub seven_day: Option<ClaudeAccountWindow>,
    pub updated_at: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeAccountUsageState {
    pub usage: Option<ClaudeAccountUsage>,
    pub usage_error: Option<String>,
    #[serde(default)]
    pub needs_authentication: bool,
}

#[derive(Serialize, Deserialize)]
pub struct ClaudeAccountState {
    #[serde(flatten)]
    pub account: ClaudeAccount,
    #[serde(flatten)]
    pub usage: ClaudeAccountUsageState,
}

#[tauri::command]
pub fn get_claude_accounts_state(
    state: tauri::State<'_, Mutex<AppState>>,
) -> Result<Vec<ClaudeAccountState>, String> {
    if state
        .lock()
        .map_err(|e| e.to_string())?
        .proof_config
        .is_some()
        && let Some(accounts) = crate::proof_harness::seed_claude_accounts_from_env()
    {
        return Ok(accounts);
    }
    let accounts = super::claude_accounts_list()?;
    let allow_reading_credentials =
        codexbar::settings::Settings::load().claude_allow_reading_claude_code_credentials;
    let state = state.lock().map_err(|e| e.to_string())?;
    Ok(accounts
        .into_iter()
        .map(|account| {
            let mut usage = state
                .claude_account_usage
                .get(&account.id)
                .cloned()
                .unwrap_or_default();
            if !allow_reading_credentials {
                usage = ClaudeAccountUsageState { usage: None, usage_error: Some(
                    "Enable Allow reading Claude Code's credentials in Settings to check saved accounts.".into()
                ), needs_authentication: false };
            }
            ClaudeAccountState { account, usage }
        })
        .collect())
}

fn quota_window(window: &RateWindow) -> Option<ClaudeAccountWindow> {
    (!window.is_informational && window.used_percent.is_finite()).then(|| ClaudeAccountWindow {
        used_percent: window.used_percent.clamp(0.0, 100.0),
        resets_at: window.resets_at.map(|at| at.to_rfc3339()),
    })
}

fn update_usage(
    previous: &mut ClaudeAccountUsageState,
    result: Result<UsageSnapshot, ProviderError>,
) {
    match result {
        Ok(usage) => {
            previous.usage = Some(ClaudeAccountUsage {
                five_hour: quota_window(&usage.primary),
                seven_day: usage.secondary.as_ref().and_then(quota_window),
                updated_at: usage.updated_at.to_rfc3339(),
            });
            previous.usage_error = None;
            previous.needs_authentication = false;
        }
        Err(error) => {
            previous.needs_authentication = matches!(
                error,
                ProviderError::OAuthRevoked(_)
                    | ProviderError::OAuthExpired(_)
                    | ProviderError::AuthRequired
                    | ProviderError::NoCookies
            );
            // Never expose response bodies or credentials through this bridge.
            previous.usage_error = Some(
                if previous.needs_authentication {
                    "Sign in again to check this Claude account."
                } else {
                    "Claude usage check failed. Will retry automatically."
                }
                .into(),
            );
        }
    }
}

fn publish_results(
    state: &mut AppState,
    generation: u64,
    live_accounts: &[ClaudeAccount],
    results: Vec<(String, Result<UsageSnapshot, ProviderError>)>,
) -> bool {
    if state.provider_refresh_generation != generation {
        return false;
    }
    let live_ids: HashMap<_, _> = live_accounts.iter().map(|a| (a.id.as_str(), a)).collect();
    state
        .claude_account_usage
        .retain(|id, _| live_ids.contains_key(id.as_str()));
    for (id, result) in results {
        if live_ids.contains_key(id.as_str()) {
            update_usage(state.claude_account_usage.entry(id).or_default(), result);
        }
    }
    true
}

const ACCOUNT_USAGE_STORE_VERSION: u32 = 1;
const ACCOUNT_USAGE_STORE_RELATIVE_PATH: &str = "claude/account-usage-v1.json";

/// Holds only rendered percentages, reset times and sanitized error text.
/// Credentials stay in the account store and are never written here.
#[derive(Serialize, Deserialize)]
struct ClaudeAccountUsageStore {
    version: u32,
    #[serde(default)]
    accounts: BTreeMap<String, ClaudeAccountUsageState>,
}

fn encode_account_usage(
    usage: &HashMap<String, ClaudeAccountUsageState>,
) -> serde_json::Result<String> {
    serde_json::to_string_pretty(&ClaudeAccountUsageStore {
        version: ACCOUNT_USAGE_STORE_VERSION,
        accounts: usage
            .iter()
            .map(|(id, state)| (id.clone(), state.clone()))
            .collect(),
    })
}

/// An unreadable or unsupported store is ignored so it never blocks startup.
fn decode_account_usage(raw: &str) -> Option<HashMap<String, ClaudeAccountUsageState>> {
    let store: ClaudeAccountUsageStore = serde_json::from_str(raw).ok()?;
    (store.version == ACCOUNT_USAGE_STORE_VERSION).then(|| store.accounts.into_iter().collect())
}

fn account_usage_store_path() -> Option<PathBuf> {
    codexbar::logging::config_root().map(|root| root.join(ACCOUNT_USAGE_STORE_RELATIVE_PATH))
}

/// Last-known per-account usage, restored at launch so the tray can show it
/// before the first refresh completes.
pub(crate) fn load_account_usage_cache() -> HashMap<String, ClaudeAccountUsageState> {
    let Some(path) = account_usage_store_path() else {
        return HashMap::new();
    };
    match secure_file::read_string(&path) {
        Ok(raw) => decode_account_usage(&raw).unwrap_or_else(|| {
            tracing::warn!("ignoring unreadable Claude account usage cache");
            HashMap::new()
        }),
        Err(error) => {
            if error.kind() != io::ErrorKind::NotFound {
                tracing::warn!("failed to read Claude account usage cache: {error}");
            }
            HashMap::new()
        }
    }
}

fn save_account_usage_cache(usage: &HashMap<String, ClaudeAccountUsageState>) -> io::Result<()> {
    let Some(path) = account_usage_store_path() else {
        return Ok(());
    };
    let raw = encode_account_usage(usage).map_err(io::Error::other)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // One fixed staging name is overwritten on each save, so no temp files accumulate.
    let staged = path.with_extension("json.tmp");
    secure_file::write_string(&staged, &raw)?;
    atomic_file::replace_staged(&staged, &path)
}

/// Share the provider refresh cadence and permits; each request uses only the
/// selected account's OAuth credentials and leaves the active CLI login intact.
pub(crate) async fn refresh_claude_account_lanes(
    app: tauri::AppHandle,
    permits: Arc<tokio::sync::Semaphore>,
    generation: u64,
) {
    if !codexbar::settings::Settings::load().claude_allow_reading_claude_code_credentials {
        return;
    }
    let Ok(accounts) = super::claude_accounts_list() else {
        return;
    };
    // One task walks the accounts in turn. Concurrent lanes burst the usage
    // endpoint and one account's 429 used to block every other account (#787).
    // The task owns each credential renewal and persistence, so a UI that stops
    // waiting never cancels a refresh-token rotation half way.
    let worker = tokio::spawn(async move {
        let mut results = Vec::new();
        for account in accounts {
            let Ok(_permit) = permits.acquire().await else {
                break;
            };
            let result = accounts::fetch_usage(&account.id)
                .await
                .map(|result| result.usage);
            results.push((account.id, result));
        }
        results
    });
    let results = worker.await.unwrap_or_default();
    let Ok(accounts) = super::claude_accounts_list() else {
        return;
    };
    let state = app.state::<Mutex<AppState>>();
    let usage = state.lock().ok().and_then(|mut state| {
        publish_results(&mut state, generation, &accounts, results)
            .then(|| state.claude_account_usage.clone())
    });
    let Some(usage) = usage else {
        return;
    };
    if let Err(error) = save_account_usage_cache(&usage) {
        tracing::warn!("failed to persist Claude account usage: {error}");
    }
    let _ = app.emit("claude-accounts-updated", ());
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || crate::tray_bridge::rebuild_tray_menu(&handle));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_api_errors_do_not_request_sign_in() {
        let mut state = ClaudeAccountUsageState::default();
        update_usage(
            &mut state,
            Err(ProviderError::OAuth("API error 500".into())),
        );
        assert!(!state.usage_error.unwrap().contains("Sign in"));
        assert!(!state.needs_authentication);
    }

    #[test]
    fn failures_preserve_last_good_limits_and_recovery_clears_error() {
        let mut state = ClaudeAccountUsageState::default();
        let usage = UsageSnapshot::new(RateWindow::new(23.0));
        update_usage(&mut state, Ok(usage.clone()));
        update_usage(
            &mut state,
            Err(ProviderError::OAuthExpired("expired".into())),
        );
        assert_eq!(
            state
                .usage
                .as_ref()
                .unwrap()
                .five_hour
                .as_ref()
                .unwrap()
                .used_percent,
            23.0
        );
        assert!(state.usage_error.as_ref().unwrap().contains("Sign in"));
        assert!(state.needs_authentication);
        update_usage(&mut state, Ok(usage));
        assert!(state.usage_error.is_none());
        assert!(!state.needs_authentication);
    }

    #[test]
    fn removed_accounts_and_superseded_generations_cannot_publish() {
        let mut state = AppState::new();
        let generation = state.provider_refresh_generation;
        state.provider_refresh_generation += 1;
        assert!(!publish_results(&mut state, generation, &[], vec![]));
        let generation = state.provider_refresh_generation;
        assert!(publish_results(
            &mut state,
            generation,
            &[],
            vec![(
                "removed".into(),
                Ok(UsageSnapshot::new(RateWindow::new(10.0)))
            )]
        ));
        assert!(state.claude_account_usage.is_empty());
    }

    #[test]
    fn missing_session_is_unavailable_instead_of_zero_usage() {
        assert!(quota_window(&RateWindow::no_active_session()).is_none());
    }

    #[test]
    fn account_usage_cache_round_trips_per_account_and_rejects_other_versions() {
        let mut usage = HashMap::new();
        usage.insert(
            "acct-a".to_string(),
            ClaudeAccountUsageState {
                usage: Some(ClaudeAccountUsage {
                    five_hour: Some(ClaudeAccountWindow {
                        used_percent: 42.0,
                        resets_at: None,
                    }),
                    seven_day: None,
                    updated_at: "2026-10-18T00:00:00+00:00".into(),
                }),
                usage_error: None,
                needs_authentication: false,
            },
        );
        usage.insert(
            "acct-b".to_string(),
            ClaudeAccountUsageState {
                usage_error: Some("Sign in again to check this Claude account.".into()),
                needs_authentication: true,
                ..Default::default()
            },
        );
        let raw = encode_account_usage(&usage).unwrap();
        let decoded = decode_account_usage(&raw).expect("current version decodes");
        assert_eq!(decoded.len(), 2);
        assert_eq!(
            decoded["acct-a"]
                .usage
                .as_ref()
                .unwrap()
                .five_hour
                .as_ref()
                .unwrap()
                .used_percent,
            42.0
        );
        assert!(decoded["acct-b"].needs_authentication);
        let other_version = raw.replace("\"version\": 1", "\"version\": 2");
        assert!(decode_account_usage(&other_version).is_none());
        assert!(decode_account_usage("not json").is_none());
    }
}
