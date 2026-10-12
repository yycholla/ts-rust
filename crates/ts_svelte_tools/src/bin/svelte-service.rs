use std::io::{self, BufRead, Read, Write};
fn main() -> io::Result<()> {
    if std::env::args()
        .nth(1)
        .is_some_and(|arg| arg == "--help" || arg == "help")
    {
        println!(
            "Usage: svelte-service\nPersistent newline-JSON lint and document-session service over stdin/stdout."
        );
        return Ok(());
    }
    let mut session = ts_svelte_tools::session::Session::default();
    let mut reader = io::stdin().lock();
    let mut writer = io::stdout().lock();
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
            return Err(io::Error::other("Request too large"));
        }
        let op = serde_json::from_slice::<serde_json::Value>(&input).ok();
        let response = if op
            .as_ref()
            .and_then(|v| v["op"].as_str())
            .is_some_and(|o| o.starts_with("session"))
        {
            session.request(&input)
        } else {
            ts_svelte_tools::dispatch(&input)
        };
        writeln!(writer, "{response}")?;
        writer.flush()?;
    }
    Ok(())
}
