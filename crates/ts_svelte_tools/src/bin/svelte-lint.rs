//! Deterministic native project lint; never loads executable configuration.
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
fn collect(path: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if path.symlink_metadata()?.file_type().is_symlink() {
        return Ok(());
    }
    if path.is_dir() {
        let mut entries = std::fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            if matches!(
                entry.file_name().to_str(),
                Some("node_modules" | ".git" | ".svelte-kit" | "build" | "dist" | "target")
            ) {
                continue;
            }
            collect(&entry.path(), files)?;
        }
    } else if path.extension().is_some_and(|e| {
        matches!(
            e.to_str(),
            Some("svelte" | "js" | "ts" | "mjs" | "cjs" | "jsx" | "tsx" | "mts" | "cts")
        )
    }) {
        files.push(path.to_owned());
    }
    Ok(())
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let Some(root) = args.next() else {
        return Err(
            "Usage: svelte-lint PATH [SVELTE_VERSION] [--runes|--legacy] [--config RULES_JSON]"
                .into(),
        );
    };
    if root == "--help" {
        println!(
            "Usage: svelte-lint PATH [SVELTE_VERSION] [--runes|--legacy] [--config RULES_JSON]\nNative compiler and configurable script/template lint. --config reads resolved JSON rule settings, never executable ESLint configuration. Exit 1 on diagnostics, 2 on incomplete lint."
        );
        return Ok(());
    }
    let mut version = None;
    let mut runes = None;
    let mut lint_config = Value::Null;
    let mut config_seen = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--runes" | "--legacy" if runes.is_none() => runes = Some(arg == "--runes"),
            "--config" if !config_seen => {
                let path = args.next().ok_or("Missing --config path")?;
                if std::fs::metadata(&path)?.len() > 1024 * 1024 {
                    return Err("Lint configuration exceeds 1 MiB".into());
                }
                lint_config = serde_json::from_str(&std::fs::read_to_string(path)?)?;
                config_seen = true;
            }
            value if !value.starts_with('-') && version.is_none() => version = Some(arg),
            _ => return Err("Unknown or duplicate lint argument".into()),
        }
    }
    let version = version.unwrap_or_else(|| "5.53.11".into());
    let validated = ts_svelte_tools::session::Session::default().request(
        json!({"op":"sessionConfigure","compatibility":{"svelteVersion":version},"runes":runes,"lintConfig":lint_config}).to_string().as_bytes());
    if validated["ok"] != true {
        println!("{validated}");
        std::process::exit(2);
    }
    let mut files = Vec::new();
    collect(Path::new(&root), &mut files)?;
    if files.is_empty() {
        return Err("No supported source files found".into());
    }
    let mut results = Vec::new();
    let mut failed = false;
    let mut incomplete = false;
    let mut enabled_rules = BTreeSet::new();
    for path in files {
        let text = std::fs::read_to_string(&path)?;
        let response=ts_svelte_tools::dispatch(json!({"op":"lint","runes":runes,"lintConfig":lint_config,"document":{"uri":path.to_string_lossy(),"version":1,"text":text},"compatibility":{"svelteVersion":version}}).to_string().as_bytes());
        let body = if response["ok"] == true {
            &response["result"]
        } else {
            &response
        };
        let diagnostics = body["diagnostics"].as_array().cloned().unwrap_or_default();
        enabled_rules.extend(
            body["rules"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned),
        );
        failed |= !diagnostics.is_empty();
        incomplete |= response["ok"] != true && diagnostics.is_empty();
        results.push(json!({"path":path,"diagnostics":diagnostics,"error":response.get("error").unwrap_or(&Value::Null)}));
    }
    println!(
        "{}",
        json!({"ok":!incomplete,"files":results.len(),"results":results,"rules":enabled_rules,"configuration":if config_seen {"resolved JSON rules"} else {"built-in defaults"}})
    );
    std::process::exit(if incomplete { 2 } else { i32::from(failed) });
}

fn main() {
    if let Err(error) = run() {
        println!(
            "{}",
            json!({"ok":false,"error":{"code":"lint_input","message":error.to_string()}})
        );
        std::process::exit(2);
    }
}
