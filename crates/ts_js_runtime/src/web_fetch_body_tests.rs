use super::*;
use boa_engine::{Source, builtins::promise::PromiseState};

fn setup() -> (Context, BodyIntrinsics, JsObject) {
    let mut context = Context::builder()
        .instructions_remaining(10_000_000)
        .build()
        .unwrap();
    crate::web_url::register(&mut context).unwrap();
    crate::web_blob::register(&mut context).unwrap();
    crate::web_form_data::register(&mut context).unwrap();
    crate::web_headers::register(&mut context).unwrap();
    crate::web_streams::register(&mut context).unwrap();
    let intrinsics = BodyIntrinsics::capture(&mut context).unwrap();
    let headers = web_headers::from_header_list(Vec::new(), &mut context).unwrap();
    (context, intrinsics, headers)
}
fn eval(context: &mut Context, source: &str) -> JsValue {
    context.eval(Source::from_bytes(source)).unwrap()
}
fn extract(intrinsics: &BodyIntrinsics, value: JsValue, context: &mut Context) -> ExtractedBody {
    let init = intrinsics.convert(&value, context).unwrap();
    intrinsics
        .extract(
            init,
            ExtractionOptions {
                keepalive: false,
                multipart_boundary: Some("----native-fetch-body-tests-1234567890"),
            },
            context,
        )
        .unwrap()
}
fn settled(promise: JsPromise, context: &mut Context) -> Result<JsValue, JsValue> {
    context.run_jobs().unwrap();
    match promise.state() {
        PromiseState::Fulfilled(value) => Ok(value),
        PromiseState::Rejected(error) => Err(error),
        PromiseState::Pending => panic!("Fetch body promise remained pending"),
    }
}
fn consume(
    intrinsics: &BodyIntrinsics,
    body: Option<&Body>,
    kind: ConsumptionKind,
    headers: &JsObject,
    context: &mut Context,
) -> JsValue {
    let promise = intrinsics
        .consume(body, kind, headers, 123, context)
        .unwrap();
    match settled(promise, context) {
        Ok(value) => value,
        Err(error) => panic!(
            "Body rejected: {}",
            error.to_string(context).unwrap().to_std_string_lossy()
        ),
    }
}
fn string(value: JsValue) -> String {
    value.as_string().unwrap().to_std_string_lossy()
}

#[test]
fn body_string_union_nullable_and_usv_semantics() {
    let (mut context, intrinsics, headers) = setup();
    assert!(
        intrinsics
            .convert_nullable(&JsValue::null(), &mut context)
            .unwrap()
            .is_none()
    );
    let value = eval(&mut context, "'a\\ud800\\n'");
    let extracted = extract(&intrinsics, value, &mut context);
    assert_eq!(
        extracted.content_type.as_deref(),
        Some("text/plain;charset=UTF-8")
    );
    assert_eq!(extracted.body.length(), Some(5));
    assert_eq!(
        string(consume(
            &intrinsics,
            Some(&extracted.body),
            ConsumptionKind::Text,
            &headers,
            &mut context
        )),
        "a\u{fffd}\n"
    );
    let extracted = extract(&intrinsics, JsValue::null(), &mut context);
    assert_eq!(
        string(consume(
            &intrinsics,
            Some(&extracted.body),
            ConsumptionKind::Text,
            &headers,
            &mut context
        )),
        "null"
    );
    let symbol = eval(&mut context, "Symbol('body')");
    assert!(intrinsics.convert(&symbol, &mut context).is_err());
}

#[test]
fn body_buffer_conversion_precedes_extraction_and_copies_view() {
    let (mut context, intrinsics, headers) = setup();
    let input = eval(
        &mut context,
        "globalThis.b = new Uint8Array([0,65,66,0]); new DataView(b.buffer,1,2)",
    );
    let init = intrinsics.convert(&input, &mut context).unwrap();
    eval(&mut context, "b[1]=67"); // init dictionary getters can mutate before extraction
    let extracted = intrinsics
        .extract(init, ExtractionOptions::default(), &mut context)
        .unwrap();
    eval(&mut context, "b[1]=68"); // extraction is a snapshot
    assert_eq!(extracted.content_type, None);
    assert_eq!(extracted.body.length(), Some(2));
    assert_eq!(
        string(consume(
            &intrinsics,
            Some(&extracted.body),
            ConsumptionKind::Text,
            &headers,
            &mut context
        )),
        "CB"
    );
}

