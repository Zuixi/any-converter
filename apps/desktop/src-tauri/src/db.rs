use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use any_converter_core::convert::Format;
use any_converter_server::config::{
    ConsoleConfig, DEFAULT_PORT, LogFileConfig, LoggingConfig, ModelRouteConfig, ProviderConfig,
    RouteStrategy, ServerConfig, ServerSettings,
};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid format: {0}")]
    InvalidFormat(String),
    #[error("invalid route strategy: {0}")]
    InvalidRouteStrategy(String),
    #[error("invalid setting value for {key}: {value}")]
    InvalidSetting { key: String, value: String },
    #[error("database mutex poisoned")]
    MutexPoisoned,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopProvider {
    pub id: i64,
    pub name: String,
    pub format: String,
    pub base_url: String,
    pub keychain_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopModelRoute {
    pub id: i64,
    pub pattern: String,
    pub providers: Vec<String>,
    pub upstream_model: Option<String>,
    pub strategy: String,
}

#[derive(Debug, Clone)]
struct StoredModelRoute {
    id: i64,
    pattern: String,
    provider_ids: Vec<i64>,
    upstream_model: Option<String>,
    strategy: RouteStrategy,
}

#[derive(Clone)]
pub struct DesktopDb {
    conn: Arc<Mutex<Connection>>,
}

impl DesktopDb {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DbError> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)
                .map_err(|_| rusqlite::Error::InvalidPath(parent.to_path_buf()))?;
        }
        let conn = Connection::open(path)?;
        initialize_schema(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn upsert_setting(&self, key: &str, value: &str) -> Result<(), DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::MutexPoisoned)?;
        conn.execute(
            "insert into app_settings (key, value) values (?1, ?2) on conflict(key) do update set value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn delete_setting(&self, key: &str) -> Result<(), DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::MutexPoisoned)?;
        conn.execute("delete from app_settings where key = ?1", params![key])?;
        Ok(())
    }

    pub fn settings(&self) -> Result<HashMap<String, String>, DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::MutexPoisoned)?;
        let mut stmt = conn.prepare("select key, value from app_settings order by key asc")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut settings = HashMap::new();
        for row in rows {
            let (key, value) = row?;
            settings.insert(key, value);
        }
        Ok(settings)
    }

    pub fn create_provider(
        &self,
        name: &str,
        format: Format,
        base_url: &str,
        keychain_ref: &str,
    ) -> Result<i64, DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::MutexPoisoned)?;
        conn.execute(
            "insert into providers (name, format, base_url, keychain_ref) values (?1, ?2, ?3, ?4)",
            params![name, format.to_string(), base_url, keychain_ref],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn delete_provider(&self, id: i64) -> Result<(), DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::MutexPoisoned)?;
        conn.execute("delete from providers where id = ?1", [id])?;
        Ok(())
    }

    pub fn list_providers(&self) -> Result<Vec<DesktopProvider>, DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::MutexPoisoned)?;
        list_providers_locked(&conn)
    }

    pub fn upsert_model_map(
        &self,
        provider_id: i64,
        client_model: &str,
        upstream_model: &str,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::MutexPoisoned)?;
        conn.execute(
            r#"
            insert into model_maps (provider_id, client_model, upstream_model)
            values (?1, ?2, ?3)
            on conflict(provider_id, client_model) do update set upstream_model = excluded.upstream_model
            "#,
            params![provider_id, client_model, upstream_model],
        )?;
        Ok(())
    }

    pub fn create_model_route(
        &self,
        pattern: &str,
        provider_ids: Vec<i64>,
        upstream_model: Option<&str>,
        strategy: &str,
    ) -> Result<i64, DbError> {
        let strategy = parse_strategy(strategy)?;
        let conn = self.conn.lock().map_err(|_| DbError::MutexPoisoned)?;
        conn.execute(
            "insert into model_routes (pattern, upstream_model, strategy) values (?1, ?2, ?3)",
            params![pattern, upstream_model, route_strategy_to_str(&strategy)],
        )?;
        let route_id = conn.last_insert_rowid();
        for (position, provider_id) in provider_ids.iter().enumerate() {
            conn.execute(
                "insert into model_route_providers (route_id, provider_id, position) values (?1, ?2, ?3)",
                params![route_id, provider_id, i64::try_from(position).unwrap_or(i64::MAX)],
            )?;
        }
        Ok(route_id)
    }

    pub fn list_model_routes(&self) -> Result<Vec<DesktopModelRoute>, DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::MutexPoisoned)?;
        let providers = list_providers_locked(&conn)?
            .into_iter()
            .map(|provider| (provider.id, provider.name))
            .collect::<HashMap<_, _>>();
        let routes = list_stored_model_routes_locked(&conn)?;
        Ok(routes
            .into_iter()
            .map(|route| DesktopModelRoute {
                id: route.id,
                pattern: route.pattern,
                providers: route
                    .provider_ids
                    .into_iter()
                    .filter_map(|id| providers.get(&id).cloned())
                    .collect(),
                upstream_model: route.upstream_model,
                strategy: route_strategy_to_str(&route.strategy).to_string(),
            })
            .collect())
    }

    pub fn build_server_config(&self, log_dir: impl AsRef<Path>) -> Result<ServerConfig, DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::MutexPoisoned)?;
        let settings = settings_locked(&conn)?;
        let providers = list_providers_locked(&conn)?;
        let provider_by_id = providers
            .iter()
            .map(|provider| (provider.id, provider.clone()))
            .collect::<HashMap<_, _>>();
        let mut model_maps = model_maps_locked(&conn)?;
        let provider_configs = providers
            .into_iter()
            .map(|provider| {
                Ok(ProviderConfig {
                    name: provider.name.clone(),
                    format: parse_format(&provider.format)?,
                    base_url: provider.base_url,
                    api_key: provider.keychain_ref,
                    account: None,
                    accounts: Vec::new(),
                    api: None,
                    wire: None,
                    model_map: model_maps.remove(&provider.id).unwrap_or_default(),
                    endpoints: Default::default(),
                    auth: Default::default(),
                    plugin: None,
                    proxy: None,
                })
            })
            .collect::<Result<Vec<_>, DbError>>()?;
        let model_routes = list_stored_model_routes_locked(&conn)?
            .into_iter()
            .map(|route| ModelRouteConfig {
                pattern: route.pattern,
                provider: None,
                providers: route
                    .provider_ids
                    .iter()
                    .filter_map(|id| provider_by_id.get(id).map(|provider| provider.name.clone()))
                    .collect(),
                upstream_model: route.upstream_model,
                strategy: route.strategy,
            })
            .collect();
        let logging = LoggingConfig {
            level: if cfg!(debug_assertions) {
                "debug".to_string()
            } else {
                "warn".to_string()
            },
            dir: Some(log_dir.as_ref().to_string_lossy().to_string()),
            max_disk_mb: parse_u64_setting(&settings, "logging.max_disk_mb", 500)?,
            console: ConsoleConfig {
                enabled: cfg!(debug_assertions),
                output: "stderr".to_string(),
                level: None,
                format: "pretty".to_string(),
            },
            files: vec![LogFileConfig {
                name: "app".to_string(),
                level: Some("warn".to_string()),
                target: None,
                format: "json".to_string(),
                rotation: "daily".to_string(),
            }],
            request_log: any_converter_server::config::RequestLogConfig {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        };

        Ok(ServerConfig {
            server: ServerSettings {
                host: settings
                    .get("server.host")
                    .cloned()
                    .unwrap_or_else(|| "127.0.0.1".to_string()),
                port: parse_u16_setting(&settings, "server.port", DEFAULT_PORT)?,
                api_key: settings.get("server.api_key").cloned(),
                pass_through_headers: Default::default(),
                proxy: settings.get("server.proxy").cloned(),
            },
            providers: provider_configs,
            model_routes,
            routes: Vec::new(),
            model_metadata: HashMap::new(),
            logging,
            pricing: Default::default(),
        })
    }
}

