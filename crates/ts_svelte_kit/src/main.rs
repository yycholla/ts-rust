use std::path::PathBuf;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|s| s == "--help") {
        println!(
            "Usage: ts_svelte_kit PROJECT NODE LOADER NATIVE_COMPILER TRACE\nFull hybrid Kit build with native Svelte compilation. Requires prior Kit sync; JavaScript handles framework orchestration, preprocessing and bundling."
        );
        return Ok(());
    }
    if args.len() != 5 {
        return Err("Expected PROJECT NODE LOADER NATIVE_COMPILER TRACE; use --help".into());
    }
    let project = PathBuf::from(&args[0]).canonicalize()?;
    let package: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
        project.join("node_modules/svelte/package.json"),
    )?)?;
    let host = ts_svelte_kit::BuildHost {
        node: PathBuf::from(&args[1]),
        loader: PathBuf::from(&args[2]).canonicalize()?,
        compiler: PathBuf::from(&args[3]).canonicalize()?,
        svelte_version: package["version"]
            .as_str()
            .ok_or("Missing installed version")?
            .to_owned(),
    };
    let status = host.build(&project, &PathBuf::from(&args[4]))?;
    std::process::exit(status.code().unwrap_or(1));
}
