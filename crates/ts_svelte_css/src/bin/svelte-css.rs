use std::io::{BufRead, Read, Write};
fn main() -> std::io::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--help"] {
        println!(
            "Usage: svelte-css [--serve] < request.json\nOperations: capabilities, bundle, link. All sources and resolutions supplied in JSON.\n--serve uses newline-delimited requests/responses. No source file writes."
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
                .take(ts_svelte_css::MAX_REQUEST_BYTES as u64 + 1)
                .read_until(b'\n', &mut request)?
                == 0
            {
                break;
            }
            writeln!(output, "{}", ts_svelte_css::dispatch(&request))?;
            output.flush()?;
            if request.len() > ts_svelte_css::MAX_REQUEST_BYTES {
                return Err(std::io::Error::other("Request limit exceeded"));
            }
        }
        return Ok(());
    }
    if !args.is_empty() {
        return Err(std::io::Error::other("Unknown arguments; use --help"));
    }
    let mut request = Vec::new();
    std::io::stdin()
        .take(ts_svelte_css::MAX_REQUEST_BYTES as u64 + 1)
        .read_to_end(&mut request)?;
    let response = ts_svelte_css::dispatch(&request);
    let mut output = std::io::stdout().lock();
    writeln!(output, "{response}")?;
    output.flush()?;
    if response["ok"] != true {
        std::process::exit(1);
    }
    Ok(())
}
