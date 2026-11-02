use std::collections::{BTreeMap, HashMap};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use any_converter_core::convert::{
    Format, convert_request, convert_response, convert_stream_event,
};
use any_converter_core::ir::StreamState;
use any_converter_core::sse::{parse_sse_block, split_sse_blocks};
use any_converter_server::config::{
    LoggingConfig, ModelRouteConfig, PluginRef, ProviderConfig, RouteStrategy, ServerConfig,
    ServerSettings, secrets_path, warn_if_world_readable,
};
use any_converter_server::credentials::account::clear_revoked_default_store;
use any_converter_server::credentials::{
    CopilotEndpoints, CopilotSignIn, Credential, FileSecretStore, FoundAccount, GeminiAccount,
    GeminiEndpoints, GeminiSignIn, ImportStatus, SecretStore, discover,
    gemini::DEFAULT_SIGN_IN_TIMEOUT, import_path, to_credential,
};
use any_converter_server::plugins::{
    AuthMethodKind, Authorization, AuthorizeParams, CallbackParams, ManagedHosts, PluginAuthStore,
    PluginProviderInfo, ProviderPlugin,
};
use any_converter_server::{logging::init_logging, run};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Clone, ValueEnum)]
enum CliFormat {
    OpenaiChat,
    Claude,
    OpenaiResponses,
    Gemini,
}

impl CliFormat {
    #[allow(clippy::wrong_self_convention)]
    fn to_format(self) -> Format {
        match self {
            CliFormat::OpenaiChat => Format::OpenAIChat,
            CliFormat::Claude => Format::Claude,
            CliFormat::OpenaiResponses => Format::OpenAIResponses,
            CliFormat::Gemini => Format::Gemini,
        }
    }
}

/// Which tool's accounts `accounts import-agent` takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum AgentProvider {
    Codex,
    Claude,
    All,
}

impl AgentProvider {
    fn matches(self, provider: &str) -> bool {
        match self {
            AgentProvider::Codex => provider == "codex",
            AgentProvider::Claude => provider == "claude",
            AgentProvider::All => true,
        }
    }
}

#[derive(Parser)]
#[command(name = "any-converter", about = "LLM API format conversion tool")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Convert a request or response JSON between formats
    Convert {
        #[arg(long, value_enum)]
        from: CliFormat,
        #[arg(long, value_enum)]
        to: CliFormat,
        /// Input file (defaults to stdin when --stdin is set or no file given)
        input_file: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        stdin: bool,
        /// Treat input as a response body instead of a request
        #[arg(long, default_value_t = false)]
        response: bool,
    },
    /// Convert SSE stream events from stdin to stdout
    Stream {
        #[arg(long, value_enum)]
        from: CliFormat,
        #[arg(long, value_enum)]
        to: CliFormat,
    },
    /// Start the HTTP proxy server
    Serve {
        /// Path to TOML configuration file
        #[arg(long, conflicts_with_all = ["port", "provider", "format", "base_url", "upstream_key"])]
        config: Option<PathBuf>,
        #[arg(long, default_value_t = 8080)]
        port: u16,
        #[arg(long, default_value = "default")]
        provider: String,
        #[arg(long, value_enum)]
        format: Option<CliFormat>,
        #[arg(long)]
        base_url: Option<String>,
        #[arg(long)]
        upstream_key: Option<String>,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long)]
        api_key: Option<String>,
    },
    /// Manage stored provider accounts (OAuth sign-ins)
    Accounts {
        #[command(subcommand)]
        command: AccountsCommand,
    },
    /// Manage installed OpenCode plugin packages
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },
}

