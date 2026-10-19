//! Detect which subscription-type providers opencode is authenticated against,
//! and load the credentials its quota fetchers need.
//!
//! opencode can sign in to providers via OAuth (a shared subscription, e.g.
//! "Sign in with ChatGPT" = the Codex/ChatGPT plan) or via API keys (metered).
//! Its `auth.json` under the XDG data root records each provider with a `type`.
//! We surface the `type: "oauth"` providers so the user can see which agent
//! subscriptions opencode also draws on (its usage counts against those plans).
//!
//! Two credential stores exist in the wild, both under the opencode data
//! directory:
//!
//! - v1 recorded every provider in `auth.json`: `{"type": "api", "key": …}`
//!   for metered keys, `{"type": "oauth", …}` for subscriptions.
//! - v2 imported that file once (`import_legacy_credentials`) and keeps
//!   credentials in the `credential` table of its SQLite database
//!   (`opencode.db`, or `opencode-<channel>.db`), naming api keys
//!   `{"type": "key", …}` from then on. `auth logout` deletes the row while
//!   the legacy file keeps its pre-migration entries forever.
//!
//! One lookup serves both generations. A readable v2 store is authoritative: a
//! missing row is a signed-out integration, never a reason to resurrect the
//! legacy file. Without a v2 store the `auth.json` path behaves exactly as
//! before. Credentials from the two stores carry different `semantic_source`
//! values, so their cache lineage never merges.

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a credential-store read waits on a concurrent writer before the
/// database counts as unusable; same posture as `cursor_desktop::read_login`.
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_millis(3000);

#[derive(Debug, Deserialize)]
struct AuthEntry {
    #[serde(default)]
    r#type: Option<String>,
}

/// Friendly subscription labels for opencode's OAuth providers, in a stable order.
pub fn detect_subscriptions() -> Vec<String> {
    let Some(dir) = opencode_dir() else {
        return Vec::new();
    };
    detect_subscriptions_in(&dir)
}

fn detect_subscriptions_in(dir: &Path) -> Vec<String> {
    match load_credential_store(dir) {
        Some(rows) => subscription_labels_from_store(&rows),
        None => legacy_subscription_labels(dir),
    }
}

/// v1's list: `auth.json` entries typed `oauth`.
fn legacy_subscription_labels(dir: &Path) -> Vec<String> {
    let Ok(raw) = std::fs::read_to_string(dir.join("auth.json")) else {
        return Vec::new();
    };
    let Ok(entries) = serde_json::from_str::<BTreeMap<String, AuthEntry>>(&raw) else {
        return Vec::new();
    };
    let mut labels: Vec<String> = entries
        .into_iter()
        .filter(|(_, entry)| {
            entry
                .r#type
                .as_deref()
                .is_some_and(|t| t.eq_ignore_ascii_case("oauth"))
        })
        .map(|(provider, _)| subscription_label(&provider))
        .collect();
    labels.sort();
    labels.dedup();
    labels
}

/// v2's list: OAuth rows, one per integration. Connector OAuth (`mcp_<hash>`
/// integrations, e.g. Linear or Notion sign-ins) is not a model subscription.
fn subscription_labels_from_store(rows: &[StoredCredential]) -> Vec<String> {
    let mut labels: Vec<String> = rows
        .iter()
        .map(|row| row.integration_id.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|integration_id| !integration_id.starts_with("mcp_"))
        .filter_map(|integration_id| select_credential(rows, integration_id))
        .filter(|row| subscription_value(&row.value))
        .map(|row| subscription_label(&row.integration_id))
        .collect();
    labels.sort();
    labels.dedup();
    labels
}

fn subscription_value(value: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(value) else {
        return false;
    };
    value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|kind| kind.eq_ignore_ascii_case("oauth"))
}

fn subscription_label(provider: &str) -> String {
    match provider.to_lowercase().as_str() {
        "openai" => "Codex".to_string(),
        "anthropic" => "Claude".to_string(),
        "github-copilot" | "copilot" => "Copilot".to_string(),
        "google" | "gemini" => "Gemini".to_string(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                None => provider.to_string(),
            }
        }
    }
}

/// The opencode data directory; the v1 `auth.json` and the v2 `*.db` both live
/// inside it.
fn opencode_dir() -> Option<PathBuf> {
    opencode_dir_from(
        std::env::var("XDG_DATA_HOME"),
        crate::user_home_dir().as_deref(),
    )
}

fn opencode_dir_from(
    xdg_data_home: Result<String, std::env::VarError>,
    user_home: Option<&Path>,
) -> Option<PathBuf> {
    let root = match xdg_data_home {
        Ok(root) if !root.is_empty() => root,
        Ok(_) | Err(_) => format!("{}/.local/share", user_home?.to_string_lossy()),
    };
    Some(PathBuf::from(format!("{root}/opencode")))
}

/// One row of v2's `credential` table. `db_path` is kept so a credential's
/// canonical location names the database that actually holds it.
struct StoredCredential {
    db_path: PathBuf,
    id: String,
    integration_id: String,
    value: String,
    active: Option<i64>,
    created_at: i64,
}

/// Every credential v2 has stored, or `None` when this machine has no v2 store
/// at all (no database, or none carrying the `credential` table) — the v1
/// layout. `Some` is authoritative for every integration: v2's `auth logout`
/// deletes the row while a legacy `auth.json` keeps its pre-migration entries
/// forever, so a missing row means signed out, never "read the older file".
fn load_credential_store(dir: &Path) -> Option<Vec<StoredCredential>> {
    let mut store_present = false;
    let mut rows = Vec::new();
    for db_path in credential_db_candidates(dir) {
        let Some(conn) = open_credential_store(&db_path) else {
            continue;
        };
        store_present = true;
        let Ok(mut statement) =
            conn.prepare("SELECT id, integration_id, value, active, time_created FROM credential")
        else {
            continue;
        };
        let Ok(stored) = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, i64>(4)?,
            ))
        }) else {
            continue;
        };
        for (id, integration_id, value, active, created_at) in stored.flatten() {
            let Some(integration_id) = integration_id.filter(|id| !id.is_empty()) else {
                continue;
            };
            rows.push(StoredCredential {
                db_path: db_path.clone(),
                id,
                integration_id,
                value,
                active,
                created_at,
            });
        }
    }
    store_present.then_some(rows)
}

