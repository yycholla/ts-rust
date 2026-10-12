use std::io::{Read, Write};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--help") {
        println!(
            "Usage: svelte-bundle < resolved-request.json > artifacts.json\nNative Rust Svelte/Rolldown bundling. Reads a resolved graph; emits JSON artifacts without writing build files or executing JavaScript configuration."
        );
        return Ok(());
    }
    let mut input = Vec::new();
    std::io::stdin()
        .take(ts_svelte_bundle::MAX_REQUEST_BYTES as u64 + 1)
        .read_to_end(&mut input)?;
    let response = if input.len() > ts_svelte_bundle::MAX_REQUEST_BYTES {
        serde_json::json!({"ok":false,"error":{"code":"size_limit","message":"Bundle request exceeds 16 MiB"}})
    } else {
        match serde_json::from_slice::<ts_svelte_bundle::BundleRequest>(&input) {
            Ok(request) => match ts_svelte_bundle::generate(request).await {
                Ok(result) => serde_json::json!({"ok":true,"result":result}),
                Err(error) => serde_json::json!({"ok":false,"error":error}),
            },
            Err(error) => {
                serde_json::json!({"ok":false,"error":{"code":"invalid_request","message":error.to_string()}})
            }
        }
    };
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{response}")?;
    stdout.flush()?;
    if response["ok"] != true {
        std::process::exit(1);
    }
    Ok(())
}
