use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

fn sibling(name: &str) -> Result<std::path::PathBuf, String> {
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    Ok(executable
        .parent()
        .ok_or("Missing executable directory")?
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX)))
}

fn execute(mode: &str, value: &serde_json::Value) -> Result<i32, String> {
    if mode == "check" {
        let mut child = Command::new(sibling("svelte-project")?)
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        let sent = child
            .stdin
            .take()
            .ok_or("Missing checker input")?
            .write_all(value.to_string().as_bytes());
        if let Err(error) = sent {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.to_string());
        }
        return child
            .wait()
            .map(|status| status.code().unwrap_or(1))
            .map_err(|e| e.to_string());
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let config = std::env::temp_dir().join(format!(
        "ts-svelte-project-{}-{stamp}.json",
        std::process::id()
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&config)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        file.write_all(value.to_string().as_bytes())
            .map_err(|e| e.to_string())?;
        let mut command = Command::new(sibling("svelte-lsp")?);
        command.arg("--project-config-delete").arg(&config);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // The editor owns the actual server PID, including signal delivery.
            Err(command.exec().to_string())
        }
        #[cfg(not(unix))]
        {
            Err("The Kit serve launcher currently requires Unix; use svelte-lsp --project-config directly".into())
        }
    })();
    let _ = std::fs::remove_file(config);
    result
}
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() || matches!(args[0].as_str(), "help" | "--help") {
        println!(
            "Usage: svelte-kit-project ROOT\n       svelte-kit-project inventory ROOT OPTIONS_JSON [--config RESOLVED_JSON]\n       svelte-kit-project emit-client|emit-server|emit-root|emit-ssr|emit-nodes|emit-env RESOLVED_JSON\n       svelte-kit-project request|check ROOT NEW_WORKSPACE runes|legacy|infer\n       svelte-kit-project lsp|serve ROOT runes|legacy|infer\n       svelte-kit-project sync ROOT [--config RESOLVED_JSON] [--env-names NAMES_JSON] [--mode MODE]\nDiscover or natively generate Kit type snapshots without executing configuration. request/lsp emit JSON; check/serve launch sibling native checker/LSP tools. sync generates route, environment, app and tsconfig declarations."
        );
        std::process::exit(if args.len() == 1 { 0 } else { 2 });
    }
    let result = (|| -> Result<serde_json::Value, String> {
        if args[0] == "inventory" {
            if args.len() != 3 && args.len() != 5 {
                return Err("Usage: svelte-kit-project inventory ROOT OPTIONS_JSON [--config RESOLVED_JSON]".into());
            }
            let root = Path::new(&args[1]);
            let config_path = if args.len() == 5 {
                if args[3] != "--config" {
                    return Err("Expected --config RESOLVED_JSON".into());
                }
                Some(Path::new(&args[4]))
            } else {
                None
            };
            let options_path = Path::new(&args[2]);
            if options_path.metadata().map_err(|e| e.to_string())?.len() > 1024 * 1024 {
                return Err("Inventory options exceed 1 MiB".into());
            }
            let options = serde_json::from_str(
                &std::fs::read_to_string(options_path).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let config = ts_svelte_kit::sync_config::load(root, config_path)?;
            let inventory =
                ts_svelte_kit::build_inventory::discover_build_inventory(root, &config, &options)?;
            return Ok(serde_json::json!({"ok":true,"inventory":inventory,
                "clientManifestInput":inventory.client_manifest_input(None),
                "rootInput":inventory.root_input()}));
        }
        if matches!(
            args[0].as_str(),
            "emit-client" | "emit-server" | "emit-root" | "emit-ssr" | "emit-nodes" | "emit-env"
        ) {
            if args.len() != 2 {
                return Err("Build emit commands require one resolved input JSON path".into());
            }
            let path = Path::new(&args[1]);
            if path.metadata().map_err(|e| e.to_string())?.len() > 16 * 1024 * 1024 {
                return Err("Build request exceeds 16 MiB".into());
            }
            let input = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
            if args[0] == "emit-env" {
                let environment = ts_svelte_kit::build_env::generate_build_environment(
                    &serde_json::from_str(&input).map_err(|e| e.to_string())?,
                )?;
                return Ok(serde_json::json!({"ok":true,"result":environment}));
            }
            if args[0] == "emit-ssr" {
                let manifest = ts_svelte_kit::build_ssr_manifest::generate_ssr_manifest(
                    &serde_json::from_str(&input).map_err(|e| e.to_string())?,
                )?;
                return Ok(serde_json::json!({"ok":true,"result":manifest}));
            }
            let files = if args[0] == "emit-nodes" {
                ts_svelte_kit::build_nodes::generate_server_nodes(
                    &serde_json::from_str(&input).map_err(|e| e.to_string())?,
                )?
            } else if args[0] == "emit-client" {
                ts_svelte_kit::build_manifest::generate_client_manifest(
                    &serde_json::from_str(&input).map_err(|e| e.to_string())?,
                )?
            } else if args[0] == "emit-root" {
                let request =
                    serde_json::from_str::<ts_svelte_kit::build_root::ResolvedRootInput>(&input)
                        .map_err(|e| e.to_string())?;
                return Ok(
                    serde_json::json!({"ok":true,"files":ts_svelte_kit::build_root::generate_root(&request)?,"entry":ts_svelte_kit::build_root::root_entry(&request)?}),
                );
            } else {
                ts_svelte_kit::build_server::generate_server(
                    &serde_json::from_str(&input).map_err(|e| e.to_string())?,
                )?
            };
            return Ok(serde_json::json!({"ok":true,"files":files}));
        }
        if args[0] == "sync" {
            let root = Path::new(args.get(1).ok_or("sync requires a project root")?);
            let mut config_path = None;
            let mut names = None::<Vec<String>>;
            let mut environment_mode = "development";
            let mut index = 2;
            while index < args.len() {
                let value = args.get(index + 1).ok_or("Missing sync option value")?;
                match args[index].as_str() {
                    "--config" => config_path = Some(Path::new(value)),
                    "--mode" => environment_mode = value,
                    "--env-names" => {
                        names = Some(
                            serde_json::from_str(
                                &std::fs::read_to_string(value).map_err(|e| e.to_string())?,
                            )
                            .map_err(|e| e.to_string())?,
                        );
                    }
                    _ => {
                        return Err(
                            "Unknown sync option; use --config, --env-names or --mode".into()
                        );
                    }
                }
                index += 2;
            }
            let config = ts_svelte_kit::sync_config::load(root, config_path)?;
            let names = match names {
                Some(names) => names,
                None => ts_svelte_kit::sync_env::discover_names(
                    root,
                    &config.env_dir,
                    environment_mode,
                )?,
            };
            return ts_svelte_kit::sync::generate(root, &config, &names);
        }
        if args.len() == 1 {
            return ts_svelte_kit::project::discover(Path::new(&args[0]))
                .and_then(|p| serde_json::to_value(p).map_err(|e| e.to_string()));
        }
        let mode = args.last().ok_or("Missing runes mode")?;
        let runes = match mode.as_str() {
            "runes" => Some(true),
            "legacy" => Some(false),
            "infer" => None,
            _ => return Err("Expected runes|legacy|infer".into()),
        };
        if args[0] == "lsp-refresh" && args.len() == 3 {
            ts_svelte_kit::sync::refresh_owned(Path::new(&args[1]))?;
        }
        let project = ts_svelte_kit::project::discover(Path::new(&args[1]))?;
        match (args[0].as_str(), args.len()) {
            ("request" | "check", 4) => project.check_request(Path::new(&args[2]), runes),
            ("lsp" | "serve" | "lsp-refresh", 3) => {
                let mut config = project.lsp_configuration(runes)?;
                config["projectProvider"] = serde_json::json!({
                    "executable":std::env::current_exe().map_err(|e|e.to_string())?,
                    "arguments":["lsp-refresh", project.root, mode]
                });
                Ok(config)
            }
            _ => Err("Invalid arguments; use --help".into()),
        }
    })();
    match result {
        Ok(value) => {
            if matches!(args[0].as_str(), "check" | "serve") {
                match execute(&args[0], &value) {
                    Ok(status) => std::process::exit(status),
                    Err(error) => {
                        eprintln!("{error}");
                        std::process::exit(2);
                    }
                }
            }
            println!("{value}");
        }
        Err(message) => {
            println!(
                "{}",
                serde_json::json!({"ok":false,"error":{"code":"project_discovery","message":message}})
            );
            std::process::exit(2);
        }
    }
}
