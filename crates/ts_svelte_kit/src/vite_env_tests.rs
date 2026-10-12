use super::*;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};

struct Dir(PathBuf);
impl Dir {
    fn new(files: &[(&str, &str)]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ts-svelte-vite-env-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        for (name, content) in files {
            std::fs::write(dir.join(name), content).unwrap();
        }
        Self(dir)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).into(), (*v).into()))
        .collect()
}

/// Each case: env files, mode, prefixes and the initial process environment.
type Case = (
    &'static [(&'static str, &'static str)],
    &'static str,
    &'static [&'static str],
    &'static [(&'static str, &'static str)],
);
const CASES: &[Case] = &[
    (
        &[(
            ".env",
            "\u{feff}PLAIN=value\nexport EXPORTED = spaced value  \nSINGLE='single # not comment'\nDOUBLE=\"line\\nbreak\\r\"\nBACK=`back`\nCOLON: colon value\nEMPTY=\nINLINE=value # comment\n# COMMENTED=1\nDOTTED.KEY-1=ok\nMULTI=\"first\nsecond\"\nUNCLOSED='abc\nAFTER=x\n",
        )],
        "production",
        &[""],
        &[],
    ),
    (
        &[(
            ".env",
            "A=1\nB=${A}\nC=$A-x\nD=${UNSET:-fallback}\nE=${A:+alt}\nF=\\$A\nG=${B}${A}\nH=${UNSET-dash}\nI=${A+plus}\nJ=${EMPTY:-empty-default}\nEMPTY=\nK=$PROC\nL=${nullkey}\nnullkey=x\nM=$A$A$A\nSHADOW=1\nS=${SHADOW:-shadowed}\n",
        )],
        "production",
        &[""],
        &[("PROC", "from-process"), ("SHADOW", "")],
    ),
    (
        &[
            (
                ".env",
                "SHARED=env\nONLY_ENV=1\n10=ten\n2=two\nREF=${SHARED}\n",
            ),
            (".env.local", "SHARED=local\n"),
            (".env.production", "SHARED=prod\nNODE_ENV=development\n"),
            (
                ".env.production.local",
                "SHARED=prod-local\nBROWSER=firefox\n",
            ),
            (".env.development", "SHARED=dev\n"),
        ],
        "production",
        &[""],
        &[("OVERRIDE", "process")],
    ),
    (
        &[(
            ".env",
            "X=$$\nY=${DOLLAR}\nDOLLAR=a$`c$'d$1$<e$$\nZ=${DOLLAR}\nCRLF=one\r\nCR=two\rLAST=three\nUNI=é😀\n",
        )],
        "staging",
        &["VITE_", "PUBLIC_"],
        &[("VITE_FROM_PROCESS", "p"), ("PUBLIC_X", "y")],
    ),
    (
        &[(
            ".env",
            "VITE_A=1\nPUBLIC_B=${VITE_A}\nPRIVATE=secret\nSELF=${SELF:-d}\nP=file\n",
        )],
        "production",
        &["VITE_", "PUBLIC_"],
        &[("P", "proc"), ("NODE_ENV", "test")],
    ),
    // `$&` reinserts the template: Vite grows the string until V8 fails.
    (
        &[(".env", "LOOP=x$&y\nUSE=${LOOP}\n")],
        "production",
        &[""],
        &[],
    ),
];
const FAILING: &[usize] = &[5];

fn native(case: &Case) -> Result<Value, String> {
    let dir = Dir::new(case.0);
    let mut process = env(case.3);
    let result = load_env(case.1, &dir.0, case.2, &mut process)?;
    Ok(json!({"env": result, "process": process}))
}

#[test]
fn parse_handles_quotes_comments_exports_and_bom() {
    let parsed = parse(CASES[0].0[0].1);
    let get = |key| parsed.get(key).map(str::to_owned);
    assert_eq!(get("PLAIN").as_deref(), Some("value"));
    assert_eq!(get("EXPORTED").as_deref(), Some("spaced value"));
    assert_eq!(get("SINGLE").as_deref(), Some("single # not comment"));
    assert_eq!(get("DOUBLE").as_deref(), Some("line\nbreak\r"));
    assert_eq!(get("BACK").as_deref(), Some("back"));
    assert_eq!(get("COLON").as_deref(), Some("colon value"));
    assert_eq!(get("EMPTY").as_deref(), Some(""));
    assert_eq!(get("INLINE").as_deref(), Some("value"));
    assert_eq!(get("COMMENTED"), None);
    assert_eq!(get("DOTTED.KEY-1").as_deref(), Some("ok"));
    assert_eq!(get("MULTI").as_deref(), Some("first\nsecond"));
}