#[derive(Subcommand)]
enum AccountsCommand {
    /// Sign in with a provider and store the account
    Add {
        /// Provider to sign in with (built-ins: gemini, copilot)
        provider: String,
        /// Plugin spec; signs in to the plugin's provider instead of a built-in
        #[arg(long)]
        plugin: Option<String>,
        /// Provider id the plugin serves (defaults to the plugin's only provider)
        #[arg(long)]
        provider_id: Option<String>,
        /// Method index (defaults to 0)
        #[arg(long)]
        method: Option<usize>,
    },
    /// List stored accounts (never prints tokens)
    List,
    /// Remove a stored account by id
    Remove {
        /// Account id to remove (see `accounts list`)
        account_id: String,
    },
    /// Validate and preview an account order for a provider (P9)
    Order {
        /// Provider name, or an account prefix like `gemini`
        provider: String,
        /// Account ids in the desired order; they move to the front, in
        /// this order (magpie "Make first"). Does not write config.toml.
        account_ids: Vec<String>,
        /// Path to config.toml (defaults to ./config.toml)
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Query a running server for every account's quota windows
    Quotas,
    /// Discover accounts other tools keep on this machine (Codex, Claude) —
    /// prints them, never writes anything
    Discover,
    /// Import accounts found in other tools' stores into secrets.json
    ImportAgent {
        /// Which provider's accounts to import (default: all)
        #[arg(long, value_enum, default_value_t = AgentProvider::All)]
        provider: AgentProvider,
        /// Print what would be written without writing anything
        #[arg(long, default_value_t = false)]
        dry_run: bool,
    },
    /// Import accounts from another tool's export (claude-swap cswap /
    /// Sub2API / CLIProxyAPI)
    Import {
        /// Export file, or a directory of *.json export files
        path: PathBuf,
        /// Report what would be imported without writing anything
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum PluginCommand {
    /// Install a plugin package into any-converter's plugin directory
    Install {
        /// npm package spec, e.g. @magpie-community/opencode-cursor-auth
        spec: String,
    },
    /// List installed plugins and the providers/methods they serve
    List,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Convert {
            from,
            to,
            input_file,
            stdin,
            response,
        } => {
            init_logging(&LoggingConfig::default())?;

            let input = read_input(input_file.as_deref(), stdin)?;
            let from_fmt = from.to_format();
            let to_fmt = to.to_format();
            let output = if response {
                convert_response(&input, from_fmt, to_fmt)?
            } else {
                convert_request(&input, from_fmt, to_fmt)?
            };
            io::stdout().write_all(&output)?;
            if !output.ends_with(b"\n") {
                io::stdout().write_all(b"\n")?;
            }
        }
        Commands::Stream { from, to } => {
            init_logging(&LoggingConfig::default())?;

            let mut input = String::new();
            io::stdin().read_to_string(&mut input)?;
            let from_fmt = from.to_format();
            let to_fmt = to.to_format();
            let mut state_in = StreamState::default();
            let mut state_out = StreamState::default();

            for block in split_sse_blocks(&input) {
                if let Some(event) = parse_sse_block(&block) {
                    let lines = convert_stream_event(
                        &event,
                        from_fmt,
                        to_fmt,
                        &mut state_in,
                        &mut state_out,
                    )?;
                    for line in lines {
                        io::stdout().write_all(line.as_bytes())?;
                    }
                }
            }
        }
        Commands::Serve {
            config,
            port,
            provider,
            format,
            base_url,
            upstream_key,
            host,
            api_key,
        } => {
            let server_config = if let Some(path) = config {
                let content = std::fs::read_to_string(&path)?;
                let config = ServerConfig::from_toml(&content)?;
                warn_if_world_readable(&path);
                config
            } else {
                let fmt = format.ok_or("--format is required when not using --config")?;
                let base = base_url.ok_or("--base-url is required when not using --config")?;
                let key =
                    upstream_key.ok_or("--upstream-key is required when not using --config")?;
                let provider_format = fmt.to_format();

                ServerConfig {
                    server: ServerSettings {
                        host,
                        port,
                        api_key,
                        pass_through_headers: Default::default(),
                        proxy: None,
                    },
                    providers: vec![ProviderConfig {
                        name: provider.clone(),
                        format: provider_format,
                        base_url: base,
                        api_key: key,
                        account: None,
                        accounts: Vec::new(),
                        api: None,
                        wire: None,
                        model_map: HashMap::new(),
                        endpoints: Default::default(),
                        auth: Default::default(),
                        plugin: None,
                        proxy: None,
                    }],
                    model_routes: vec![ModelRouteConfig {
                        pattern: "*".into(),
                        provider: Some(provider),
                        providers: vec![],
                        upstream_model: None,
                        strategy: RouteStrategy::Priority,
                    }],
                    routes: vec![],
                    model_metadata: HashMap::new(),
                    logging: LoggingConfig::default(),
                }
            };

            let validation_errors = server_config.validate();
            if !validation_errors.is_empty() {
                #[allow(clippy::print_stderr)]
                for err in &validation_errors {
                    eprintln!("config error: {err}");
                }
                return Err(
                    format!("{} config validation error(s)", validation_errors.len()).into(),
                );
            }

            #[allow(clippy::print_stderr)]
            if !server_config.routes.is_empty() {
                eprintln!(
                    "warning: legacy [[routes]] entries are ignored; migrate them to [[model_routes]]"
                );
            }

            let log_dir = init_logging(&server_config.logging)?;
            let _log_dir = log_dir;

            run(server_config).await?;
        }
        Commands::Accounts { command } => accounts(command).await?,
        Commands::Plugin { command } => plugin(command).await?,
    }

    Ok(())
}

// ── accounts ─────────────────────────────────────────────────────────

#[allow(clippy::print_stdout)]
async fn accounts(command: AccountsCommand) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        AccountsCommand::Add {
            provider,
            plugin,
            provider_id,
            method,
        } => accounts_add(&provider, plugin.as_deref(), provider_id.as_deref(), method).await,
        AccountsCommand::List => accounts_list(),
        AccountsCommand::Remove { account_id } => accounts_remove(&account_id),
        AccountsCommand::Order {
            provider,
            account_ids,
            config,
        } => accounts_order(config.as_deref(), &provider, &account_ids),
        AccountsCommand::Quotas => accounts_quotas().await,
        AccountsCommand::Discover => accounts_discover(),
        AccountsCommand::Import { path, dry_run } => accounts_import(&path, dry_run),
        AccountsCommand::ImportAgent { provider, dry_run } => {
            accounts_import_agent(provider, dry_run)
        }
    }
}

