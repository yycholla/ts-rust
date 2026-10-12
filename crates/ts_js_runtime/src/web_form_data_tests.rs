use super::*;
use boa_engine::{Source, builtins::promise::PromiseState, object::builtins::JsPromise};
use futures_util::FutureExt;

fn context() -> Context {
    let mut context = Context::builder()
        .instructions_remaining(10_000_000)
        .build()
        .unwrap();
    web_blob::register(&mut context).unwrap();
    register(&mut context).unwrap();
    context
}
fn check(body: &str) {
    let mut context = context();
    let source = format!(
        r#"(async()=>{{
        function eq(a,b) {{if(JSON.stringify(a)!==JSON.stringify(b)) throw new Error(JSON.stringify({{actual:a,expected:b}}));}}
        function throws(fn,kind=TypeError) {{try{{fn()}}catch(e){{if(e instanceof kind)return e;throw e}}throw new Error('Expected exception')}}
        {body}
        return true;
    }})()"#
    );
    let result = context.eval(Source::from_bytes(&source)).unwrap();
    let promise = JsPromise::from_object(result.as_object().unwrap()).unwrap();
    context.run_jobs().unwrap();
    match promise.state() {
        PromiseState::Fulfilled(value) => assert_eq!(value.as_boolean(), Some(true)),
        state => panic!("FormData test did not fulfill: {state:?}"),
    }
}
fn text(name: &str, value: &str) -> EntrySnapshot {
    EntrySnapshot {
        name: name.into(),
        value: EntryValue::Text(value.into()),
    }
}
fn parse(body: Vec<u8>, content_type: &str) -> Result<Vec<EntrySnapshot>, String> {
    decode_multipart(body, content_type, 123)
        .now_or_never()
        .expect("Pure memory multipart parsing cannot wait for external work")
}
#[test]
fn form_data_order_duplicates_and_mutations() {
    check(
        r#"
        const f=new FormData();
        f.append('b','1');f.append('a','2');f.append('b','3');f.append('z','4');
        eq([...f],[['b','1'],['a','2'],['b','3'],['z','4']]);
        eq(f.get('b'),'1');eq(f.getAll('b'),['1','3']);eq(f.has('b'),true);
        eq(f.get('missing'),null);eq(f.getAll('missing'),[]);
        f.set('b','new');eq([...f],[['b','new'],['a','2'],['z','4']]);
        f.set('end','5');f.delete('a');eq([...f.keys()],['b','z','end']);
        f.append('b','last');f.delete('b');eq([...f.values()],['4','5']);
        eq(f.delete('absent'),undefined);
    "#,
    );
}
#[test]
fn form_data_usv_coercions_and_overload_errors() {
    check(
        r#"
        const f=new FormData();
        f.append('\ud800', '\udc00');eq(f.get('\udfff'),'\ufffd');
        f.append(12,null);eq(f.get(12),'null');
        f.append('undefined',undefined);eq(f.get('undefined'),'undefined');
        throws(()=>f.append('x','a','filename'));
        throws(()=>f.append('x','a',undefined));
        throws(()=>f.set('x','a',undefined,12));
        throws(()=>f.append(Symbol(),'a'));throws(()=>f.append('x',Symbol()));
        for(const method of ['append','set']) throws(()=>f[method]('x'));
        for(const method of ['get','getAll','has','delete']) throws(()=>f[method]());
        const calls=[];f.append({toString(){calls.push('name');return 'x'}},{toString(){calls.push('value');return 'v'}});
        eq(calls,['name','value']);eq(f.get('x'),'v');
    "#,
    );
}
#[test]
fn form_data_file_identity_and_filename_overrides() {
    check(
        r#"
        const f=new FormData(),file=new File(['hello'],'original',{type:'TEXT/PLAIN',lastModified:42});
        f.append('same',file);eq(f.get('same')===file,true);
        f.append('undefined',file,undefined);eq(f.get('undefined')===file,true);
        f.append('renamed',file,'new\ud800');const renamed=f.get('renamed');
        eq(renamed===file,false);eq(renamed.name,'new\ufffd');eq(renamed.lastModified,42);eq(renamed.type,'text/plain');eq(await renamed.text(),'hello');
        const blob=new Blob(['data'],{type:'IMAGE/PNG'});f.append('blob',blob);
        const converted=f.get('blob');eq(converted instanceof File,true);eq(converted===blob,false);eq(converted.name,'blob');eq(converted.type,'image/png');
        f.set('blob',blob,null);eq(f.get('blob').name,'null');
        Object.defineProperty(file,'name',{get(){throw Error('shadow')}});
        Object.defineProperty(file,'type',{get(){throw Error('shadow')}});
        f.append('native',file,'another');eq(f.get('native').name,'another');eq(f.get('native').type,'text/plain');
    "#,
    );
}
#[test]
fn form_data_iterators_are_live_and_complete_permanently() {
    check(
        r#"
        const f=new FormData();f.append('a','1');f.append('b','2');
        const it=f.entries();eq(it[Symbol.iterator](),it);eq(it.next().value,['a','1']);
        f.append('c','3');eq(it.next().value,['b','2']);f.set('c','changed');eq(it.next().value,['c','changed']);
        eq(it.next().done,true);f.append('d','4');eq(it.next().done,true);
        const deleted=f.keys();eq(deleted.next().value,'a');f.delete('a');eq(deleted.next().value,'c');
        eq(f[Symbol.iterator],f.entries);eq(Object.prototype.toString.call(it),'[object FormData Iterator]');
        throws(()=>Object.getPrototypeOf(it).next.call({}));
    "#,
    );
}
#[test]
fn form_data_for_each_and_coercions_can_reenter() {
    check(
        r#"
        const f=new FormData();f.append('a','1');f.append('b','2');const seen=[],that={};
        f.forEach(function(value,key,form){eq(this,that);eq(form,f);seen.push([key,value]);if(key==='a'){f.set('b','changed');f.append('c','3')}},that);
        eq(seen,[['a','1'],['b','changed'],['c','3']]);
        f.set({toString(){f.append('x','old');return 'x'}},{toString(){f.append('x','older');return 'new'}});
        eq(f.getAll('x'),['new']);
        throws(()=>f.forEach(null));
        const error=new Error('callback');try{f.forEach(()=>{throw error})}catch(caught){eq(caught===error,true)}
    "#,
    );
}
#[test]
fn form_data_constructor_brand_and_descriptor_contract() {
    check(
        r#"
        eq([...new FormData(undefined)],[]);eq([...new FormData(undefined,null)],[]);
        throws(()=>new FormData(null));throws(()=>new FormData({}));throws(()=>new FormData([]));
        throws(()=>new FormData(undefined,{}));throws(()=>FormData());
        throws(()=>FormData.prototype.append.call({},'x','y'));
        throws(()=>FormData.prototype.entries.call(Object.create(FormData.prototype)));
        eq(FormData.length,0);eq(FormData.prototype.append.length,2);eq(FormData.prototype.get.length,1);
        eq(Object.getOwnPropertyDescriptor(FormData.prototype,'append').enumerable,true);
        eq(Object.prototype.toString.call(new FormData()),'[object FormData]');
        class Sub extends FormData {}const sub=new Sub();sub.append('x','y');eq(sub instanceof Sub,true);eq(sub.get('x'),'y');
    "#,
    );
}
#[test]
fn form_data_gc_retains_files_through_form_and_iterator() {
    let mut context = context();
    context.eval(Source::from_bytes("globalThis.saved=(()=>{const f=new FormData();f.append('x',new File(['abc'],'x'));return f.values()})()" )).unwrap();
    boa_gc::force_collect();
    let value = context
        .eval(Source::from_bytes("saved.next().value"))
        .unwrap();
    let snapshot = web_blob::snapshot(&value).unwrap();
    assert_eq!(snapshot.bytes, b"abc");
    assert_eq!(snapshot.file.unwrap().name, "x");
}
#[test]
fn form_data_native_snapshot_round_trip() {
    let mut context = context();
    let entries = vec![
        text("a", "one"),
        EntrySnapshot {
            name: "file".into(),
            value: EntryValue::File(FileSnapshot {
                name: "a.bin".into(),
                bytes: vec![0, 255],
                media_type: "application/octet-stream".into(),
                last_modified: -7,
            }),
        },
        text("a", "two"),
    ];
    let form = from_entries(entries.clone(), &mut context).unwrap();
    assert_eq!(snapshot(&form.into()).unwrap(), entries);
    assert!(snapshot(&JsValue::null()).is_err());
    assert!(from_entries(vec![text("x", &"x".repeat(MAX_DATA))], &mut context).is_err());
}
#[test]
fn multipart_encoding_uses_exact_crlf_names_and_file_bytes() {
    let entries = vec![
        text("n\n\"\\", "a\rb\nc\r\nd"),
        EntrySnapshot {
            name: "file".into(),
            value: EntryValue::File(FileSnapshot {
                name: "f\r\n\"\\.bin".into(),
                bytes: vec![0, 255, 13],
                media_type: String::new(),
                last_modified: 1,
            }),
        },
    ];
    let encoded = encode_multipart(&entries, "test-boundary").unwrap();
    let mut expected=b"--test-boundary\r\nContent-Disposition: form-data; name=\"n%0D%0A%22\\\"\r\n\r\na\r\nb\r\nc\r\nd\r\n--test-boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"f%0D%0A%22\\.bin\"\r\nContent-Type: application/octet-stream\r\n\r\n".to_vec();
    expected.extend_from_slice(&[0, 255, 13]);
    expected.extend_from_slice(b"\r\n--test-boundary--\r\n");
    assert_eq!(encoded.bytes, expected);
    assert_eq!(
        encoded.content_type,
        "multipart/form-data; boundary=test-boundary"
    );
    assert_eq!(entries[0], text("n\n\"\\", "a\rb\nc\r\nd"));
}
#[test]
fn multipart_boundary_is_validated_and_collision_is_atomic() {
    for invalid in ["", "bad\r\nheader", "bad\"quote", "trailing "] {
        assert!(encode_multipart(&[], invalid).is_err());
    }
    assert!(encode_multipart(&[], &"a".repeat(71)).is_err());
    assert!(encode_multipart(&[text("x", "before--boundary-after")], "boundary").is_err());
    let empty = encode_multipart(&[], "has:space inside").unwrap();
    assert_eq!(
        empty.content_type,
        "multipart/form-data; boundary=\"has:space inside\""
    );
    assert_eq!(empty.bytes, b"--has:space inside--\r\n");
    assert_eq!(parse(empty.bytes, &empty.content_type).unwrap(), vec![]);
}
#[test]
fn multipart_parser_preserves_order_files_and_utf8_without_bom_removal() {
    let entries = vec![
        text("same", "\u{feff}hello"),
        EntrySnapshot {
            name: "upload".into(),
            value: EntryValue::File(FileSnapshot {
                name: "日本\\file.bin".into(),
                bytes: vec![0, 255, 13, 10],
                media_type: "application/octet-stream".into(),
                last_modified: 123,
            }),
        },
        text("same", "second"),
    ];
    let encoded = encode_multipart(&entries, "roundtrip").unwrap();
    assert_eq!(
        parse(encoded.bytes, &encoded.content_type).unwrap(),
        entries
    );
    let body=b"--x\r\nContent-Disposition: FORM-DATA; NAME=\"a;b\"; FILENAME=\"quote%22.bin\"\r\n\r\nx\r\n--x\r\nContent-Disposition: form-data; name=\"_charset_\"\r\nContent-Type: text/plain; charset=windows-1252\r\n\r\n\xff\r\n--x--\r\n".to_vec();
    let parsed = parse(body, "multipart/form-data; boundary=x").unwrap();
    assert_eq!(
        parsed[0],
        EntrySnapshot {
            name: "a;b".into(),
            value: EntryValue::File(FileSnapshot {
                name: "quote\".bin".into(),
                bytes: b"x".to_vec(),
                media_type: "text/plain".into(),
                last_modified: 123
            })
        }
    );
    assert_eq!(parsed[1], text("_charset_", "\u{fffd}"));
}
#[test]
fn multipart_parser_rejects_malformed_or_unsupported_parts() {
    for headers in [
        "Content-Disposition: form-data; name=\"x\"; filename*=utf-8''x",
        "Content-Disposition: attachment; name=\"x\"",
        "Content-Disposition: form-data; filename=\"x\"",
        "Content-Disposition: form-data; name=\"unterminated",
        "Content-Disposition: form-data; name=\"x\"\r\nContent-Transfer-Encoding: base64",
        "Content-Disposition: form-data; name=\"x\"\r\nContent-Type: multipart/mixed; boundary=y",
    ] {
        let body = format!("--x\r\n{headers}\r\n\r\nbody\r\n--x--\r\n").into_bytes();
        assert!(
            parse(body, "multipart/form-data; boundary=x").is_err(),
            "{headers}"
        );
    }
    assert!(
        parse(
            b"--x\r\nContent-Disposition: form-data; name=\"x\"\r\n\r\ntruncated".to_vec(),
            "multipart/form-data; boundary=x"
        )
        .is_err()
    );
    assert!(parse(vec![], "multipart/form-data").is_err());
    assert!(parse(vec![], "text/plain; boundary=x").is_err());
}
#[test]
fn form_urlencoding_normalizes_html_entries_and_decodes_forgivingly() {
    let entries = vec![
        text("n\nx", "a b+é\r"),
        EntrySnapshot {
            name: "file".into(),
            value: EntryValue::File(FileSnapshot {
                name: "a\nb".into(),
                bytes: vec![],
                media_type: String::new(),
                last_modified: 0,
            }),
        },
    ];
    let encoded = encode_urlencoded(&entries).unwrap();
    assert_eq!(encoded.bytes, b"n%0D%0Ax=a+b%2B%C3%A9%0D%0A&file=a%0D%0Ab");
    assert_eq!(
        decode_urlencoded(&encoded.bytes).unwrap(),
        vec![text("n\r\nx", "a b+é\r\n"), text("file", "a\r\nb")]
    );
    assert_eq!(
        decode_urlencoded(b"a=1&a=2&&bad=%ff&plus=+&incomplete=%Q&bare").unwrap(),
        vec![
            text("a", "1"),
            text("a", "2"),
            text("bad", "\u{fffd}"),
            text("plus", " "),
            text("incomplete", "%Q"),
            text("bare", "")
        ]
    );
}
#[test]
fn form_data_registration_requires_blob_and_preserves_conflicting_globals() {
    let mut context = Context::builder()
        .instructions_remaining(10_000_000)
        .build()
        .unwrap();
    assert!(register(&mut context).is_err());
    assert_eq!(
        context
            .eval(Source::from_bytes("typeof FormData"))
            .unwrap()
            .as_string()
            .unwrap()
            .to_std_string_lossy(),
        "undefined"
    );
    web_blob::register(&mut context).unwrap();
    context
        .eval(Source::from_bytes("globalThis.FormData=123"))
        .unwrap();
    assert!(register(&mut context).is_err());
    assert_eq!(
        context
            .eval(Source::from_bytes("FormData"))
            .unwrap()
            .as_number(),
        Some(123.0)
    );
}

#[test]
fn multipart_repeated_headers_use_last_header_and_first_parameter() {
    let body=b"--x\r\nContent-Disposition: form-data; name=\"old\"\r\nContent-Disposition: form-data; name=\"new\"; name=\"ignored\"\r\n\r\nvalue\r\n--x--\r\n".to_vec();
    assert_eq!(
        parse(body, "multipart/form-data; boundary=x").unwrap(),
        vec![text("new", "value")]
    );
}