/// The row that represents an integration right now. Databases are preferred
/// in candidate order (the default store first); within one database, match
/// OpenCode's descending `active`, `time_created`, `id` order. In particular,
/// updating an older legacy row must not select it over a newer credential.
fn select_credential<'a>(
    rows: &'a [StoredCredential],
    integration_id: &str,
) -> Option<&'a StoredCredential> {
    let database = rows
        .iter()
        .find(|row| row.integration_id == integration_id)?
        .db_path
        .as_path();
    rows.iter()
        .filter(|row| row.integration_id == integration_id && row.db_path.as_path() == database)
        .max_by_key(|row| (row.active, row.created_at, row.id.as_str()))
}

/// The credential databases to try, default channel first: `opencode.db`
/// before any `opencode-<channel>.db` variant, each sorted. The filename rule
/// mirrors the engine scanner's `discover_opencode_dbs`
/// (`vendor/tokscale-core/src/scanner.rs`), so both layers agree on what an
/// opencode database looks like.
fn credential_db_candidates(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dbs: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_file() {
                return None;
            }
            let name = path.file_name()?.to_str()?;
            is_credential_db_filename(name).then_some(path)
        })
        .collect();
    dbs.sort_unstable();
    if let Some(position) = dbs
        .iter()
        .position(|path| path.file_name().is_some_and(|name| name == "opencode.db"))
    {
        dbs.swap(0, position);
    }
    dbs
}

/// `opencode.db` or `opencode-<channel>.db` with the channel drawn from
/// `[a-zA-Z0-9._-]` (upstream sanitizes channels with
/// `/[^a-zA-Z0-9._-]/g -> "-"`). Sidecars (`opencode.db-wal`, `opencode.db-shm`)
/// end elsewhere and never match.
fn is_credential_db_filename(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".db") else {
        return false;
    };
    if stem == "opencode" {
        return true;
    }
    let Some(channel) = stem.strip_prefix("opencode-") else {
        return false;
    };
    !channel.is_empty()
        && channel
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Open one candidate read-only and confirm it is a credential store. Every
/// failure (absent file, unreadable database, pre-`credential` schema) is
/// `None`: the caller keeps looking, and a machine with no usable store falls
/// back to the v1 `auth.json` path.
fn open_credential_store(db_path: &Path) -> Option<rusqlite::Connection> {
    if !db_path.is_file() {
        return None;
    }
    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    conn.busy_timeout(SQLITE_BUSY_TIMEOUT).ok()?;
    let has_table = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'credential' LIMIT 1",
            [],
            |_| Ok(()),
        )
        .is_ok();
    has_table.then_some(conn)
}

/// The `type` field of a credential entry, against the type vocabulary of the
/// store it came from (`api`/`oauth` in v1, `key`/`oauth` in v2).
enum EntryType {
    Expected,
    Other,
    Malformed,
}

fn entry_type(entry: &serde_json::Value, expected: &str) -> EntryType {
    match entry.get("type") {
        Some(serde_json::Value::String(kind)) if kind.eq_ignore_ascii_case(expected) => {
            EntryType::Expected
        }
        Some(serde_json::Value::String(_)) => EntryType::Other,
        _ => EntryType::Malformed,
    }
}

/// The outcome of reading one credential shape out of either store, with the
/// absent/malformed distinctions the loaders have always drawn.
enum EntryOutcome {
    Present(String),
    Absent,
    Malformed,
}

/// The `key` of an api-key entry. A missing, null, or empty key is signed out;
/// a present non-string key is malformed evidence.
fn entry_api_key(entry: &serde_json::Value) -> EntryOutcome {
    match entry.get("key") {
        Some(serde_json::Value::String(value)) if !value.trim().is_empty() => {
            EntryOutcome::Present(value.trim().to_string())
        }
        None | Some(serde_json::Value::Null) => EntryOutcome::Absent,
        Some(serde_json::Value::String(_)) => EntryOutcome::Absent,
        Some(_) => EntryOutcome::Malformed,
    }
}

/// The first non-empty `refresh`/`access` token of an OAuth entry. A token
/// field that is present but not a string is malformed; an entry whose token
/// fields are all null/empty is absent; an entry with no token fields at all
/// is malformed.
fn entry_oauth_token(entry: &serde_json::Value) -> EntryOutcome {
    let mut token = None;
    let mut saw_token_field = false;
    for key in ["refresh", "access"] {
        match entry.get(key) {
            None => {}
            Some(serde_json::Value::Null) => saw_token_field = true,
            Some(serde_json::Value::String(value)) if value.trim().is_empty() => {
                saw_token_field = true;
            }
            Some(serde_json::Value::String(value)) if token.is_none() => {
                saw_token_field = true;
                token = Some(value.trim().to_string());
            }
            Some(serde_json::Value::String(_)) => saw_token_field = true,
            Some(_) => return EntryOutcome::Malformed,
        }
    }
    match token {
        Some(token) => EntryOutcome::Present(token),
        None if saw_token_field => EntryOutcome::Absent,
        None => EntryOutcome::Malformed,
    }
}

/// A v2 `credential.value` in the api-key shape (`type: "key"`).
fn database_api_key(value: &str) -> EntryOutcome {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(value) else {
        return EntryOutcome::Malformed;
    };
    if !value.is_object() {
        return EntryOutcome::Malformed;
    }
    match entry_type(&value, "key") {
        EntryType::Expected => entry_api_key(&value),
        EntryType::Other => EntryOutcome::Absent,
        EntryType::Malformed => EntryOutcome::Malformed,
    }
}