#[allow(clippy::print_stdout)]
async fn accounts_add(
    provider: &str,
    plugin: Option<&str>,
    provider_id: Option<&str>,
    method: Option<usize>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(spec) = plugin {
        if spec.is_empty() {
            return Err("empty --plugin spec".into());
        }
        return accounts_add_plugin(spec, provider, provider_id, method).await;
    }
    let store = open_secret_store()?;
    match provider.to_ascii_lowercase().as_str() {
        "gemini" => {
            let sign_in = GeminiSignIn::new(GeminiEndpoints::default());
            let started = sign_in.start().await?;
            println!("Open this URL in your browser to sign in:");
            println!("  {}", started.url);
            println!("Waiting up to 600s for the browser callback...");
            let account = sign_in.wait(DEFAULT_SIGN_IN_TIMEOUT).await?;
            let account_id = format!("gemini:{}", account.email);
            store.put(&account_id, &gemini_credential(&account))?;
            // A fresh sign-in restores a previously revoked account.
            if let Err(err) = clear_revoked_default_store(&account_id).await {
                log::warn!("failed to clear revoked marker for {account_id}: {err}");
            }
            println!("Signed in as {} (stored as {account_id})", account.email);
            println!();
            println!("Add this to config.toml to serve it:");
            println!();
            println!("  [[providers]]");
            println!("  name = \"gemini\"");
            println!("  format = \"gemini\"");
            println!("  base_url = \"https://cloudcode-pa.googleapis.com\"");
            println!("  api_key = \"\"            # unused: the account signs the requests");
            println!("  account = \"{account_id}\"");
        }
        "copilot" => {
            let sign_in = CopilotSignIn::new(CopilotEndpoints::default());
            let started = sign_in.start().await?;
            println!("Open {} and enter this code:", started.verification_uri);
            println!("  {}", started.user_code);
            println!("Waiting up to 600s for the device flow to complete...");
            let account = sign_in.wait(&started, DEFAULT_SIGN_IN_TIMEOUT).await?;
            let account_id = format!("copilot:{}", account.user_code_hint);
            store.put(
                &account_id,
                &Credential::OAuth {
                    provider: "copilot".to_string(),
                    account: account.user_code_hint.clone(),
                    access_token: account.access_token,
                    refresh_token: None,
                    expires_at_unix: None,
                },
            )?;
            // A fresh sign-in restores a previously revoked account.
            if let Err(err) = clear_revoked_default_store(&account_id).await {
                log::warn!("failed to clear revoked marker for {account_id}: {err}");
            }
            println!("Signed in (stored as {account_id})");
            println!();
            println!("Copilot accounts are served by their plugin host; bind one to an");
            println!("any-converter provider with `account = \"{account_id}\"` in config.toml");
            println!("once a Copilot wire is available.");
        }
        _ => {
            return Err(format!(
                "invalid value '{provider}': unknown provider: {provider} \
                 (built-in ones are gemini, copilot; use --plugin for a plugin provider)"
            )
            .into());
        }
    }
    Ok(())
}