fn initialize_schema(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        r#"
        pragma journal_mode = wal;
        pragma synchronous = normal;
        pragma foreign_keys = on;

        create table if not exists app_settings (
            key text primary key,
            value text not null
        );

        create table if not exists providers (
            id integer primary key autoincrement,
            name text not null unique,
            format text not null,
            base_url text not null,
            keychain_ref text not null,
            created_at text not null default current_timestamp,
            updated_at text not null default current_timestamp
        );

        create table if not exists model_maps (
            id integer primary key autoincrement,
            provider_id integer not null references providers(id) on delete cascade,
            client_model text not null,
            upstream_model text not null,
            unique(provider_id, client_model)
        );

        create table if not exists model_routes (
            id integer primary key autoincrement,
            pattern text not null,
            upstream_model text,
            strategy text not null default 'priority',
            created_at text not null default current_timestamp
        );

        create table if not exists model_route_providers (
            route_id integer not null references model_routes(id) on delete cascade,
            provider_id integer not null references providers(id) on delete cascade,
            position integer not null,
            primary key(route_id, provider_id)
        );

        -- `request_logs` is owned by any_converter_server::storage (migrated via user_version), not here.
        "#,
    )
}

fn settings_locked(conn: &Connection) -> Result<HashMap<String, String>, DbError> {
    let mut stmt = conn.prepare("select key, value from app_settings")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut settings = HashMap::new();
    for row in rows {
        let (key, value) = row?;
        settings.insert(key, value);
    }
    Ok(settings)
}