#[test]
fn body_buffers_reject_shared_resizable_and_ignore_shadowed_getters() {
    let (mut context, intrinsics, _) = setup();
    for source in [
        "new ArrayBuffer(2,{maxByteLength:4})",
        "new Uint8Array(new SharedArrayBuffer(2))",
    ] {
        let value = eval(&mut context, source);
        assert!(intrinsics.convert(&value, &mut context).is_err());
    }
    let value = eval(
        &mut context,
        "Object.defineProperty(new ArrayBuffer(3),'resizable',{get(){throw Error('shadow')}})",
    );
    let init = intrinsics.convert(&value, &mut context).unwrap();
    JsArrayBuffer::from_object(value.as_object().unwrap())
        .unwrap()
        .detach(&JsValue::undefined())
        .unwrap();
    let body = intrinsics
        .extract(init, ExtractionOptions::default(), &mut context)
        .unwrap()
        .body;
    assert_eq!(body.length(), Some(0));
}

#[test]
fn body_url_search_params_preserves_line_endings_and_ignores_js_methods() {
    let (mut context, intrinsics, headers) = setup();
    let value = eval(
        &mut context,
        "let p=new URLSearchParams([['x','a\\nb'],['x','+']]); p.toString=()=>{throw Error('not called')}; p",
    );
    let body = extract(&intrinsics, value, &mut context);
    assert_eq!(
        body.content_type.as_deref(),
        Some("application/x-www-form-urlencoded;charset=UTF-8")
    );
    assert_eq!(
        string(consume(
            &intrinsics,
            Some(&body.body),
            ConsumptionKind::Text,
            &headers,
            &mut context
        )),
        "x=a%0Ab&x=%2B"
    );
}

#[test]
fn body_blob_brand_and_source_are_preserved() {
    let (mut context, intrinsics, headers) = setup();
    let value = eval(
        &mut context,
        "let blob=new File(['ok'],'f',{type:'TEXT/PLAIN'}); Object.defineProperty(blob,'type',{get(){throw Error('not called')}}); blob",
    );
    let body = extract(&intrinsics, value.clone(), &mut context);
    assert_eq!(body.content_type.as_deref(), Some("text/plain"));
    match body.body.source() {
        BodySource::Blob(ref object) => {
            assert!(JsObject::equals(object, &value.as_object().unwrap()))
        }
        _ => panic!("Lost Blob source"),
    }
    assert_eq!(
        string(consume(
            &intrinsics,
            Some(&body.body),
            ConsumptionKind::Text,
            &headers,
            &mut context
        )),
        "ok"
    );
}

#[test]
fn missing_body_is_repeatable_but_empty_stream_is_disturbed_and_locked() {
    let (mut context, intrinsics, headers) = setup();
    for _ in 0..2 {
        assert_eq!(
            string(consume(
                &intrinsics,
                None,
                ConsumptionKind::Text,
                &headers,
                &mut context
            )),
            ""
        );
    }
    let body = extract(&intrinsics, JsString::from("").into(), &mut context).body;
    assert!(!body.body_used().unwrap());
    let promise = intrinsics
        .consume(
            Some(&body),
            ConsumptionKind::Text,
            &headers,
            0,
            &mut context,
        )
        .unwrap();
    assert!(body.body_used().unwrap());
    assert!(body.unusable().unwrap());
    assert_eq!(string(settled(promise, &mut context).unwrap()), "");
    assert!(web_streams::is_locked(&body.stream().into()).unwrap());
    let second = intrinsics
        .consume(
            Some(&body),
            ConsumptionKind::Text,
            &headers,
            0,
            &mut context,
        )
        .unwrap();
    assert!(settled(second, &mut context).is_err());
}

