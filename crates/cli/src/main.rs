use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use any_converter_core::convert::{
    Format, convert_request, convert_response, convert_stream_event,
};
use any_converter_core::ir::StreamState;
use any_converter_core::sse::{parse_sse_block, split_sse_blocks};
use any_converter_server::config::{
    LoggingConfig, ModelRouteConfig, ProviderConfig, RouteStrategy, ServerConfig, ServerSettings,
    secrets_path, warn_if_world_readable,
};
use any_converter_server::credentials::account::clear_revoked_default_store;
use any_converter_server::credentials::{
    CopilotEndpoints, CopilotSignIn, Credential, FileSecretStore, GeminiAccount, GeminiEndpoints,
    GeminiSignIn, SecretStore, gemini::DEFAULT_SIGN_IN_TIMEOUT,
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
}

#[derive(Subcommand)]
enum AccountsCommand {
    /// Sign in with a provider and store the account
    Add {
        /// Provider to sign in with
        provider: AccountsProvider,
    },
    /// List stored accounts (never prints tokens)
    List,
    /// Remove a stored account by id
    Remove {
        /// Account id to remove (see `accounts list`)
        account_id: String,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum AccountsProvider {
    Gemini,
    Copilot,
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
                    },
                    providers: vec![ProviderConfig {
                        name: provider.clone(),
                        format: provider_format,
                        base_url: base,
                        api_key: key,
                        account: None,
                        wire: None,
                        model_map: HashMap::new(),
                        endpoints: Default::default(),
                        auth: Default::default(),
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
    }

    Ok(())
}

// ── accounts ─────────────────────────────────────────────────────────

#[allow(clippy::print_stdout)]
async fn accounts(command: AccountsCommand) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        AccountsCommand::Add { provider } => accounts_add(provider).await,
        AccountsCommand::List => accounts_list(),
        AccountsCommand::Remove { account_id } => accounts_remove(&account_id),
    }
}

#[allow(clippy::print_stdout)]
async fn accounts_add(provider: AccountsProvider) -> Result<(), Box<dyn std::error::Error>> {
    let store = open_secret_store()?;
    match provider {
        AccountsProvider::Gemini => {
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
        }
        AccountsProvider::Copilot => {
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
        }
    }
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
