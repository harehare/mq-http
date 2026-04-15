use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Clone, Debug)]
#[command(name = "mq-http")]
#[command(about = "HTTP server for mq scripts", long_about = None)]
pub struct Args {
    /// Path to the mq script
    #[arg(value_name = "SCRIPT")]
    pub script: Option<PathBuf>,

    /// Execute mq script from string
    #[arg(short = 'c', long)]
    pub command: Option<String>,

    /// Port to listen on
    #[arg(short, long, default_value = "3000")]
    pub port: u16,

    /// Bind address
    #[arg(short, long, default_value = "127.0.0.1")]
    pub addr: String,

    /// Default output format (markdown, html, text, json)
    #[arg(short = 'F', long, default_value = "markdown")]
    pub format: String,

    /// Search modules from the directory
    #[arg(short = 'L', long = "directory")]
    pub module_directories: Option<Vec<PathBuf>>,

    /// Sets string that can be referenced at runtime
    #[arg(long, value_names = ["NAME", "VALUE"])]
    pub args: Option<Vec<String>>,

    /// Sets file contents that can be referenced at runtime
    #[arg(long = "rawfile", value_names = ["NAME", "FILE"])]
    pub raw_file: Option<Vec<String>>,

    /// Automatically reload the script when it changes
    #[arg(short, long)]
    pub reload: bool,

    /// OpenTelemetry OTLP endpoint (e.g., http://localhost:4318)
    #[arg(long, env = "OTEL_EXPORTER_OTLP_ENDPOINT")]
    pub otel_endpoint: Option<String>,

    /// OpenTelemetry service name
    #[arg(long, default_value = "mq-http", env = "OTEL_SERVICE_NAME")]
    pub otel_service_name: String,

    /// Path to TLS certificate file (PEM)
    #[arg(long)]
    pub tls_cert: Option<PathBuf>,

    /// Path to TLS private key file (PEM)
    #[arg(long)]
    pub tls_key: Option<PathBuf>,

    /// Read the mq script from stdin
    #[arg(long)]
    pub stdin: bool,
}
