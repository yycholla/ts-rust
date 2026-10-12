use super::*;
use serde_json::json;
use ts_goport::frontend::vfs::Fs;

struct Client {
    host: Host,
    session: Value,
    generation: Value,
    id: u32,
}
impl Client {
    fn new() -> Self {
        let mut host = Host::new();
        let reply = host.request(json!({"id":1,"op":"initialize","params":{
            "root":"/project","project":"/project/tsconfig.json","files":{
                "/project/tsconfig.json":"{\"compilerOptions\":{\"strict\":true,\"target\":\"ESNext\",\"module\":\"ESNext\",\"moduleResolution\":\"Bundler\",\"skipLibCheck\":true},\"include\":[\"*.ts\"]}",
                "/project/main.ts":"import { value } from './value';\nexport const answer: number = value;\n",
                "/project/value.ts":"export const value = 42;\n"
            }}}));
        assert_eq!(reply["ok"], true, "{reply}");
        Self {
            host,
            session: reply["session"].clone(),
            generation: reply["generation"].clone(),
            id: 1,
        }
    }
    fn call(&mut self, op: &str, params: Value) -> Value {
        self.id += 1;
        let reply = self
            .host
            .request(json!({"id":self.id,"session":self.session,
            "generation":self.generation,"op":op,"params":params}));
        self.generation = reply["generation"].clone();
        assert_eq!(reply["ok"], true, "{op}: {reply}");
        reply["result"].clone()
    }
    fn diagnostics(&mut self) -> Value {
        self.call(
            "diagnostics",
            json!({"textDocument":{"uri":"file:///project/main.ts"}}),
        )
    }
    fn change(&mut self, version: i32, text: &str) {
        self.call(
            "change",
            json!({"textDocument":{"uri":"file:///project/main.ts","version":version},
            "contentChanges":[{"text":text}]}),
        );
    }
}

fn has_code(value: &Value, code: u32) -> bool {
    match value {
        Value::Object(map) => {
            map.get("code")
                .is_some_and(|value| value.as_u64() == Some(u64::from(code)))
                || map.values().any(|value| has_code(value, code))
        }
        Value::Array(array) => array.iter().any(|value| has_code(value, code)),
        _ => false,
    }
}

#[test]
fn persistent_semantics_edits_navigation_watches_and_disposal() {
    let mut c = Client::new();
    let good = "import { value } from './value';\nexport const answer: number = value;\n";
    c.call("open", json!({"textDocument":{"uri":"file:///project/main.ts","version":-7,"languageId":"typescript","text":good}}));
    assert!(!has_code(&c.diagnostics(), 2322));
    for (version, text, error) in [
        (-6, "export const answer: number = 'wrong';\n", true),
        (-5, good, false),
        (-4, "export const answer: number = false;\n", true),
        (-3, good, false),
    ] {
        c.change(version, text);
        assert_eq!(has_code(&c.diagnostics(), 2322), error);
    }
    let hover = c.call("hover", json!({"textDocument":{"uri":"file:///project/main.ts"},"position":{"line":1,"character":14}}));
    assert!(hover.to_string().contains("number"), "{hover}");
    let definition = c.call("definition", json!({"textDocument":{"uri":"file:///project/main.ts"},"position":{"line":1,"character":30}}));
    assert!(definition.to_string().contains("value.ts"), "{definition}");
    c.call(
        "files",
        json!({"files":{"/project/value.ts":"export const value = 'changed';\n"}}),
    );
    assert!(has_code(&c.diagnostics(), 2322));
    c.call(
        "files",
        json!({"files":{"/project/value.ts":"export const value = 42;\n"}}),
    );
    assert!(!has_code(&c.diagnostics(), 2322));
    c.call("files", json!({"files":{"/project/value.ts":null}}));
    assert!(has_code(&c.diagnostics(), 2307));
    c.call(
        "files",
        json!({"files":{"/project/value.ts":"export const value = 42;\n"}}),
    );
    assert!(!has_code(&c.diagnostics(), 2307));
    c.change(-2, "export function loose(value) { return value; }\n");
    assert!(has_code(&c.diagnostics(), 7006));
    c.call("files", json!({"files":{"/project/tsconfig.json":"{\"compilerOptions\":{\"strict\":false,\"target\":\"ESNext\",\"skipLibCheck\":true},\"include\":[\"*.ts\"]}"}}));
    assert!(!has_code(&c.diagnostics(), 7006));

    let generation = c.generation.clone();
    let stale = c.host.request(
        json!({"id":100,"session":c.session,"generation":0,"op":"diagnostics","params":{}}),
    );
    assert_eq!(stale["error"]["code"], "stale_session");
    assert_eq!(stale["generation"], generation);
    c.call("shutdown", json!({}));
    let closed = c.host.request(json!({"op":"initialize","params":{}}));
    assert_eq!(closed["error"]["code"], "closed");
    let mut replacement = Client::new();
    assert_ne!(replacement.session, c.session);
    replacement.call("shutdown", json!({}));
}

#[test]
fn memory_batches_are_atomic_and_entries_are_sorted() {
    let fs = memory::MemoryFs::new(&json!({"/p/z.ts":"z","/p/a/x.ts":"x","/p/b.ts":"b"})).unwrap();
    let entries = fs.get_accessible_entries("/p");
    assert_eq!(entries.files, ["b.ts", "z.ts"]);
    assert_eq!(entries.directories, ["a"]);
    assert!(
        fs.prepare(&json!({"/p/z.ts":null,"/p/a":"collision"}))
            .is_err()
    );
    assert!(fs.file_exists("/p/z.ts"));
    assert!(fs.prepare(&json!({"/p/../escape.ts":"bad"})).is_err());
    fs.apply(
        fs.prepare(&json!({"/p/a/x.ts":null,"/p/a":"now a file"}))
            .unwrap(),
    );
    assert!(fs.file_exists("/p/a"));
    assert!(!fs.directory_exists("/p/a"));
}

#[test]
fn sequential_utf16_edits_reject_split_surrogates_and_false_lengths() {
    let source = "a😀b\r\nsecond";
    let range = json!({"start":{"line":0,"character":1},"end":{"line":0,"character":3}});
    assert_eq!(changes::apply(source, &json!([
        {"range":range,"rangeLength":2,"text":"x"},
        {"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":6}},"text":"done"}
    ])).unwrap(), "axb\r\ndone");
    assert!(
        changes::apply(
            source,
            &json!([{ "range":range,"rangeLength":1,"text":"x" }])
        )
        .is_err()
    );
    assert!(changes::apply(source, &json!([{ "range":{"start":{"line":0,"character":2},"end":{"line":0,"character":3}},"text":"x" }])).is_err());
    assert!(changes::apply(source, &json!([])).is_err());
}