#[test]
fn stream_extraction_keepalive_lock_and_invalid_chunks() {
    let (mut context, intrinsics, headers) = setup();
    let value = eval(
        &mut context,
        "globalThis.cancelled=false; new ReadableStream({start(c){c.enqueue('not bytes');c.close()},cancel(){cancelled=true}})",
    );
    let init = intrinsics.convert(&value, &mut context).unwrap();
    assert!(init.is_stream());
    assert!(
        intrinsics
            .extract(
                init,
                ExtractionOptions {
                    keepalive: true,
                    multipart_boundary: None
                },
                &mut context
            )
            .is_err()
    );
    let body = extract(&intrinsics, value.clone(), &mut context).body;
    assert_eq!(body.length(), None);
    let promise = intrinsics
        .consume(
            Some(&body),
            ConsumptionKind::Bytes,
            &headers,
            0,
            &mut context,
        )
        .unwrap();
    assert!(settled(promise, &mut context).is_err());
    assert!(web_streams::is_locked(&value).unwrap());
    assert_eq!(eval(&mut context, "cancelled").as_boolean(), Some(false));
    let init = intrinsics.convert(&value, &mut context).unwrap();
    assert!(
        intrinsics
            .extract(init, ExtractionOptions::default(), &mut context)
            .is_err()
    );
}

#[test]
fn body_clone_tees_actual_stream_and_replaces_original_stream() {
    let (mut context, intrinsics, headers) = setup();
    let body = extract(&intrinsics, JsString::from("clone 😀").into(), &mut context).body;
    let old = body.stream();
    let clone = body.clone_body(&mut context).unwrap();
    assert!(!JsObject::equals(&old, &body.stream()));
    assert!(web_streams::is_locked(&old.into()).unwrap());
    for body in [&body, &clone] {
        assert_eq!(
            string(consume(
                &intrinsics,
                Some(body),
                ConsumptionKind::Text,
                &headers,
                &mut context
            )),
            "clone 😀"
        );
        assert!(body.clone_body(&mut context).is_err());
    }
}

#[test]
fn body_json_uses_captured_ecmascript_intrinsic_and_utf8_bom_rules() {
    let (mut context, intrinsics, headers) = setup();
    eval(
        &mut context,
        "JSON.parse=()=>{throw Error('shadow')}; JSON.stringify=()=>{throw Error('shadow')}",
    );
    let body = extract(
        &intrinsics,
        JsString::from("\u{feff}{\"n\":-0,\"big\":1e999,\"s\":\"\\ud800\"}").into(),
        &mut context,
    )
    .body;
    let value = consume(
        &intrinsics,
        Some(&body),
        ConsumptionKind::Json,
        &headers,
        &mut context,
    );
    let object = value.as_object().unwrap();
    assert!(
        object
            .get(JsString::from("n"), &mut context)
            .unwrap()
            .as_number()
            .unwrap()
            .is_sign_negative()
    );
    assert!(
        object
            .get(JsString::from("big"), &mut context)
            .unwrap()
            .as_number()
            .unwrap()
            .is_infinite()
    );
    assert_eq!(
        object
            .get(JsString::from("s"), &mut context)
            .unwrap()
            .as_string()
            .unwrap()
            .iter()
            .collect::<Vec<_>>(),
        vec![0xd800]
    );
    assert_eq!(
        intrinsics
            .json_bytes(&JsValue::from(42), &mut context)
            .unwrap(),
        b"42"
    );
    assert!(
        intrinsics
            .json_bytes(&JsValue::undefined(), &mut context)
            .is_err()
    );
    let promise = intrinsics
        .consume(None, ConsumptionKind::Json, &headers, 0, &mut context)
        .unwrap();
    assert!(settled(promise, &mut context).is_err());
}

