use std::process::ExitCode;

use mcp_server::transport::http::{HttpConfig, HttpServer};
use mcp_server::Server;

const USAGE: &str = "\
mcp-server-rs - Model Context Protocol server

USAGE:
    mcp-server-rs [stdio]
    mcp-server-rs http [--bind ADDR] [--allow-origin ORIGIN]...

OPTIONS:
    --bind ADDR            HTTP listen address (default 127.0.0.1:8808)
    --allow-origin ORIGIN  extra allowed Origin (loopback is always allowed)
    -h, --help             print this help
    -V, --version          print version
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    #[cfg(target_os = "linux")]
    if args.first().map(String::as_str) == Some(mcp_server::sandbox::WORKER_ARG) {
        let job = args.get(1).map(String::as_str).unwrap_or("");
        return ExitCode::from(mcp_server::sandbox::worker_main(job) as u8);
    }

    match args.first().map(String::as_str) {
        None | Some("stdio") => {
            let stdin = std::io::stdin().lock();
            let stdout = std::io::stdout().lock();
            match mcp_server::transport::stdio::serve(&Server::new(), stdin, stdout) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("stdio transport error: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("http") => run_http(&args[1..]),
        Some("-h" | "--help") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("-V" | "--version") => {
            println!("mcp-server-rs {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("unknown argument '{other}'\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn run_http(args: &[String]) -> ExitCode {
    let mut bind = "127.0.0.1:8808".to_string();
    let mut cfg = HttpConfig::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match (a.as_str(), it.next()) {
            ("--bind", Some(v)) => bind = v.clone(),
            ("--allow-origin", Some(v)) => cfg.allowed_origins.push(v.clone()),
            _ => {
                eprintln!("invalid HTTP arguments\n\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let server = match HttpServer::bind(&bind, Server::new(), cfg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot bind {bind}: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "mcp-server-rs listening on http://{}/mcp",
        server.local_addr()
    );
    server.run();
    ExitCode::SUCCESS
}