#[test]
fn expansion_follows_dotenv_expand_operators_and_process_priority() {
    let result = native(&CASES[1]).unwrap();
    let env = &result["env"];
    assert_eq!(env["B"], "1");
    assert_eq!(env["C"], "1-x");
    assert_eq!(env["D"], "fallback");
    assert_eq!(env["E"], "alt");
    assert_eq!(env["F"], "$A");
    assert_eq!(env["G"], "11");
    assert_eq!(env["K"], "from-process");
    assert_eq!(env["M"], "111");
    // SHADOW is "" in the process environment: it shadows the file value both
    // during expansion and in the final overlay.
    assert_eq!(env["S"], "shadowed");
    assert_eq!(env["SHADOW"], "");
}

#[test]
fn files_override_in_mode_order_and_node_env_is_recorded() {
    let result = native(&CASES[2]).unwrap();
    assert_eq!(result["env"]["SHARED"], "prod-local");
    assert_eq!(result["env"]["REF"], "prod-local");
    assert_eq!(result["env"]["OVERRIDE"], "process");
    assert_eq!(result["process"]["VITE_USER_NODE_ENV"], "development");
    assert_eq!(result["process"]["BROWSER"], "firefox");
    assert!(result["env"].get("ONLY_ENV").is_some());
}

#[test]
fn js_object_order_puts_array_indices_first() {
    let mut object = JsStringObject::default();
    for key in ["b", "10", "a", "2", "01", "4294967295"] {
        object.set(key, String::new());
    }
    assert_eq!(object.keys(), ["2", "10", "b", "a", "01", "4294967295"]);
}

#[test]
fn replacement_patterns_follow_get_substitution() {
    assert_eq!(replace_first("x${A}y", "${A}", "$$"), "x$y");
    assert_eq!(replace_first("x${A}y", "${A}", "[$&]"), "x[${A}]y");
    assert_eq!(replace_first("x${A}y", "${A}", "$`|$'"), "xx|yy");
    assert_eq!(replace_first("x${A}y", "${A}", "$1$<n>"), "x$1$<n>y");
}

#[test]
fn kit_build_defaults_node_env_and_rejects_local_mode() {
    let dir = Dir::new(&[(".env", "FROM_FILE=1\n")]);
    let root = dir.0.parent().unwrap();
    let name = dir.0.file_name().unwrap().to_str().unwrap();
    let result = kit_build_env(root, name, "production", &env(&[("NODE_ENV", "")])).unwrap();
    assert_eq!(result["NODE_ENV"], "production");
    assert_eq!(result["FROM_FILE"], "1");
    let kept = kit_build_env(root, name, "production", &env(&[("NODE_ENV", "test")])).unwrap();
    assert_eq!(kept["NODE_ENV"], "test");
    assert!(load_env("local", &dir.0, &[""], &mut BTreeMap::new()).is_err());
}

/// Real Vite `loadEnv` over the same files and process environment.
#[test]
#[ignore = "requires TS_SVELTE_VITE_PACKAGE (installed vite 7.3.1 directory) and node"]
fn matches_installed_vite_load_env() {
    let package = PathBuf::from(std::env::var_os("TS_SVELTE_VITE_PACKAGE").unwrap());
    let version: Value =
        serde_json::from_slice(&std::fs::read(package.join("package.json")).unwrap()).unwrap();
    assert_eq!(version["version"], "7.3.1");
    let node = std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("node"))
                .find(|candidate| candidate.is_file())
        })
        .expect("node on PATH");
    let module = url::Url::from_file_path(package.join("dist/node/index.js")).unwrap();
    for (index, case) in CASES.iter().enumerate() {
        let dir = Dir::new(case.0);
        let script = format!(
            "import {{loadEnv}} from {module:?};\
             const env=loadEnv({mode:?},{dir:?},{prefixes});\
             process.stdout.write(JSON.stringify({{env,process:{{...process.env}}}}));",
            module = module.as_str(),
            mode = case.1,
            dir = dir.0.to_str().unwrap(),
            prefixes = serde_json::to_string(case.2).unwrap(),
        );
        let output = std::process::Command::new(&node)
            .args(["--input-type=module", "-e", &script])
            .env_clear()
            .envs(case.3.iter().copied())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        if FAILING.contains(&index) {
            // V8 throws RangeError or runs out of heap first, depending on memory.
            assert!(!output.status.success(), "Vite unexpectedly completed");
            assert!(native(case).is_err());
            continue;
        }
        assert!(output.status.success(), "{stderr}");
        let oracle: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(native(case).unwrap(), oracle, "case {:?}", case.0);
    }
}
