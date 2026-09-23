//! Listen on this machine. The hub connects here; this process does not dial out.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;
use tokio::net::TcpListener;

use picrew_agent::{router, Agent};

#[derive(Parser, Debug)]
#[command(name = "picrew-agent", about = "Listen for the PiCrew hub. Does not phone home.")]
struct Cli {
    /// Bind address.
    #[arg(long, default_value = "0.0.0.0")]
    host: String,
    /// Port the hub dials.
    #[arg(long, env = "PICREW_AGENT_PORT", default_value_t = 5280)]
    port: u16,
    /// Shared token the hub presents.
    #[arg(long, env = "PICREW_TOKEN")]
    token: String,
    /// Name this box reports.
    #[arg(long, env = "PICREW_AGENT_NAME")]
    name: Option<String>,
    /// Local OpenCode.
    #[arg(long, env = "OPENCODE_URL", default_value = "http://127.0.0.1:4096")]
    opencode: String,
    /// Directory whose children are repos.
    #[arg(long, env = "PICREW_PROJECTS", default_value = "/srv/projects")]
    projects: PathBuf,
    #[arg(long, env = "PICREW_TLS_CERT")]
    tls_cert: Option<PathBuf>,
    #[arg(long, env = "PICREW_TLS_KEY")]
    tls_key: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "picrew_agent=info".into()),
        )
        .init();

    let cli = Cli::parse();
    if cli.token.is_empty() {
        eprintln!("picrew-agent: PICREW_TOKEN is required");
        std::process::exit(2);
    }
    let name = cli.name.unwrap_or_else(|| {
        std::env::var("HOSTNAME").unwrap_or_else(|_| "box".into())
    });
    let _ = std::fs::create_dir_all(&cli.projects);
    let app = router(Agent {
        name: name.clone(),
        token: cli.token,
        opencode: cli.opencode.trim_end_matches('/').to_string(),
        projects: cli.projects,
    });
    let addr: SocketAddr = format!("{}:{}", cli.host, cli.port)
        .parse()
        .expect("bind address");
    tracing::info!("picrew-agent {name} {} listening on {addr} (hub connects here)", picrew_agent::VERSION);
    tokio::spawn(picrew_agent::auto_update_loop());
    let _ = rustls::crypto::ring::default_provider().install_default();
    let tls = match (cli.tls_cert, cli.tls_key) {
        (Some(cert), Some(key)) if cert.is_file() && key.is_file() => Some((cert, key)),
        (Some(cert), Some(key)) => {
            tracing::error!("TLS files missing: {} / {}", cert.display(), key.display());
            std::process::exit(2);
        }
        _ => None,
    };
    if let Some((cert, key)) = tls {
        let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key)
            .await
            .expect("tls cert");
        tracing::info!("TLS on");
        axum_server::bind_rustls(addr, config)
            .serve(app.into_make_service())
            .await
            .expect("serve tls");
    } else {
        let listener = TcpListener::bind(addr).await.expect("bind");
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await
            .expect("serve");
    }
}