#[test]
fn body_blob_reads_live_headers_at_completion() {
    let (mut context, intrinsics, headers) = setup();
    let body = extract(&intrinsics, JsString::from("payload").into(), &mut context).body;
    let promise = intrinsics
        .consume(
            Some(&body),
            ConsumptionKind::Blob,
            &headers,
            0,
            &mut context,
        )
        .unwrap();
    let set = headers
        .get(JsString::from("set"), &mut context)
        .unwrap()
        .as_callable()
        .unwrap();
    set.call(
        &headers.clone().into(),
        &[
            JsString::from("Content-Type").into(),
            JsString::from("TEXT/HTML;Charset=UTF-8").into(),
        ],
        &mut context,
    )
    .unwrap();
    let blob = web_blob::snapshot(&settled(promise, &mut context).unwrap()).unwrap();
    assert_eq!(blob.media_type, "text/html;charset=utf-8");
    assert_eq!(blob.bytes, b"payload");
}

#[test]
fn body_multipart_native_roundtrip_and_urlencoded_decode() {
    let (mut context, intrinsics, _) = setup();
    let form = eval(
        &mut context,
        "let f=new FormData();f.append('x','a\\nb');f.append('x',new File(['file'],'a.txt',{type:'text/plain'}));f",
    );
    let extracted = extract(&intrinsics, form, &mut context);
    let mut list = Vec::new();
    append_content_type_if_missing(&mut list, extracted.content_type.as_deref()).unwrap();
    let headers = web_headers::from_header_list(list, &mut context).unwrap();
    let value = consume(
        &intrinsics,
        Some(&extracted.body),
        ConsumptionKind::FormData,
        &headers,
        &mut context,
    );
    let entries = web_form_data::snapshot(&value).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(matches!(&entries[0].value, web_form_data::EntryValue::Text(text) if text == "a\r\nb"));
    assert!(
        matches!(&entries[1].value, web_form_data::EntryValue::File(file) if file.name == "a.txt" && file.bytes == b"file" && file.last_modified == 123)
    );
    let headers = web_headers::from_header_list(
        vec![(
            b"content-type".to_vec(),
            b"application/x-www-form-urlencoded;charset=shift_jis".to_vec(),
        )],
        &mut context,
    )
    .unwrap();
    let body = extract(
        &intrinsics,
        JsString::from("x=%EF%BB%BF&x=%FF&z=+").into(),
        &mut context,
    )
    .body;
    let decoded = consume(
        &intrinsics,
        Some(&body),
        ConsumptionKind::FormData,
        &headers,
        &mut context,
    );
    let entries = web_form_data::snapshot(&decoded).unwrap();
    assert!(
        matches!(&entries[0].value, web_form_data::EntryValue::Text(text) if text == "\u{feff}")
    );
    assert!(
        matches!(&entries[1].value, web_form_data::EntryValue::Text(text) if text == "\u{fffd}")
    );
}

#[test]
fn mime_parser_preserves_whatwg_forgiving_parameters_and_quoting() {
    let mime = parse_mime_type(" Text/HTML ; Charset=\"UTF-8\"garbage; charset=latin1;bad =skip;x=\"a,b\\\"c\";empty=\"\" ").unwrap().unwrap();
    assert_eq!(mime.essence, "text/html");
    assert_eq!(mime.parameter("charset"), Some("UTF-8"));
    assert_eq!(mime.parameter("bad "), None);
    assert_eq!(mime.parameter("x"), Some("a,b\"c"));
    assert_eq!(mime.parameter("empty"), Some(""));
    assert_eq!(parse_mime_type(&mime.serialize()).unwrap(), Some(mime));
    for invalid in [
        "",
        "text",
        "text /html",
        "text/ html",
        "/html",
        "text/",
        "text/html,x",
    ] {
        assert_eq!(parse_mime_type(invalid).unwrap(), None);
    }
}

