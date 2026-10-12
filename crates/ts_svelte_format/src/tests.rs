use super::*;

#[test]
fn supported_languages_are_idempotent_and_keep_data() {
    for (filename, source) in [
        (
            "Component.svelte",
            "<script lang=\"ts\">let count:number=0;</script><button onclick={()=>count++}>{count}</button><style>button{color:red}</style>",
        ),
        ("module.js", "const value={name:'🙂',count:1};"),
        (
            "module.ts",
            "interface Item{value:string};const item:Item={value:'ok'};",
        ),
        ("module.tsx", "const element=<div title='ok'>hello</div>;"),
        ("style.css", "a{color:red;margin:0 1px}"),
        ("value.json", "{\"message\":\"🙂\",\"items\":[1,2,3]}"),
        ("value.jsonc", "// retain comment\n{\"message\":\"ok\",}"),
        ("value.json5", "{message:'ok',}"),
    ] {
        let first = format(source, filename, &ResolvedOptions::default())
            .unwrap_or_else(|error| panic!("{filename}: {error}"));
        let second = format(&first.code, filename, &ResolvedOptions::default())
            .unwrap_or_else(|error| panic!("{filename}: {error}"));
        assert_eq!(first.code, second.code, "{filename}");
        assert!(!second.changed, "{filename}");
        assert!(second.edits.is_empty(), "{filename}");
        if filename.ends_with("jsonc") {
            assert!(first.code.contains("retain comment"));
        }
    }
}

#[test]
fn replacements_use_original_crlf_bom_utf16_snapshot() {
    let source = "\u{feff}<p>🙂</p>\r\n<p title='ok'>hello</p>";
    let snapshot = Snapshot {
        uri: "file:///A.svelte".into(),
        version: 7,
        text: source.into(),
    };
    let document = Document::new(snapshot.clone());
    let result = format_document(
        snapshot,
        "A.svelte",
        &ResolvedOptions {
            end_of_line: EndOfLine::Crlf,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(result.code.starts_with('\u{feff}'));
    assert!(result.code.contains("🙂"));
    assert!(result.code.ends_with("\r\n"));
    assert!(!result.code.replace("\r\n", "").contains('\n'));
    assert_eq!(result.version, 7);
    assert_eq!(result.edits.len(), 1);
    assert_eq!(
        result.edits[0].range.end,
        document.position_at(source.len()).unwrap()
    );
    assert_eq!(result.edits[0].new_text, result.code);
    assert_eq!(result.edits[0].bytes.end, source.len());
}

#[test]
fn resolved_options_change_js_and_svelte_output() {
    let options:ResolvedOptions=serde_json::from_value(json!({"singleQuote":true,"semi":false,"svelteAllowShorthand":false,"svelteIndentScriptAndStyle":false,"svelteSortOrder":"scripts-markup-styles-options","useTabs":true})).unwrap();
    let result = format("const value = \"hello\";", "x.ts", &options).unwrap();
    assert_eq!(result.code, "const value = 'hello'\n");
    let result = format(
        "<script>let title='hello';</script><p title={title}>{title}</p>",
        "X.svelte",
        &options,
    )
    .unwrap();
    assert!(result.code.contains("title={title}"));
    assert!(result.code.contains("\nlet title = 'hello'\n"));
}

#[test]
fn unsupported_or_invalid_requests_never_return_edits() {
    for options in [
        json!({"plugins":["prettier-plugin-svelte"]}),
        json!({"singleQuote":"yes"}),
        json!({"tabWidth":256}),
        json!({"trailingComma":"maybe"}),
    ] {
        let response=dispatch(json!({"op":"format","document":{"uri":"a.svelte","version":1,"text":"<p>hello</p>"},"options":options}).to_string().as_bytes());
        assert_eq!(response["ok"], false, "{response}");
        assert!(response.get("result").is_none());
    }
    assert!(
        format(
            "<p>hello</p>",
            "x.svelte",
            &ResolvedOptions {
                svelte_sort_order: "scripts-only".into(),
                ..Default::default()
            }
        )
        .is_err()
    );
    assert!(format("const = ;", "x.js", &Default::default()).is_err());
    assert!(format("{broken", "x.json", &Default::default()).is_err());
    assert!(format("text", "x.md", &Default::default()).is_err());
    for language in ["sass", "stylus", "unknown"] {
        let source = format!("<style lang=\"{language}\">p\n  color: red</style>");
        assert!(
            format(&source, "x.svelte", &Default::default()).is_err(),
            "{language}"
        );
    }
}

#[test]
fn auto_eol_and_protocol_preserve_version() {
    let response=dispatch(br#"{"op":"format","document":{"uri":"file:///x.ts","version":42,"text":"const x=1;\r\n"},"options":{"endOfLine":"auto"}}"#);
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["result"]["version"], 42);
    assert_eq!(response["result"]["code"], "const x = 1;\r\n");
    assert_eq!(dispatch(br#"{"op":"capabilities"}"#)["ok"], true);
}