/// How long `accounts add --plugin` waits for an "auto" sign-in flow.
const PLUGIN_SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);
/// Pause between "auto" sign-in retries.
const PLUGIN_SIGN_IN_RETRY: Duration = Duration::from_secs(2);

#[allow(clippy::print_stdout)]
async fn accounts_add_plugin(
    spec: &str,
    provider: &str,
    provider_id: Option<&str>,
    method: Option<usize>,
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = plugins_dir();
    let r = plugin_ref(spec, &dir);
    let hosts = managed_hosts();
    let host = hosts.host_for(&r).await?;
    let providers = hosts.providers(&r).await?;
    if providers.is_empty() {
        return Err(format!("plugin `{spec}` serves no providers").into());
    }
    let info = pick_provider(&providers, provider_id, provider)?;
    let method = method.unwrap_or(0);
    if info.methods.is_empty() {
        return Err(format!("plugin `{spec}` provider `{}` has no auth methods", info.id).into());
    }
    if method >= info.methods.len() {
        return Err(format!(
            "plugin `{spec}` provider `{}` has no auth method {method} (available: 0..{})",
            info.id,
            info.methods.len()
        )
        .into());
    }
    let params = AuthorizeParams {
        provider: info.id.clone(),
        method,
        inputs: Some(BTreeMap::new()),
        account: "new".to_string(),
        proxy: None,
    };
    let authorization = ProviderPlugin::authorize(&*host, params.clone()).await?;
    print_authorization(&authorization);
    let result = if authorization.method == "code" {
        // The plugin wants the code the user got from the provider.
        println!("Paste the code and press Enter:");
        let mut code = String::new();
        io::stdin().read_line(&mut code)?;
        ProviderPlugin::callback(
            &*host,
            CallbackParams {
                session: authorization.session,
                code: code.trim().to_string(),
            },
        )
        .await?
    } else {
        // "auto" (and anything else): the plugin itself waits inside its
        // callback. Each callback consumes the host session, so a failed
        // attempt is retried on a freshly started flow, every 2s, for up
        // to 5 minutes.
        let deadline = Instant::now() + PLUGIN_SIGN_IN_TIMEOUT;
        let mut authorization = authorization;
        loop {
            match ProviderPlugin::callback(
                &*host,
                CallbackParams {
                    session: authorization.session.clone(),
                    code: String::new(),
                },
            )
            .await
            {
                Ok(result) => break result,
                Err(err) => {
                    if Instant::now() >= deadline {
                        return Err(format!(
                            "sign-in did not complete within {}s (last error: {err})",
                            PLUGIN_SIGN_IN_TIMEOUT.as_secs()
                        )
                        .into());
                    }
                    tokio::time::sleep(PLUGIN_SIGN_IN_RETRY).await;
                    authorization = ProviderPlugin::authorize(&*host, params.clone()).await?;
                }
            }
        }
    };
    println!("Signed in to {} as {}", result.provider, result.account);
    println!();
    println!("Add this to config.toml to serve it:");
    println!();
    println!("  [[providers]]");
    println!("  name = \"{}\"", result.provider);
    println!(
        "  plugin = {{ spec = \"{spec}\", target = \"{}\" }}",
        dir.display()
    );
    println!("  account = \"{}:{}\"", result.provider, result.account);
    Ok(())
}