/// A v2 `credential.value` in the OAuth shape (`type: "oauth"`).
fn database_oauth_token(value: &str) -> EntryOutcome {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(value) else {
        return EntryOutcome::Malformed;
    };
    if !value.is_object() {
        return EntryOutcome::Malformed;
    }
    match entry_type(&value, "oauth") {
        EntryType::Expected => entry_oauth_token(&value),
        EntryType::Other => EntryOutcome::Absent,
        EntryType::Malformed => EntryOutcome::Malformed,
    }
}

pub(crate) struct GitHubCopilotCredential {
    pub(crate) request_token: String,
    pub(crate) marker: Vec<u8>,
    pub(crate) semantic_source: &'static str,
    pub(crate) canonical_location: String,
}

pub(crate) enum GitHubCopilotCredentialLoad {
    Absent,
    Present(GitHubCopilotCredential),
    Terminal(String),
}

/// Load only the durable GitHub OAuth credential used by Copilot — the v2
/// `credential` row when a v2 store exists, else the v1 `auth.json` entry. A
/// missing store/row or explicit logout is `Absent`; storage, syntax,
/// canonicalization, or a present malformed OAuth entry is `Terminal` and must
/// not be hidden as a signed-out state.
pub(crate) fn github_copilot_credential() -> GitHubCopilotCredentialLoad {
    let Some(dir) = opencode_dir() else {
        return GitHubCopilotCredentialLoad::Terminal(
            "OpenCode auth location could not be resolved.".to_string(),
        );
    };
    github_copilot_credential_in(&dir)
}

fn github_copilot_credential_in(dir: &Path) -> GitHubCopilotCredentialLoad {
    let Some(rows) = load_credential_store(dir) else {
        return github_copilot_credential_at(&dir.join("auth.json"));
    };
    let Some(row) = select_credential(&rows, "github-copilot") else {
        return GitHubCopilotCredentialLoad::Absent;
    };
    match database_oauth_token(&row.value) {
        EntryOutcome::Present(token) => {
            present_copilot(&row.db_path, token, "opencode-credential-db")
        }
        EntryOutcome::Absent => GitHubCopilotCredentialLoad::Absent,
        EntryOutcome::Malformed => GitHubCopilotCredentialLoad::Terminal(
            "OpenCode Copilot OAuth entry is malformed.".to_string(),
        ),
    }
}

/// The v1 `auth.json` reader.
fn github_copilot_credential_at(path: &Path) -> GitHubCopilotCredentialLoad {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return GitHubCopilotCredentialLoad::Absent;
        }
        Err(_) => {
            return GitHubCopilotCredentialLoad::Terminal(
                "OpenCode auth file could not be read.".to_string(),
            );
        }
    };
    let json = match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(json) => json,
        Err(_) => {
            return GitHubCopilotCredentialLoad::Terminal(
                "OpenCode auth file could not be decoded.".to_string(),
            );
        }
    };
    github_copilot_credential_from(path, &json)
}

fn github_copilot_credential_from(
    path: &Path,
    json: &serde_json::Value,
) -> GitHubCopilotCredentialLoad {
    let Some(entry) = json.get("github-copilot") else {
        return GitHubCopilotCredentialLoad::Absent;
    };
    if !entry.is_object() {
        return GitHubCopilotCredentialLoad::Terminal(
            "OpenCode Copilot OAuth entry is malformed.".to_string(),
        );
    }
    match entry_type(entry, "oauth") {
        EntryType::Expected => {}
        EntryType::Other => return GitHubCopilotCredentialLoad::Absent,
        EntryType::Malformed => {
            return GitHubCopilotCredentialLoad::Terminal(
                "OpenCode Copilot OAuth entry is malformed.".to_string(),
            );
        }
    }
    match entry_oauth_token(entry) {
        EntryOutcome::Present(token) => present_copilot(path, token, "opencode-auth-json"),
        EntryOutcome::Absent => GitHubCopilotCredentialLoad::Absent,
        EntryOutcome::Malformed => GitHubCopilotCredentialLoad::Terminal(
            "OpenCode Copilot OAuth entry is malformed.".to_string(),
        ),
    }
}

fn present_copilot(
    path: &Path,
    token: String,
    semantic_source: &'static str,
) -> GitHubCopilotCredentialLoad {
    let canonical_location =
        match crate::agent_account_scope::canonical_file_location(path, Some("github-copilot")) {
            Ok(location) => location,
            Err(_) => {
                return GitHubCopilotCredentialLoad::Terminal(
                    "OpenCode Copilot auth location could not be verified.".to_string(),
                );
            }
        };
    GitHubCopilotCredentialLoad::Present(GitHubCopilotCredential {
        request_token: token.clone(),
        marker: token.into_bytes(),
        semantic_source,
        canonical_location,
    })
}

pub(crate) struct OpenCodeGoCredential {
    pub(crate) request_token: String,
    pub(crate) marker: Vec<u8>,
    pub(crate) semantic_source: &'static str,
    pub(crate) canonical_location: String,
}

pub(crate) enum OpenCodeGoCredentialLoad {
    Absent,
    Present(OpenCodeGoCredential),
    Terminal(String),
}

/// Load the OpenCode Go API key — the v2 `credential` row (`type: "key"`) when
/// a v2 store exists, else the v1 `auth.json` entry (`type: "api"`). A missing
/// store/row, a non-api-key entry, or an empty key is `Absent`; storage,
/// syntax, canonicalization, or a present malformed entry is `Terminal` and
/// must not be hidden as signed-out. This mirrors mana.bar's
/// `OpenCodeGoProvider.loadApiKey`, minus the env-var key override, which
/// Syrtis does not use.
pub(crate) fn opencode_go_credential() -> OpenCodeGoCredentialLoad {
    let Some(dir) = opencode_dir() else {
        return OpenCodeGoCredentialLoad::Terminal(
            "OpenCode auth location could not be resolved.".to_string(),
        );
    };
    opencode_go_credential_in(&dir)
}

