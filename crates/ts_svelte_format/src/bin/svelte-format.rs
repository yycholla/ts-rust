#[cfg(not(target_arch = "wasm32"))]
fn main() -> std::io::Result<()> {
    use std::io::{BufRead, Read, Write};
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["--help"] {
        println!(
            "Usage: svelte-format [--serve] < request.json\nPure Rust Svelte/JS/TS/CSS/JSON formatting. Operations: capabilities, format.\n--serve reads one JSON request per line and writes one response per line.\nSource/config are supplied in JSON; no input file is read or overwritten."
        );
        return Ok(());
    }
    if args == ["--serve"] {
        let mut input = std::io::stdin().lock();
        let mut output = std::io::stdout().lock();
        loop {
            let mut request = Vec::new();
            if input
                .by_ref()
                .take(ts_svelte_format::MAX_REQUEST_BYTES as u64 + 1)
                .read_until(b'\n', &mut request)?
                == 0
            {
                break;
            }
            let response = ts_svelte_format::dispatch(&request);
            writeln!(output, "{response}")?;
            output.flush()?;
            // An oversized line has unread bytes; closing avoids treating its
            // remainder as a new request and desynchronizing the stream.
            if request.len() > ts_svelte_format::MAX_REQUEST_BYTES {
                return Err(std::io::Error::other("Request exceeds the protocol limit"));
            }
        }
        return Ok(());
    }
    if !args.is_empty() {
        return Err(std::io::Error::other("Unknown arguments; use --help"));
    }
    let mut request = Vec::new();
    std::io::stdin()
        .take(ts_svelte_format::MAX_REQUEST_BYTES as u64 + 1)
        .read_to_end(&mut request)?;
    let response = ts_svelte_format::dispatch(&request);
    let mut output = std::io::stdout().lock();
    writeln!(output, "{response}")?;
    output.flush()?;
    if response["ok"] != true {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn main() {}