/// Pick the provider `accounts add --plugin` signs in to: `requested`
/// (`--provider-id`) wins, then a single served provider, then one matching
/// the positional argument; otherwise the caller must disambiguate.
fn pick_provider<'a>(
    providers: &'a [PluginProviderInfo],
    requested: Option<&str>,
    positional: &str,
) -> Result<&'a PluginProviderInfo, Box<dyn std::error::Error>> {
    if let Some(id) = requested {
        return providers.iter().find(|p| p.id == id).ok_or_else(|| {
            format!(
                "the plugin serves no provider `{id}`; available: {}",
                provider_ids(providers)
            )
            .into()
        });
    }
    if providers.len() == 1 {
        return Ok(&providers[0]);
    }
    if let Some(matched) = providers.iter().find(|p| p.id == positional) {
        return Ok(matched);
    }
    Err(format!(
        "the plugin serves multiple providers; pick one with --provider-id: {}",
        provider_ids(providers)
    )
    .into())
}

fn provider_ids(providers: &[PluginProviderInfo]) -> String {
    providers
        .iter()
        .map(|p| p.id.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Show the details of a plugin authorization flow (the protocol has no
/// `code` field of its own, so url/instructions are all there is to print).
#[allow(clippy::print_stdout)]
fn print_authorization(authorization: &Authorization) {
    if let Some(url) = authorization.url.as_deref().filter(|url| !url.is_empty()) {
        println!("Open this URL to sign in:");
        println!("  {url}");
    }
    if let Some(instructions) = authorization
        .instructions
        .as_deref()
        .filter(|instructions| !instructions.is_empty())
    {
        println!("{instructions}");
    }
}

// ── plugin ──────────────────────────────────────────────────────────

async fn plugin(command: PluginCommand) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        PluginCommand::Install { spec } => plugin_install(&spec).await,
        PluginCommand::List => plugin_list().await,
    }
}

/// Directory plugin packages are installed into: `$ANY_CONVERTER_PLUGINS`
/// when set, otherwise `<config_dir>/plugins` (mirrors the server's
/// `config_dir`: `$APPDATA/any-converter` on Windows, `~/.config/any-converter`
/// elsewhere).
fn plugins_dir() -> PathBuf {
    if let Ok(path) = std::env::var("ANY_CONVERTER_PLUGINS") {
        return PathBuf::from(path);
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        return PathBuf::from(appdata).join("any-converter").join("plugins");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".config")
        .join("any-converter")
        .join("plugins")
}

/// The [`ManagedHosts`] the CLI drives: plugin credentials go to the same
/// `plugin-auth.json` file the server runtime uses.
fn managed_hosts() -> ManagedHosts {
    ManagedHosts::with_store(Arc::new(PluginAuthStore::new(plugin_auth_path())))
}

/// Where plugin credentials live: `plugin-auth.json` next to the account
/// store (mirrors the server's `plugin_auth_path`).
fn plugin_auth_path() -> PathBuf {
    secrets_path().with_file_name("plugin-auth.json")
}

/// [`PluginRef`] for a plugin installed into `dir`.
fn plugin_ref(spec: &str, dir: &Path) -> PluginRef {
    PluginRef {
        spec: spec.to_string(),
        target: dir.to_string_lossy().into_owned(),
    }
}

#[allow(clippy::print_stdout, clippy::print_stderr)]
async fn plugin_install(spec: &str) -> Result<(), Box<dyn std::error::Error>> {
    let dir = plugins_dir();
    ensure_plugins_dir(&dir)?;
    ensure_plugins_package_json(&dir)?;

    let output = std::process::Command::new("npm")
        .args(["install", spec, "--prefix"])
        .arg(&dir)
        .args(["--no-audit", "--no-fund"])
        .output();
    match output {
        Err(err) => {
            return Err(format!(
                "npm install could not be run: {err}\n\
                 Manual alternative: cd {} && npm install {spec}",
                dir.display()
            )
            .into());
        }
        Ok(output) if !output.status.success() => {
            let code = output
                .status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| output.status.to_string());
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "npm install {spec} failed (exit code {code}):\n\
                 {}\n\
                 Manual alternative: cd {} && npm install {spec}",
                last_lines(&stderr, 20),
                dir.display()
            )
            .into());
        }
        Ok(_) => {}
    }

    // Success: start the host once and report what the plugin serves. A
    // plugin that cannot be loaded is not an install failure.
    let hosts = managed_hosts();
    let r = plugin_ref(spec, &dir);
    match hosts.providers(&r).await {
        Ok(providers) => {
            println!("{spec} → {}", provider_ids(&providers));
        }
        Err(err) => {
            eprintln!("warning: {spec} is installed but could not be loaded: {err}");
        }
    }
    Ok(())
}