#[test]
fn body_multipart_internal_jobs_do_not_read_mutable_promise_then() {
    let (mut context, intrinsics, _) = setup();
    let form = eval(&mut context, "let f=new FormData();f.append('x','ok');f");
    let extracted = extract(&intrinsics, form, &mut context);
    let headers = web_headers::from_header_list(
        vec![(
            b"content-type".to_vec(),
            extracted.content_type.unwrap().into_bytes(),
        )],
        &mut context,
    )
    .unwrap();
    eval(
        &mut context,
        "Object.defineProperty(Promise.prototype,'then',{get(){throw Error('poisoned then')}}); Object.defineProperty(Promise.prototype,'constructor',{get(){throw Error('poisoned constructor')}})",
    );
    let value = consume(
        &intrinsics,
        Some(&extracted.body),
        ConsumptionKind::FormData,
        &headers,
        &mut context,
    );
    let entries = web_form_data::snapshot(&value).unwrap();
    assert!(matches!(&entries[0].value, web_form_data::EntryValue::Text(text) if text == "ok"));
}

#[test]
fn body_proxy_and_fresh_byte_results_use_actual_stream() {
    let (mut context, intrinsics, headers) = setup();
    let body = extract(&intrinsics, JsString::from("abc").into(), &mut context).body;
    let proxy = body.proxy(&mut context).unwrap();
    assert!(body.unusable().unwrap());
    let bytes = consume(
        &intrinsics,
        Some(&proxy),
        ConsumptionKind::Bytes,
        &headers,
        &mut context,
    );
    assert_eq!(
        web_streams::copy_uint8_chunk(&bytes, &mut context).unwrap(),
        b"abc"
    );
    let first = consume(
        &intrinsics,
        None,
        ConsumptionKind::ArrayBuffer,
        &headers,
        &mut context,
    )
    .as_object()
    .unwrap();
    let second = consume(
        &intrinsics,
        None,
        ConsumptionKind::ArrayBuffer,
        &headers,
        &mut context,
    )
    .as_object()
    .unwrap();
    assert!(!JsObject::equals(&first, &second));
    assert_eq!(JsArrayBuffer::from_object(first).unwrap().byte_length(), 0);
}

#[test]
fn body_stream_rejection_preserves_original_reason_and_reader_lock() {
    let (mut context, intrinsics, headers) = setup();
    let value = eval(
        &mut context,
        "globalThis.reason={cause:'native-test'};new ReadableStream({start(c){c.error(reason)}})",
    );
    let body = extract(&intrinsics, value.clone(), &mut context).body;
    let promise = intrinsics
        .consume(
            Some(&body),
            ConsumptionKind::Text,
            &headers,
            0,
            &mut context,
        )
        .unwrap();
    let error = settled(promise, &mut context)
        .unwrap_err()
        .as_object()
        .unwrap();
    let original = eval(&mut context, "reason").as_object().unwrap();
    assert!(JsObject::equals(&error, &original));
    assert!(body.body_used().unwrap());
    assert!(web_streams::is_locked(&value).unwrap());
}

#[test]
fn mime_header_selection_and_saved_charset_follow_fetch() {
    let headers = vec![(
        b"content-type".to_vec(),
        b"text/html;charset=first, text/html;charset=second, text/html; x=\"a,b\", */*, invalid"
            .to_vec(),
    )];
    let mime = extract_mime_type(&headers).unwrap().unwrap();
    assert_eq!(mime.parameter("charset"), Some("first"));
    assert_eq!(mime.parameter("x"), Some("a,b"));
    let mut headers = headers;
    headers.push((b"Content-Type".to_vec(), b"application/json".to_vec()));
    let mime = extract_mime_type(&headers).unwrap().unwrap();
    assert_eq!(mime.essence, "application/json");
    assert_eq!(mime.parameter("charset"), None);
    let mut headers = vec![(b"Content-Type".to_vec(), Vec::new())];
    append_content_type_if_missing(&mut headers, Some("text/plain")).unwrap();
    assert_eq!(headers.len(), 1);
    assert!(headers[0].1.is_empty());
}
