//! `goku-engine-server`: the process the VS Code extension spawns. Protocol on stdin/stdout (LSP framing), logs on
//! stderr (the extension's output channel). Exits when the client sends `exit` or closes stdin, so it never outlives
//! the extension host.
mod framing;
mod server;
mod sessions;

use std::process::ExitCode;

fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("goku-engine-server {} (protocol {})", goku_engine::VERSION, server::PROTOCOL_VERSION);
        return ExitCode::SUCCESS;
    }
    // The one runtime for the whole process (the counterpart of GokuCore's single event-loop group).
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("goku-engine").build() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("goku-engine: can't start the runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let exit = runtime.block_on(server::run(tokio::io::BufReader::new(tokio::io::stdin()), tokio::io::stdout()));
    // Don't wait for stuck work (e.g. a driver ignoring cancellation) after the client is gone.
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    if exit == server::Exit::Clean { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

#[cfg(test)]
mod tests;