/// Create `dir` when missing, with `0700` permissions.
fn ensure_plugins_dir(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// A minimal root package so `npm install --prefix` has a project to add
/// to. Written `0600` non-executable; the content is not sensitive, the
/// mode just keeps the plugins dir uniform.
fn ensure_plugins_package_json(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let package_json = dir.join("package.json");
    if package_json.exists() {
        return Ok(());
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&package_json)?;
    file.write_all(b"{\"name\":\"any-converter-plugins\",\"private\":true}\n")?;
    Ok(())
}

/// The last `count` lines of `text` (all of them when there are fewer).
fn last_lines(text: &str, count: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let skip = lines.len().saturating_sub(count);
    lines.into_iter().skip(skip).collect::<Vec<_>>().join("\n")
}

#[allow(clippy::print_stdout)]
async fn plugin_list() -> Result<(), Box<dyn std::error::Error>> {
    let dir = plugins_dir();
    let specs = installed_specs(&dir);
    if specs.is_empty() {
        println!("No plugins installed. Try: any-converter plugin install <spec>");
        return Ok(());
    }
    let hosts = managed_hosts();
    for spec in &specs {
        println!("{spec}");
        match hosts.providers(&plugin_ref(spec, &dir)).await {
            Ok(providers) => {
                for provider in &providers {
                    let methods = provider
                        .methods
                        .iter()
                        .map(|method| {
                            let kind = match method.kind {
                                AuthMethodKind::Oauth => "oauth",
                                AuthMethodKind::Api => "api",
                            };
                            // The plugin's own label, when it names one.
                            if method.label.is_empty() {
                                kind.to_string()
                            } else {
                                format!("{kind}({})", method.label)
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    println!("  provider: {}  methods: {}", provider.id, methods);
                }
            }
            Err(err) => println!("  (failed to load: {err})"),
        }
    }
    Ok(())
}

/// Package names under `<dir>/node_modules` (scoped packages expanded one
/// level; `.bin` and non-directories skipped).
fn installed_specs(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir.join("node_modules")) else {
        return Vec::new();
    };
    let mut specs = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy().into_owned();
        if name == ".bin" {
            continue;
        }
        if name.starts_with('@') {
            let Ok(scoped) = std::fs::read_dir(entry.path()) else {
                continue;
            };
            for package in scoped.flatten() {
                if package
                    .file_type()
                    .map(|kind| kind.is_dir())
                    .unwrap_or(false)
                {
                    specs.push(format!("{name}/{}", package.file_name().to_string_lossy()));
                }
            }
        } else {
            specs.push(name);
        }
    }
    specs.sort();
    specs
}

/// `accounts import`: bring accounts in from another tool's export files
/// (claude-swap cswap / Sub2API / CLIProxyAPI), one line per account and a
/// summary of the counts last.
///
/// `--dry-run` imports into a throwaway copy of the secret store instead,
/// so "already stored" verdicts are real but nothing is written.
#[allow(clippy::print_stdout)]
fn accounts_import(path: &Path, dry_run: bool) -> Result<(), Box<dyn std::error::Error>> {
    if dry_run {
        println!("(dry run: nothing will be written)");
    }
    let store = open_secret_store()?;
    let results = if dry_run {
        let dir = std::env::temp_dir().join(format!(
            "any-converter-import-dry-run-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir)?;
        let copy = dir.join("secrets.json");
        if let Ok(bytes) = std::fs::read(store.path()) {
            std::fs::write(&copy, bytes)?;
        }
        let results = import_path(&FileSecretStore::new(copy), path);
        let _ = std::fs::remove_dir_all(&dir);
        results
    } else {
        import_path(&store, path)
    };

    let mut added = 0usize;
    let mut existing = 0usize;
    let mut refused = 0usize;
    for result in &results {
        let label = match &result.status {
            ImportStatus::Added => {
                added += 1;
                "Added".to_string()
            }
            ImportStatus::Exists(why) => {
                existing += 1;
                format!("Exists({why})")
            }
            ImportStatus::Refused(why) => {
                refused += 1;
                format!("Refused({why})")
            }
        };
        if result.account.is_empty() {
            println!("{label}  <- {}", result.file);
        } else {
            println!("{label}  {}  <- {}", result.account, result.file);
        }
    }
    println!(
        "Summary: {added} added, {existing} already stored, {refused} refused \
         ({} account row(s) in total)",
        results.len()
    );
    Ok(())
}

fn gemini_credential(account: &GeminiAccount) -> Credential {
    Credential::OAuth {
        provider: "gemini".to_string(),
        account: account.email.clone(),
        access_token: account.tokens.access_token.clone(),
        refresh_token: account.tokens.refresh_token.clone(),
        expires_at_unix: account.tokens.expires_at.and_then(instant_to_unix),
    }
}

/// `Instant` → unix seconds (round-trips through `SystemTime`; `None` when
/// the expiry already passed or the clock overflows).
fn instant_to_unix(expires_at: Instant) -> Option<i64> {
    let delta = expires_at.checked_duration_since(Instant::now())?;
    let unix = SystemTime::now().checked_add(delta)?;
    unix.duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

#[allow(clippy::print_stdout)]
fn accounts_list() -> Result<(), Box<dyn std::error::Error>> {
    let store = open_secret_store()?;
    let ids = store.list()?;
    if ids.is_empty() {
        println!("No accounts stored.");
        return Ok(());
    }
    for id in ids {
        let kind = match store.get(&id)? {
            Some(cred) => kind_label(&cred),
            None => "unknown",
        };
        println!("{id}  [{kind}]");
    }
    Ok(())
}

/// Storage tag of a credential — deliberately only the variant name, so
/// listing can never leak secret material.
fn kind_label(cred: &Credential) -> &'static str {
    match cred {
        Credential::ApiKey { .. } => "api_key",
        Credential::OAuth { .. } => "oauth",
    }
}

/// `accounts discover`: print every account other tools keep on this
/// machine (id/provider/email/plan/source). Read-only: never writes the
/// tools' files, never prints a token.
#[allow(clippy::print_stdout)]
fn accounts_discover() -> Result<(), Box<dyn std::error::Error>> {
    let found = discover(&home_dir());
    if found.is_empty() {
        println!("No accounts found in other tools' stores.");
        return Ok(());
    }
    for account in found {
        println!(
            "{}  provider={}  email={}  plan={}  source={}",
            account.account, account.provider, account.email, account.plan, account.source
        );
    }
    Ok(())
}

/// `accounts import-agent`: store the found accounts as OAuth credentials
/// in secrets.json (`0600`, via [`FileSecretStore`]). The tools' own files
/// are only ever read — writing them back is P11b.
#[allow(clippy::print_stdout)]
fn accounts_import_agent(
    provider: AgentProvider,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let found: Vec<FoundAccount> = discover(&home_dir())
        .into_iter()
        .filter(|account| provider.matches(&account.provider))
        .collect();
    if found.is_empty() {
        println!("No accounts found in other tools' stores.");
        return Ok(());
    }
    if dry_run {
        println!("Would import (--dry-run, nothing written):");
        for account in &found {
            println!("  {}  from {}", account.account, account.source);
        }
        return Ok(());
    }
    let store = open_secret_store()?;
    for account in &found {
        store.put(&account.account, &to_credential(account))?;
        println!(
            "Imported {} ({}) from {}",
            account.account, account.email, account.source
        );
    }
    Ok(())
}

/// The home folder agent stores live under: `$HOME`, else `$USERPROFILE`,
/// else the current directory.
fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

#[allow(clippy::print_stdout)]
fn accounts_remove(account_id: &str) -> Result<(), Box<dyn std::error::Error>> {
    let store = open_secret_store()?;
    if store.get(account_id)?.is_none() {
        return Err(format!("no such account: {account_id}").into());
    }
    store.delete(account_id)?;
    println!("Removed {account_id}");
    Ok(())
}

/// `accounts order`: validate that the given account ids all belong to the
/// provider and preview the order they produce (magpie "Make first": the
/// named ids move to the front in the given order, the rest keep theirs).
///
/// This command is read-only on purpose: config.toml is the user's file,
/// and writing it back belongs to the P10 config editor.
#[allow(clippy::print_stdout)]
fn accounts_order(
    config_path: Option<&Path>,
    provider: &str,
    given: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let path = config_path.unwrap_or(Path::new("config.toml"));
    let content = std::fs::read_to_string(path).map_err(|err| {
        format!(
            "cannot read {}: {err} (pass --config to name your config)",
            path.display()
        )
    })?;
    let config = ServerConfig::from_toml(&content)?;

    // `provider` is a provider name or an account prefix (`gemini`).
    let matches: Vec<&ProviderConfig> = config
        .providers
        .iter()
        .filter(|p| {
            p.name == provider
                || p.account_ids()
                    .iter()
                    .any(|id| id.split(':').next() == Some(provider))
        })
        .collect();
    let matched = match matches.len() {
        0 => {
            return Err(format!(
                "no provider named `{provider}` or serving accounts with the `{provider}:` prefix"
            )
            .into());
        }
        1 => matches[0],
        _ => {
            return Err(format!(
                "`{provider}` matches {} providers; name one: {}",
                matches.len(),
                matches
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
            .into());
        }
    };

    let available = matched.account_ids();
    if available.is_empty() {
        return Err(format!("provider `{}` has no accounts in its config", matched.name).into());
    }
    if given.is_empty() {
        return Err("no account ids given".into());
    }

    // Every given id must belong to the provider; repeats are invalid.
    let mut seen: Vec<&str> = Vec::new();
    for id in given {
        if !available.contains(&id.as_str()) {
            return Err(format!(
                "account `{id}` is not in provider `{}`'s accounts: {}",
                matched.name,
                available.join(", ")
            )
            .into());
        }
        if seen.contains(&id.as_str()) {
            return Err(format!("account `{id}` given more than once").into());
        }
        seen.push(id.as_str());
    }

    // The order the ids would produce: the named ones first (Make first),
    // the rest after them in their current order.
    let mut order: Vec<&str> = seen.clone();
    for id in &available {
        if !order.contains(id) {
            order.push(id);
        }
    }

    println!(
        "provider `{}` accounts, current order ({}):",
        matched.name,
        available.len()
    );
    for (i, id) in available.iter().enumerate() {
        println!("  {}. {id}", i + 1);
    }
    println!();
    println!(
        "validated; with `{}` made first the order becomes:",
        given[0]
    );
    for (i, id) in order.iter().enumerate() {
        println!("  {}. {id}", i + 1);
    }
    println!();
    println!(
        "This command validates the order only — it does not write {}.",
        path.display()
    );
    println!("Writing config.toml back is reserved for the P10 config editor; to apply");
    println!("the order now, set the provider's `accounts` list to:");
    println!(
        "  accounts = [{}]",
        order
            .iter()
            .map(|id| format!("\"{id}\""))
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(())
}

/// `accounts quotas`: ask a running server for `GET /v1/accounts/quotas`
/// and print the reply, pretty-printed. A hand-rolled HTTP/1.1 GET keeps
/// the CLI free of an HTTP dependency (the workspace adds none for P9).
#[allow(clippy::print_stdout)]
async fn accounts_quotas() -> Result<(), Box<dyn std::error::Error>> {
    let addr = std::env::var("ANY_CONVERTER_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    let mut stream = tokio::net::TcpStream::connect(&addr)
        .await
        .map_err(|err| format!("cannot reach the server at {addr}: {err} (is it running?)"))?;
    let request = format!(
        "GET /v1/accounts/quotas HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nAccept: application/json\r\n\r\n"
    );
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    stream.write_all(request.as_bytes()).await?;
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).await?;
    let text = String::from_utf8_lossy(&reply);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.trim())
        .unwrap_or_default();
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(quotas) => {
            println!("{}", serde_json::to_string_pretty(&quotas)?);
            Ok(())
        }
        Err(_) => Err(format!("the server at {addr} answered: {body}").into()),
    }
}

fn open_secret_store() -> Result<FileSecretStore, Box<dyn std::error::Error>> {
    Ok(FileSecretStore::new(secrets_path()))
}

fn read_input(
    input_file: Option<&std::path::Path>,
    stdin: bool,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    match input_file {
        Some(path) if !stdin => Ok(std::fs::read(path)?),
        _ => {
            let mut buf = Vec::new();
            io::stdin().read_to_end(&mut buf)?;
            Ok(buf)
        }
    }
}
