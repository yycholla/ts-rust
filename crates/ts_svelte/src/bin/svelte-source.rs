//! One JSON request on stdin, one JSON response on stdout. No project writes.

use std::io::{Read, Write};
use ts_svelte::protocol::{MAX_REQUEST_BYTES, Response, dispatch};

fn main() -> std::io::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--help") {
        println!(
            "Usage: svelte-source < request.json\nSource mapping prototype, not a Svelte checker/compiler. Operations: capabilities, locate, edit, project."
        );
        return Ok(());
    }
    let mut input = Vec::new();
    std::io::stdin()
        .take(MAX_REQUEST_BYTES as u64 + 1)
        .read_to_end(&mut input)?;
    let response = dispatch(&input);
    let failed = matches!(response, Response::Failure { .. });
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    serde_json::to_writer(&mut writer, &response)?;
    writeln!(writer)?;
    writer.flush()?;
    if failed {
        std::process::exit(1);
    }
    Ok(())
}
