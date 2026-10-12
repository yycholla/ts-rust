use std::io::{BufRead, Read, Write};

fn main() -> std::io::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--help") {
        println!(
            "Usage: svelte-tool < request.json\nExperimental Rust Svelte compiler. Operations: capabilities, compile, project."
        );
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("--serve") {
        let mut reader = std::io::stdin().lock();
        let mut stdout = std::io::stdout().lock();
        loop {
            let mut input = Vec::new();
            if reader
                .by_ref()
                .take(ts_svelte::protocol::MAX_REQUEST_BYTES as u64 + 1)
                .read_until(b'\n', &mut input)?
                == 0
            {
                break;
            }
            if input.len() > ts_svelte::protocol::MAX_REQUEST_BYTES {
                return Err(std::io::Error::other("request too large"));
            }
            let response = ts_svelte_compiler::dispatch(&input);
            writeln!(stdout, "{response}")?;
            stdout.flush()?;
        }
        return Ok(());
    }
    let mut input = Vec::new();
    std::io::stdin()
        .take(ts_svelte::protocol::MAX_REQUEST_BYTES as u64 + 1)
        .read_to_end(&mut input)?;
    let response = ts_svelte_compiler::dispatch(&input);
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{response}")?;
    stdout.flush()?;
    if response["ok"] != true {
        std::process::exit(1);
    }
    Ok(())
}
