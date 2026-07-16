mod logging;

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::PathBuf;

use any_converter_core::convert::{
    Format, convert_request, convert_response, convert_stream_event,
};
use any_converter_core::ir::StreamState;
use any_converter_core::sse::{parse_sse_block, split_sse_blocks};
use any_converter_server::config::{
    LoggingConfig, ProviderConfig, RouteConfig, ServerConfig, ServerSettings,
};
use any_converter_server::run;
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
            logging::init_logging(&LoggingConfig::default())?;

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
            logging::init_logging(&LoggingConfig::default())?;

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
                let content = std::fs::read_to_string(path)?;
                ServerConfig::from_toml(&content)?
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
                    },
                    providers: vec![ProviderConfig {
                        name: provider.clone(),
                        format: provider_format,
                        base_url: base,
                        api_key: key,
                        model_map: HashMap::new(),
                        endpoints: Default::default(),
                        auth: Default::default(),
                    }],
                    model_routes: vec![],
                    routes: vec![RouteConfig {
                        client_format: provider_format,
                        provider,
                    }],
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

            let log_dir = logging::init_logging(&server_config.logging)?;
            let _log_dir = log_dir;

            run(server_config).await?;
        }
    }

    Ok(())
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