fn list_providers_locked(conn: &Connection) -> Result<Vec<DesktopProvider>, DbError> {
    let mut stmt = conn.prepare(
        "select id, name, format, base_url, keychain_ref from providers order by name asc",
    )?;
    let rows = stmt.query_map([], |row| {
        let format_text = row.get::<_, String>(2)?;
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            format_text,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut providers = Vec::new();
    for row in rows {
        let (id, name, format, base_url, keychain_ref) = row?;
        providers.push(DesktopProvider {
            id,
            name,
            format,
            base_url,
            keychain_ref,
        });
    }
    Ok(providers)
}

fn model_maps_locked(conn: &Connection) -> Result<HashMap<i64, HashMap<String, String>>, DbError> {
    let mut stmt =
        conn.prepare("select provider_id, client_model, upstream_model from model_maps")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut maps: HashMap<i64, HashMap<String, String>> = HashMap::new();
    for row in rows {
        let (provider_id, client_model, upstream_model) = row?;
        maps.entry(provider_id)
            .or_default()
            .insert(client_model, upstream_model);
    }
    Ok(maps)
}

fn list_stored_model_routes_locked(conn: &Connection) -> Result<Vec<StoredModelRoute>, DbError> {
    let mut stmt =
        conn.prepare("select id, pattern, upstream_model, strategy from model_routes order by id")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut routes = Vec::new();
    for row in rows {
        let (id, pattern, upstream_model, strategy) = row?;
        let mut provider_stmt = conn.prepare(
            "select provider_id from model_route_providers where route_id = ?1 order by position asc",
        )?;
        let provider_rows = provider_stmt.query_map([id], |row| row.get::<_, i64>(0))?;
        let mut provider_ids = Vec::new();
        for provider_row in provider_rows {
            provider_ids.push(provider_row?);
        }
        routes.push(StoredModelRoute {
            id,
            pattern,
            provider_ids,
            upstream_model,
            strategy: parse_strategy(&strategy)?,
        });
    }
    Ok(routes)
}

fn parse_format(value: &str) -> Result<Format, DbError> {
    match value {
        "openai_chat" => Ok(Format::OpenAIChat),
        "openai_responses" => Ok(Format::OpenAIResponses),
        "claude" => Ok(Format::Claude),
        "gemini" => Ok(Format::Gemini),
        other => Err(DbError::InvalidFormat(other.to_string())),
    }
}

fn parse_strategy(value: &str) -> Result<RouteStrategy, DbError> {
    match value {
        "priority" => Ok(RouteStrategy::Priority),
        "round_robin" => Ok(RouteStrategy::RoundRobin),
        other => Err(DbError::InvalidRouteStrategy(other.to_string())),
    }
}

fn route_strategy_to_str(value: &RouteStrategy) -> &'static str {
    match value {
        RouteStrategy::Priority => "priority",
        RouteStrategy::RoundRobin => "round_robin",
    }
}

fn parse_u16_setting(
    settings: &HashMap<String, String>,
    key: &str,
    default: u16,
) -> Result<u16, DbError> {
    settings
        .get(key)
        .map(|value| {
            value.parse::<u16>().map_err(|_| DbError::InvalidSetting {
                key: key.to_string(),
                value: value.clone(),
            })
        })
        .unwrap_or(Ok(default))
}

fn parse_u64_setting(
    settings: &HashMap<String, String>,
    key: &str,
    default: u64,
) -> Result<u64, DbError> {
    settings
        .get(key)
        .map(|value| {
            value.parse::<u64>().map_err(|_| DbError::InvalidSetting {
                key: key.to_string(),
                value: value.clone(),
            })
        })
        .unwrap_or(Ok(default))
}