fn opencode_go_credential_in(dir: &Path) -> OpenCodeGoCredentialLoad {
    let Some(rows) = load_credential_store(dir) else {
        return opencode_go_credential_at(&dir.join("auth.json"));
    };
    let Some(row) = select_credential(&rows, "opencode-go") else {
        return OpenCodeGoCredentialLoad::Absent;
    };
    match database_api_key(&row.value) {
        EntryOutcome::Present(key) => {
            present_opencode_go(&row.db_path, key, "opencode-credential-db")
        }
        EntryOutcome::Absent => OpenCodeGoCredentialLoad::Absent,
        EntryOutcome::Malformed => {
            OpenCodeGoCredentialLoad::Terminal("OpenCode Go auth entry is malformed.".to_string())
        }
    }
}

/// The v1 `auth.json` reader.
fn opencode_go_credential_at(path: &Path) -> OpenCodeGoCredentialLoad {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return OpenCodeGoCredentialLoad::Absent;
        }
        Err(_) => {
            return OpenCodeGoCredentialLoad::Terminal(
                "OpenCode auth file could not be read.".to_string(),
            );
        }
    };
    let json = match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(json) => json,
        Err(_) => {
            return OpenCodeGoCredentialLoad::Terminal(
                "OpenCode auth file could not be decoded.".to_string(),
            );
        }
    };
    opencode_go_credential_from(path, &json)
}

fn opencode_go_credential_from(path: &Path, json: &serde_json::Value) -> OpenCodeGoCredentialLoad {
    let Some(entry) = json.get("opencode-go") else {
        return OpenCodeGoCredentialLoad::Absent;
    };
    if !entry.is_object() {
        return OpenCodeGoCredentialLoad::Terminal(
            "OpenCode Go auth entry is malformed.".to_string(),
        );
    }
    match entry_type(entry, "api") {
        EntryType::Expected => {}
        EntryType::Other => return OpenCodeGoCredentialLoad::Absent,
        EntryType::Malformed => {
            return OpenCodeGoCredentialLoad::Terminal(
                "OpenCode Go auth entry is malformed.".to_string(),
            );
        }
    }
    match entry_api_key(entry) {
        EntryOutcome::Present(key) => present_opencode_go(path, key, "opencode-auth-json"),
        EntryOutcome::Absent => OpenCodeGoCredentialLoad::Absent,
        EntryOutcome::Malformed => {
            OpenCodeGoCredentialLoad::Terminal("OpenCode Go auth entry is malformed.".to_string())
        }
    }
}

