use std::io::{self, BufRead, Read, Write};
use ts_tailwind::protocol::{MAX_REQUEST_BYTES, Service};

fn main() -> io::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "help" | "--help"))
    {
        println!(
            "tailwind [--serve]\nRead one JSON request, or persistent newline-delimited requests with --serve.\nOperations: capabilities, prepare, build, scan, dispose.\nCSS inputs and plugin registrations are explicit; native Rust compilation."
        );
        return Ok(());
    }
    if !args.is_empty() && args != ["--serve"] {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Use tailwind --help",
        ));
    }
    let persistent = !args.is_empty();
    let mut service = Service::default();
    let mut input = io::stdin().lock();
    let mut output = io::BufWriter::new(io::stdout().lock());
    loop {
        let mut bytes = Vec::new();
        if persistent {
            (&mut input)
                .take((MAX_REQUEST_BYTES + 1) as u64)
                .read_until(b'\n', &mut bytes)?;
        } else {
            (&mut input)
                .take((MAX_REQUEST_BYTES + 1) as u64)
                .read_to_end(&mut bytes)?;
        }
        if bytes.is_empty() {
            break;
        }
        if bytes.len() > MAX_REQUEST_BYTES {
            writeln!(
                output,
                "{{\"ok\":false,\"error\":{{\"code\":\"request_limit\",\"message\":\"Tailwind request exceeds 16 MiB\"}}}}"
            )?;
            output.flush()?;
            std::process::exit(2);
        }
        let response = match std::str::from_utf8(&bytes) {
            Ok(text) => service.request_json(text),
            Err(_) => "{\"ok\":false,\"error\":{\"code\":\"invalid_utf8\",\"message\":\"Request must be UTF-8\"}}".into(),
        };
        writeln!(output, "{response}")?;
        output.flush()?;
        if !persistent {
            if serde_json::from_str::<serde_json::Value>(&response)
                .ok()
                .is_none_or(|value| value["ok"] != true)
            {
                std::process::exit(1);
            }
            break;
        }
    }
    Ok(())
}