fn present_opencode_go(
    path: &Path,
    key: String,
    semantic_source: &'static str,
) -> OpenCodeGoCredentialLoad {
    let canonical_location =
        match crate::agent_account_scope::canonical_file_location(path, Some("opencode-go")) {
            Ok(location) => location,
            Err(_) => {
                return OpenCodeGoCredentialLoad::Terminal(
                    "OpenCode Go auth location could not be verified.".to_string(),
                );
            }
        };
    OpenCodeGoCredentialLoad::Present(OpenCodeGoCredential {
        request_token: key.clone(),
        marker: key.into_bytes(),
        semantic_source,
        canonical_location,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh per-test directory under the process temp dir.
    fn fixture_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tokenbar-opencode-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Writes a v2 credential store. Rows are
    /// `(integration_id, value_json, active, time_created)`; initially the
    /// update time matches the creation time, as on a newly inserted row.
    fn write_credential_db(dir: &Path, file: &str, rows: &[(&str, &str, Option<i64>, i64)]) {
        let conn = rusqlite::Connection::open(dir.join(file)).unwrap();
        conn.execute_batch(
            "CREATE TABLE `credential` (
                `id` text PRIMARY KEY,
                `integration_id` text,
                `label` text NOT NULL,
                `value` text NOT NULL,
                `connector_id` text,
                `method_id` text,
                `active` integer,
                `time_created` integer NOT NULL,
                `time_updated` integer NOT NULL
            );",
        )
        .unwrap();
        for (index, (integration_id, value, active, time_created)) in rows.iter().enumerate() {
            conn.execute(
                "INSERT INTO credential \
                 (id, integration_id, label, value, active, time_created, time_updated) \
                 VALUES (?1, ?2, 'label', ?3, ?4, ?5, ?5)",
                rusqlite::params![
                    format!("row-{index}"),
                    integration_id,
                    value,
                    active,
                    time_created
                ],
            )
            .unwrap();
        }
    }

    #[test]
    fn opencode_go_key_present_absent_and_terminal() {
        let path = std::env::temp_dir().join("fixture-opencode-auth.json");
        // Present: an api entry with a non-empty key.
        let present = serde_json::json!({
            "opencode-go": { "type": "api", "key": "  sk-go-secret  " }
        });
        match opencode_go_credential_from(&path, &present) {
            OpenCodeGoCredentialLoad::Present(credential) => {
                assert_eq!(credential.request_token, "sk-go-secret");
                assert_eq!(credential.marker, b"sk-go-secret");
                assert_eq!(credential.semantic_source, "opencode-auth-json");
            }
            _ => panic!("expected present credential"),
        }
        // Absent: missing entry, non-api type, or empty/absent key.
        for json in [
            serde_json::json!({}),
            serde_json::json!({ "opencode-go": { "type": "oauth", "key": "x" } }),
            serde_json::json!({ "opencode-go": { "type": "api", "key": "  " } }),
            serde_json::json!({ "opencode-go": { "type": "api" } }),
        ] {
            assert!(matches!(
                opencode_go_credential_from(&path, &json),
                OpenCodeGoCredentialLoad::Absent
            ));
        }
        // Terminal: a present but malformed entry.
        for json in [
            serde_json::json!({ "opencode-go": "api" }),
            serde_json::json!({ "opencode-go": { "type": "api", "key": 42 } }),
            serde_json::json!({ "opencode-go": { "key": "x" } }),
        ] {
            assert!(matches!(
                opencode_go_credential_from(&path, &json),
                OpenCodeGoCredentialLoad::Terminal(_)
            ));
        }
    }

    #[test]
    fn opencode_go_file_loader_treats_missing_as_absent_and_io_or_json_as_terminal() {
        // Mirrors the Copilot file-loader matrix: a missing file is a signed-out
        // Absent, while an unreadable path (a directory) or undecodable content
        // is Terminal and must not be hidden as signed-out.
        let root = std::env::temp_dir().join(format!(
            "tokenbar-opencode-go-loader-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        assert!(matches!(
            opencode_go_credential_at(&root.join("missing.json")),
            OpenCodeGoCredentialLoad::Absent
        ));
        assert!(matches!(
            opencode_go_credential_at(&root),
            OpenCodeGoCredentialLoad::Terminal(_)
        ));

        let invalid = root.join("invalid.json");
        std::fs::write(&invalid, "not json").unwrap();
        assert!(matches!(
            opencode_go_credential_at(&invalid),
            OpenCodeGoCredentialLoad::Terminal(_)
        ));

        let present = root.join("present.json");
        std::fs::write(&present, r#"{"opencode-go":{"type":"api","key":"sk-go"}}"#).unwrap();
        assert!(matches!(
            opencode_go_credential_at(&present),
            OpenCodeGoCredentialLoad::Present(_)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn database_credential_wins_over_a_stale_legacy_file() {
        // The regression this module now guards: v2 rotated the Go key in its
        // SQLite store while `auth.json` still holds the pre-rotation key. The
        // store must win, and the credential must be marked with the
        // database's semantic source so cache lineage never merges the stores.
        let dir = fixture_dir("db-wins");
        write_credential_db(
            &dir,
            "opencode.db",
            &[(
                "opencode-go",
                r#"{"type":"key","key":"sk-go-new"}"#,
                Some(1),
                10,
            )],
        );
        std::fs::write(
            dir.join("auth.json"),
            r#"{"opencode-go":{"type":"api","key":"sk-go-old"}}"#,
        )
        .unwrap();

        match opencode_go_credential_in(&dir) {
            OpenCodeGoCredentialLoad::Present(credential) => {
                assert_eq!(credential.request_token, "sk-go-new");
                assert_eq!(credential.marker, b"sk-go-new");
                assert_eq!(credential.semantic_source, "opencode-credential-db");
                assert!(credential.canonical_location.contains("opencode.db"));
                assert!(credential.canonical_location.ends_with("opencode-go"));
            }
            OpenCodeGoCredentialLoad::Absent => panic!("expected the database credential"),
            OpenCodeGoCredentialLoad::Terminal(display) => {
                panic!("unexpected terminal load: {display}")
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn database_row_absence_is_authoritative_not_a_legacy_fallback() {
        // v2 `auth logout` deletes the credential row; the legacy file keeps
        // its pre-migration entries forever. A readable store with no row for
        // the integration therefore means signed out — reading the legacy file
        // would resurrect a credential the user logged out of.
        let dir = fixture_dir("db-signed-out");
        write_credential_db(&dir, "opencode.db", &[]);
        std::fs::write(
            dir.join("auth.json"),
            r#"{"opencode-go":{"type":"api","key":"sk-go-stale"}}"#,
        )
        .unwrap();
        assert!(matches!(
            opencode_go_credential_in(&dir),
            OpenCodeGoCredentialLoad::Absent
        ));
        assert!(matches!(
            github_copilot_credential_in(&dir),
            GitHubCopilotCredentialLoad::Absent
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn database_value_shapes_follow_the_legacy_contract() {
        // Present: a key entry with a non-empty key (trimmed).
        match database_api_key(r#"{"type":"key","key":" sk-go "}"#) {
            EntryOutcome::Present(key) => assert_eq!(key, "sk-go"),
            _ => panic!("expected a present key"),
        }
        // Absent: empty/missing key, or an entry that is not an api key.
        for value in [
            r#"{"type":"key","key":"  "}"#,
            r#"{"type":"key"}"#,
            r#"{"type":"key","key":null}"#,
            r#"{"type":"oauth","access":"token"}"#,
            r#"{"type":"wellknown","token":"token"}"#,
        ] {
            assert!(
                matches!(database_api_key(value), EntryOutcome::Absent),
                "{value}"
            );
        }
        // Malformed: unusable evidence must not masquerade as signed out.
        for value in [
            "not json",
            "[]",
            r#"{"key":"sk-go"}"#,
            r#"{"type":"key","key":42}"#,
        ] {
            assert!(
                matches!(database_api_key(value), EntryOutcome::Malformed),
                "{value}"
            );
        }
    }

    #[test]
    fn malformed_database_value_is_terminal_even_with_a_legacy_file() {
        let dir = fixture_dir("db-malformed");
        write_credential_db(
            &dir,
            "opencode.db",
            &[("opencode-go", "not json", Some(1), 10)],
        );
        std::fs::write(
            dir.join("auth.json"),
            r#"{"opencode-go":{"type":"api","key":"sk-go-legacy"}}"#,
        )
        .unwrap();
        assert!(matches!(
            opencode_go_credential_in(&dir),
            OpenCodeGoCredentialLoad::Terminal(_)
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn database_prefers_active_then_newest_created_row() {
        // `auth switch` marks the switched-to row active; a newer credential in an
        // inactive row must not override it.
        let dir = fixture_dir("db-active");
        write_credential_db(
            &dir,
            "opencode.db",
            &[
                (
                    "opencode-go",
                    r#"{"type":"key","key":"sk-inactive"}"#,
                    Some(0),
                    30,
                ),
                (
                    "opencode-go",
                    r#"{"type":"key","key":"sk-active"}"#,
                    Some(1),
                    20,
                ),
            ],
        );
        match opencode_go_credential_in(&dir) {
            OpenCodeGoCredentialLoad::Present(credential) => {
                assert_eq!(credential.request_token, "sk-active");
            }
            _ => panic!("expected the active credential"),
        }
        std::fs::remove_dir_all(dir).unwrap();

        // Legacy-imported rows carry `active = NULL`; the newest creation wins.
        let dir = fixture_dir("db-newest");
        write_credential_db(
            &dir,
            "opencode.db",
            &[
                (
                    "opencode-go",
                    r#"{"type":"key","key":"sk-oldest"}"#,
                    None,
                    10,
                ),
                (
                    "opencode-go",
                    r#"{"type":"key","key":"sk-newest"}"#,
                    None,
                    40,
                ),
            ],
        );
        match opencode_go_credential_in(&dir) {
            OpenCodeGoCredentialLoad::Present(credential) => {
                assert_eq!(credential.request_token, "sk-newest");
            }
            _ => panic!("expected the newest credential"),
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn updating_an_older_legacy_credential_does_not_switch_the_selected_account() {
        let dir = fixture_dir("db-update-keeps-account");
        write_credential_db(
            &dir,
            "opencode.db",
            &[
                (
                    "opencode-go",
                    r#"{"type":"key","key":"older-go-account"}"#,
                    None,
                    10,
                ),
                (
                    "opencode-go",
                    r#"{"type":"key","key":"current-go-account"}"#,
                    None,
                    20,
                ),
                (
                    "github-copilot",
                    r#"{"type":"oauth","refresh":"older-copilot-account"}"#,
                    None,
                    10,
                ),
                (
                    "github-copilot",
                    r#"{"type":"oauth","refresh":"current-copilot-account"}"#,
                    None,
                    20,
                ),
            ],
        );
        let assert_selected_accounts = || {
            match opencode_go_credential_in(&dir) {
                OpenCodeGoCredentialLoad::Present(credential) => {
                    assert_eq!(credential.request_token, "current-go-account");
                }
                _ => panic!("expected the current Go credential"),
            }
            match github_copilot_credential_in(&dir) {
                GitHubCopilotCredentialLoad::Present(credential) => {
                    assert_eq!(credential.request_token, "current-copilot-account");
                }
                _ => panic!("expected the current Copilot credential"),
            }
        };
        assert_selected_accounts();
        let conn = rusqlite::Connection::open(dir.join("opencode.db")).unwrap();
        // Renaming or refreshing an older credential changes time_updated,
        // without activating that credential in OpenCode.
        conn.execute(
            "UPDATE credential SET label = 'renamed', time_updated = 30 WHERE time_created = 10",
            [],
        )
        .unwrap();
        drop(conn);
        assert_selected_accounts();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn updating_an_older_legacy_oauth_row_does_not_invent_a_subscription() {
        let dir = fixture_dir("db-update-keeps-subscriptions");
        write_credential_db(
            &dir,
            "opencode.db",
            &[
                (
                    "openai",
                    r#"{"type":"oauth","access":"older-subscription"}"#,
                    None,
                    10,
                ),
                (
                    "openai",
                    r#"{"type":"key","key":"current-api-key"}"#,
                    None,
                    20,
                ),
            ],
        );
        assert!(detect_subscriptions_in(&dir).is_empty());
        let conn = rusqlite::Connection::open(dir.join("opencode.db")).unwrap();
        conn.execute(
            "UPDATE credential SET time_updated = 30 WHERE time_created = 10",
            [],
        )
        .unwrap();
        assert!(detect_subscriptions_in(&dir).is_empty());
        // The OAuth row is still usable once it really becomes selected.
        conn.execute("DELETE FROM credential WHERE time_created = 20", [])
            .unwrap();
        assert_eq!(detect_subscriptions_in(&dir), vec!["Codex"]);
        drop(conn);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn equally_old_legacy_credentials_use_id_order_instead_of_insertion_order() {
        let dir = fixture_dir("db-creation-tie");
        write_credential_db(
            &dir,
            "opencode.db",
            &[
                (
                    "opencode-go",
                    r#"{"type":"key","key":"higher-id"}"#,
                    None,
                    10,
                ),
                (
                    "opencode-go",
                    r#"{"type":"key","key":"lower-id"}"#,
                    None,
                    10,
                ),
            ],
        );
        let conn = rusqlite::Connection::open(dir.join("opencode.db")).unwrap();
        conn.execute(
            "UPDATE credential SET id = CASE id WHEN 'row-0' THEN 'cred-z' ELSE 'cred-a' END",
            [],
        )
        .unwrap();
        drop(conn);
        match opencode_go_credential_in(&dir) {
            OpenCodeGoCredentialLoad::Present(credential) => {
                assert_eq!(credential.request_token, "higher-id");
            }
            _ => panic!("expected the credential with the higher id"),
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn database_without_a_credential_table_falls_back_to_the_legacy_file() {
        // A pre-credential opencode database (sessions only) is not a v2
        // store; the legacy file keeps working exactly as before.
        let dir = fixture_dir("db-precredential");
        let conn = rusqlite::Connection::open(dir.join("opencode.db")).unwrap();
        conn.execute_batch("CREATE TABLE `session` (`id` text PRIMARY KEY);")
            .unwrap();
        drop(conn);
        std::fs::write(
            dir.join("auth.json"),
            r#"{"opencode-go":{"type":"api","key":"sk-go-legacy"}}"#,
        )
        .unwrap();
        match opencode_go_credential_in(&dir) {
            OpenCodeGoCredentialLoad::Present(credential) => {
                assert_eq!(credential.request_token, "sk-go-legacy");
                assert_eq!(credential.semantic_source, "opencode-auth-json");
            }
            _ => panic!("expected the legacy credential"),
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unreadable_database_falls_back_to_the_legacy_file() {
        let dir = fixture_dir("db-unreadable");
        std::fs::write(dir.join("opencode.db"), b"not a database").unwrap();
        std::fs::write(
            dir.join("auth.json"),
            r#"{"opencode-go":{"type":"api","key":"sk-go-legacy"}}"#,
        )
        .unwrap();
        assert!(matches!(
            opencode_go_credential_in(&dir),
            OpenCodeGoCredentialLoad::Present(_)
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn channel_database_filenames_are_recognized() {
        for name in ["opencode.db", "opencode-stable.db", "opencode-nightly.db"] {
            assert!(is_credential_db_filename(name), "{name}");
        }
        for name in [
            "opencode.db-wal",
            "opencode.db-shm",
            "opencode.db.bak",
            "opencode-.db",
            "opencode",
            "other.db",
        ] {
            assert!(!is_credential_db_filename(name), "{name}");
        }
    }

    #[test]
    fn channel_database_is_read_when_present() {
        let dir = fixture_dir("channel-db");
        write_credential_db(
            &dir,
            "opencode-stable.db",
            &[(
                "opencode-go",
                r#"{"type":"key","key":"sk-go-channel"}"#,
                Some(1),
                10,
            )],
        );
        match opencode_go_credential_in(&dir) {
            OpenCodeGoCredentialLoad::Present(credential) => {
                assert_eq!(credential.request_token, "sk-go-channel");
                assert!(credential.canonical_location.contains("opencode-stable.db"));
            }
            _ => panic!("expected the channel database credential"),
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn default_database_is_preferred_over_channel_variants() {
        // Candidates are tried default-first: when both stores have a row, the
        // default channel's database supplies the credential even if the
        // channel row was written later.
        let dir = fixture_dir("default-db-first");
        write_credential_db(
            &dir,
            "opencode.db",
            &[(
                "opencode-go",
                r#"{"type":"key","key":"sk-go-default"}"#,
                Some(1),
                10,
            )],
        );
        write_credential_db(
            &dir,
            "opencode-stable.db",
            &[(
                "opencode-go",
                r#"{"type":"key","key":"sk-go-channel"}"#,
                Some(1),
                99,
            )],
        );
        match opencode_go_credential_in(&dir) {
            OpenCodeGoCredentialLoad::Present(credential) => {
                assert_eq!(credential.request_token, "sk-go-default");
            }
            _ => panic!("expected the default database credential"),
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn database_copilot_oauth_uses_refresh_then_access() {
        for (label, value, expected) in [
            (
                "refresh preferred",
                r#"{"type":"oauth","refresh":"refresh-token","access":"access-token"}"#,
                Some("refresh-token"),
            ),
            (
                "refresh null",
                r#"{"type":"oauth","refresh":null,"access":" access-token "}"#,
                Some("access-token"),
            ),
            (
                "logged out entry",
                r#"{"type":"oauth","refresh":null,"access":"  "}"#,
                None,
            ),
        ] {
            let dir = fixture_dir(&format!("db-copilot-{}", label.replace(' ', "-")));
            write_credential_db(
                &dir,
                "opencode.db",
                &[("github-copilot", value, Some(1), 10)],
            );
            let loaded = github_copilot_credential_in(&dir);
            match expected {
                Some(expected) => match loaded {
                    GitHubCopilotCredentialLoad::Present(credential) => {
                        assert_eq!(credential.request_token, expected, "{label}");
                        assert_eq!(credential.semantic_source, "opencode-credential-db");
                    }
                    GitHubCopilotCredentialLoad::Absent => {
                        panic!("{label}: unexpectedly absent")
                    }
                    GitHubCopilotCredentialLoad::Terminal(display) => {
                        panic!("{label}: unexpectedly terminal: {display}")
                    }
                },
                None => assert!(
                    matches!(loaded, GitHubCopilotCredentialLoad::Absent),
                    "{label}"
                ),
            }
            std::fs::remove_dir_all(dir).unwrap();
        }

        let dir = fixture_dir("db-copilot-malformed");
        write_credential_db(
            &dir,
            "opencode.db",
            &[(
                "github-copilot",
                r#"{"type":"oauth","refresh":false,"access":"token"}"#,
                Some(1),
                10,
            )],
        );
        assert!(matches!(
            github_copilot_credential_in(&dir),
            GitHubCopilotCredentialLoad::Terminal(_)
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn database_subscriptions_skip_connectors_and_non_oauth_rows() {
        let dir = fixture_dir("db-subscriptions");
        write_credential_db(
            &dir,
            "opencode.db",
            &[
                (
                    "openai",
                    r#"{"type":"oauth","access":"token"}"#,
                    Some(1),
                    10,
                ),
                (
                    "github-copilot",
                    r#"{"type":"oauth","refresh":"token"}"#,
                    Some(1),
                    10,
                ),
                ("xai", r#"{"type":"oauth","access":"token"}"#, None, 10),
                (
                    "opencode-go",
                    r#"{"type":"key","key":"sk-go"}"#,
                    Some(1),
                    10,
                ),
                (
                    "mcp_0582fcfd783eb508",
                    r#"{"type":"oauth","access":"token"}"#,
                    Some(1),
                    10,
                ),
            ],
        );
        assert_eq!(
            detect_subscriptions_in(&dir),
            vec![
                "Codex".to_string(),
                "Copilot".to_string(),
                "Xai".to_string()
            ]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn subscriptions_fall_back_to_the_legacy_file_without_a_store() {
        let dir = fixture_dir("legacy-subscriptions");
        std::fs::write(
            dir.join("auth.json"),
            r#"{"openai":{"type":"oauth"},"anthropic":{"type":"api"},"xai":{"type":"oauth"}}"#,
        )
        .unwrap();
        assert_eq!(
            detect_subscriptions_in(&dir),
            vec!["Codex".to_string(), "Xai".to_string()]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn labels_oauth_providers_only() {
        assert_eq!(subscription_label("openai"), "Codex");
        assert_eq!(subscription_label("github-copilot"), "Copilot");
        assert_eq!(subscription_label("anthropic"), "Claude");
        assert_eq!(
            subscription_label("minimax-coding-plan"),
            "Minimax-coding-plan"
        );
    }

    #[test]
    fn opencode_dir_uses_nonempty_xdg_data_root() {
        assert_eq!(
            opencode_dir_from(Ok("configured-xdg-data".to_string()), None),
            Some(PathBuf::from("configured-xdg-data/opencode"))
        );
    }

    #[test]
    fn opencode_dir_preserves_whitespace_only_xdg_data_root() {
        assert_eq!(
            opencode_dir_from(Ok("   ".to_string()), None),
            Some(PathBuf::from("   /opencode"))
        );
    }

    #[test]
    fn opencode_dir_falls_back_for_empty_xdg_data_root() {
        let home = PathBuf::from("resolved-home");
        assert_eq!(
            opencode_dir_from(Ok(String::new()), Some(&home)),
            Some(PathBuf::from("resolved-home/.local/share/opencode"))
        );
    }

    #[test]
    fn opencode_dir_falls_back_on_environment_errors() {
        let home = PathBuf::from("resolved-home");
        let fallback = Some(PathBuf::from("resolved-home/.local/share/opencode"));
        for error in [
            std::env::VarError::NotPresent,
            std::env::VarError::NotUnicode(std::ffi::OsString::new()),
        ] {
            assert_eq!(opencode_dir_from(Err(error), Some(&home)), fallback);
        }
    }

    #[test]
    fn copilot_lineage_marker_uses_first_valid_refresh_or_access() {
        let path = std::env::temp_dir().join("fixture-opencode-auth.json");
        let cases = [
            (
                "refresh preferred",
                serde_json::json!({
                    "github-copilot": {
                        "type": "oauth",
                        "refresh": " refresh-marker ",
                        "access": "access-marker"
                    }
                }),
                Some("refresh-marker"),
            ),
            (
                "refresh missing",
                serde_json::json!({
                    "github-copilot": { "type": "oauth", "access": " access-marker " }
                }),
                Some("access-marker"),
            ),
            (
                "refresh null",
                serde_json::json!({
                    "github-copilot": {
                        "type": "oauth",
                        "refresh": null,
                        "access": "access-marker"
                    }
                }),
                Some("access-marker"),
            ),
            (
                "refresh empty",
                serde_json::json!({
                    "github-copilot": {
                        "type": "oauth",
                        "refresh": "",
                        "access": "access-marker"
                    }
                }),
                Some("access-marker"),
            ),
            (
                "refresh whitespace",
                serde_json::json!({
                    "github-copilot": {
                        "type": "oauth",
                        "refresh": " \t\n ",
                        "access": "access-marker"
                    }
                }),
                Some("access-marker"),
            ),
            (
                "refresh non-string",
                serde_json::json!({
                    "github-copilot": {
                        "type": "oauth",
                        "refresh": { "unexpected": true },
                        "access": "access-marker"
                    }
                }),
                None,
            ),
            (
                "both invalid",
                serde_json::json!({
                    "github-copilot": {
                        "type": "oauth",
                        "refresh": false,
                        "access": "   "
                    }
                }),
                None,
            ),
        ];

        for (label, json, expected) in cases {
            let credential = github_copilot_credential_from(&path, &json);
            match expected {
                Some(expected) => match credential {
                    GitHubCopilotCredentialLoad::Present(credential) => {
                        assert_eq!(credential.request_token, expected, "{label}");
                        assert_eq!(credential.marker, expected.as_bytes(), "{label}");
                    }
                    GitHubCopilotCredentialLoad::Absent => panic!("{label}: unexpectedly absent"),
                    GitHubCopilotCredentialLoad::Terminal(display) => {
                        panic!("{label}: unexpectedly terminal: {display}")
                    }
                },
                None => assert!(
                    matches!(credential, GitHubCopilotCredentialLoad::Terminal(_)),
                    "{label}"
                ),
            }
        }
    }

    #[test]
    fn copilot_loader_distinguishes_absent_from_terminal_evidence() {
        let path = std::env::temp_dir().join("fixture-opencode-auth.json");
        for (label, json) in [
            ("missing entry", serde_json::json!({})),
            (
                "non-oauth entry",
                serde_json::json!({ "github-copilot": { "type": "api" } }),
            ),
            (
                "logged out entry",
                serde_json::json!({
                    "github-copilot": {
                        "type": "oauth",
                        "refresh": null,
                        "access": "  "
                    }
                }),
            ),
        ] {
            assert!(
                matches!(
                    github_copilot_credential_from(&path, &json),
                    GitHubCopilotCredentialLoad::Absent
                ),
                "{label}"
            );
        }

        for (label, json) in [
            (
                "non-object entry",
                serde_json::json!({ "github-copilot": "oauth" }),
            ),
            (
                "oauth entry without token fields",
                serde_json::json!({ "github-copilot": { "type": "oauth" } }),
            ),
            (
                "missing type",
                serde_json::json!({ "github-copilot": { "access": "token" } }),
            ),
            (
                "malformed token sibling",
                serde_json::json!({
                    "github-copilot": {
                        "type": "oauth",
                        "refresh": false,
                        "access": "token"
                    }
                }),
            ),
        ] {
            assert!(
                matches!(
                    github_copilot_credential_from(&path, &json),
                    GitHubCopilotCredentialLoad::Terminal(_)
                ),
                "{label}"
            );
        }
    }

    #[test]
    fn copilot_file_loader_treats_missing_as_absent_and_io_or_json_as_terminal() {
        let root =
            std::env::temp_dir().join(format!("tokenbar-copilot-loader-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        assert!(matches!(
            github_copilot_credential_at(&root.join("missing.json")),
            GitHubCopilotCredentialLoad::Absent
        ));
        assert!(matches!(
            github_copilot_credential_at(&root),
            GitHubCopilotCredentialLoad::Terminal(_)
        ));

        let invalid = root.join("invalid.json");
        std::fs::write(&invalid, "not json").unwrap();
        assert!(matches!(
            github_copilot_credential_at(&invalid),
            GitHubCopilotCredentialLoad::Terminal(_)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn copilot_loader_rejects_non_utf8_canonical_location() {
        use std::os::unix::ffi::OsStringExt;

        let path = std::path::PathBuf::from(std::ffi::OsString::from_vec(vec![0xff]));
        let json = serde_json::json!({
            "github-copilot": { "type": "oauth", "access": "token" }
        });
        assert!(matches!(
            github_copilot_credential_from(&path, &json),
            GitHubCopilotCredentialLoad::Terminal(_)
        ));
    }
}
