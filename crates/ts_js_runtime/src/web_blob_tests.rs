use super::*;
use boa_engine::{Source, builtins::promise::PromiseState};

fn context() -> Context {
    let mut context = Context::builder()
        .instructions_remaining(10_000_000)
        .build()
        .unwrap();
    crate::web_streams::register(&mut context).unwrap();
    register(&mut context).unwrap();
    context
        .register_global_builtin_callable(
            JsString::from("detachBuffer"),
            1,
            NativeFunction::from_fn_ptr(|_, args, _| {
                let buffer = JsArrayBuffer::from_object(
                    args.get_or_undefined(0)
                        .as_object()
                        .ok_or_else(|| typ("Buffer required"))?,
                )?;
                buffer.detach(&JsValue::undefined())?;
                Ok(JsValue::undefined())
            }),
        )
        .unwrap();
    context
}
fn check(body: &str) {
    let mut context = context();
    let source = format!(
        r#"(async()=>{{
        function eq(a,b) {{if(JSON.stringify(a)!==JSON.stringify(b)) throw new Error(JSON.stringify({{actual:a,expected:b}}));}}
        function throws(fn,kind=TypeError) {{try{{fn()}}catch(e){{if(e instanceof kind)return e;throw e}}throw new Error('Expected exception')}}
        async function rejects(p) {{try{{await p}}catch(e){{if(e instanceof TypeError)return;throw e}}throw new Error('Expected rejection')}}
        {body}
        return true;
    }})()"#
    );
    let result = context.eval(Source::from_bytes(&source)).unwrap();
    let promise = JsPromise::from_object(result.as_object().unwrap()).unwrap();
    context.run_jobs().unwrap();
    match promise.state() {
        PromiseState::Fulfilled(value) => assert_eq!(value.as_boolean(), Some(true)),
        PromiseState::Rejected(value) => panic!(
            "Blob test rejected: {}",
            value.to_string(&mut context).unwrap().to_std_string_lossy()
        ),
        state => panic!("Blob test did not fulfill: {state:?}"),
    }
}
#[test]
fn blob_streams_are_fresh_byte_streams_over_immutable_snapshots() {
    check(
        r#"
        const blob=new Blob(['ABC']);
        const first=blob.stream(), second=blob.stream();
        eq(first===second,false);
        const reader=first.getReader({mode:'byob'});
        const buffer=new Uint8Array(2);
        const pending=reader.read(buffer);
        eq(buffer.byteLength,0);
        const value=(await pending).value;
        eq([...value],[65,66]);
        value[0]=0;
        eq([...(await reader.read(new Uint8Array(2))).value],[67]);
        eq((await reader.read(new Uint8Array(2))).done,true);
        eq([...(await second.getReader().read()).value],[65,66,67]);
        eq(await blob.text(),'ABC');
        eq((await new Blob().stream().getReader({mode:'byob'}).read(new Uint8Array(2))).done,true);
        eq([...(await new File(['Z'],'z').stream().getReader().read()).value],[90]);
        throws(()=>Blob.prototype.stream.call({}));
    "#,
    );
}
#[test]
fn blob_string_parts_and_usv_conversion() {
    check(
        r#"
        const blob = new Blob(['A\ud800',42,null,undefined,true,12n]);
        eq(await blob.text(),'A\ufffd42nullundefinedtrue12');
        eq(blob.size,25);
        eq(new Blob().size,0);
        throws(()=>new Blob('abc'));
        throws(()=>new Blob(null));
        throws(()=>new Blob([Symbol()]));
        throws(()=>Blob());
    "#,
    );
}
#[test]
fn blob_copies_exact_views_and_nested_blobs() {
    check(
        r#"
        const source = new Uint8Array([0,65,66,67,0]);
        const blob = new Blob([source.subarray(1,3),new DataView(source.buffer,3,1),new Blob(['D'],{type:'text/html'})]);
        source.fill(90);
        eq(await blob.text(),'ABCD');
        eq(blob.type,'');
        const a = await blob.bytes(); a[0]=0;
        const b = new Uint8Array(await blob.arrayBuffer()); b[1]=0;
        eq([...(await blob.bytes())],[65,66,67,68]);
        eq(a instanceof Uint8Array,true);
        eq(new Blob([new Uint16Array([0x1234])]).size,2);
    "#,
    );
}
#[test]
fn blob_type_and_dictionary_conversion_order() {
    check(
        r#"
        const order=[];
        const blob = new File([{toString(){order.push('part');return 'a'}}],{toString(){order.push('name');return 'file'}},{
            get endings(){order.push('endings');return 'transparent'},
            get lastModified(){order.push('lastModified');return -1.9},
            get type(){order.push('type');return 'TeXT/PLAIN; X=A'}
        });
        eq(order,['part','name','endings','type','lastModified']);
        eq(blob.type,'text/plain; x=a');eq(blob.lastModified,-1);
        eq(new Blob([],{type:'é'}).type,'');eq(new Blob([],{type:'a\nb'}).type,'');
        eq(new Blob([],{type:null}).type,'null');
        throws(()=>new Blob([],{endings:'Native'}));throws(()=>new Blob([],3));
    "#,
    );
}
#[test]
fn blob_buffers_are_copied_after_options_and_detachment_is_empty() {
    check(
        r#"
        const a = new Uint8Array([65]);
        const first = new Blob([a],{get type(){a[0]=66;return ''}});
        eq(await first.text(),'B');
        const b = new Uint8Array([67]);const view=new DataView(b.buffer);
        const second = new Blob([b,view,b.buffer],{get type(){detachBuffer(b.buffer);return ''}});
        eq(second.size,0);
        eq(new Blob([b,view,b.buffer]).size,0);
    "#,
    );
}
#[test]
fn blob_buffer_brand_and_resizable_rules_ignore_shadowed_getters() {
    check(
        r#"
        const buffer = new ArrayBuffer(1,{maxByteLength:4});
        Object.defineProperty(buffer,'resizable',{value:false});
        throws(()=>new Blob([buffer]));throws(()=>new Blob([new Uint8Array(buffer)]));
        throws(()=>new Blob([new DataView(buffer)]));
        throws(()=>new Blob([new Uint8Array(new SharedArrayBuffer(1))]));
        const bytes = new Uint8Array([65]);
        for(const key of ['buffer','byteOffset','byteLength']) Object.defineProperty(bytes,key,{get(){throw new Error('shadow read')}});
        eq(await new Blob([bytes]).text(),'A');
    "#,
    );
}
#[test]
fn blob_slice_long_long_and_relative_offsets() {
    check(
        r#"
        const b=new Blob(['abcdef'],{type:'text/plain'});
        eq(await b.slice(-4,-1,'APPLICATION/JSON').text(),'cde');
        eq(b.slice().type,'');eq(b.slice(2,1).size,0);
        eq(await b.slice(Infinity).text(),'abcdef');eq(b.slice(0,Infinity).size,0);
        eq(await b.slice(2.9,5.9).text(),'cde');
        eq(await b.slice(18446744073709551616).text(),'abcdef');
        eq(b.slice(-9223372036854775808,9223372036854775808).size,0);
        throws(()=>b.slice(1n));
        eq(await b.slice(1,5).slice(1,3).text(),'cd');
    "#,
    );
}
#[test]
fn blob_iterator_conversion_does_not_close_on_abrupt() {
    check(
        r#"
        let closed=false, visited=0;
        function* parts(){try{yield {toString(){throw new Error('bad')}};visited++}finally{closed=true}}
        throws(()=>new Blob(parts()),Error);eq(closed,false);eq(visited,0);
        let seen=[];
        const iterable = {
            [Symbol.iterator]() {
                let i=0;
                return {
                    next() {
                        seen.push('next');
                        return i++ ? {done:true} : {
                            done:false,
                            value:{toString(){seen.push('convert');return 'x'}}
                        };
                    }
                };
            }
        };
        eq(await new Blob(iterable).text(),'x');eq(seen,['next','convert','next']);
    "#,
    );
}
#[test]
fn file_inherits_blob_and_preserves_file_metadata() {
    check(
        r#"
        const file = new File(['abc'],'a/b\\c\ud800',{lastModified:-3,type:'IMAGE/SVG+XML'});
        eq(file instanceof Blob,true);eq(file instanceof File,true);
        eq(Object.getPrototypeOf(File)===Blob,true);eq(Object.getPrototypeOf(File.prototype)===Blob.prototype,true);
        eq(file.name,'a/b\\c\ufffd');eq(file.webkitRelativePath,'');eq(file.lastModified,-3);
        eq(file.type,'image/svg+xml');eq(await file.text(),'abc');
        const slice=file.slice();eq(slice instanceof File,false);eq(slice instanceof Blob,true);
        class Sub extends File {} const sub=new Sub(['z'],'z');eq(sub instanceof Sub,true);eq(await sub.text(),'z');
        throws(()=>new File());throws(()=>new File([]));throws(()=>new File(undefined,'x'));
        eq(Object.prototype.toString.call(file),'[object File]');
        eq(typeof file.stream,'function');
    "#,
    );
}
#[test]
fn blob_text_utf8_bom_replacement_and_promise_brand_errors() {
    check(
        r#"
        const blob=new Blob([new Uint8Array([239,187,191,65,239,187,191,255])],{type:'text/plain;charset=windows-1252'});
        const promise=blob.text();eq(promise instanceof Promise,true);
        eq(await promise,'A\ufeff\ufffd');
        await rejects(Blob.prototype.text.call({}));
        await rejects(Blob.prototype.bytes.call(Object.create(Blob.prototype)));
        await rejects(Blob.prototype.arrayBuffer.call(null));
        throws(()=>Blob.prototype.slice.call({}));
        throws(()=>Object.getOwnPropertyDescriptor(File.prototype,'name').get.call(blob));
        eq(Object.getOwnPropertyDescriptor(Blob.prototype,'text').enumerable,true);
    "#,
    );
}
#[test]
fn blob_native_endings_only_transform_string_parts() {
    let expected = if cfg!(windows) {
        "a\r\nb\r\nc\r\n"
    } else {
        "a\nb\nc\n"
    };
    let mut context = context();
    let result=context.eval(Source::from_bytes("new Blob(['a\\rb\\r\\nc\\n',new Uint8Array([13]),new Blob(['\\r'])],{endings:'native'})")).unwrap();
    let result = snapshot(&result).unwrap();
    assert_eq!(result.bytes, [expected.as_bytes(), b"\r\r"].concat());
}
#[test]
fn native_blob_helpers_preserve_metadata_and_apply_bounds() {
    let mut context = context();
    let object = file_from_bytes(vec![0, 255], "TEXT/PLAIN", "file", -123, &mut context).unwrap();
    assert_eq!(
        snapshot(&object.into()),
        Some(BlobSnapshot {
            bytes: vec![0, 255],
            media_type: "text/plain".into(),
            file: Some(FileMetadata {
                name: "file".into(),
                last_modified: -123,
                webkit_relative_path: String::new()
            })
        })
    );
    let blob = from_bytes(vec![65], "é", &mut context).unwrap();
    assert_eq!(snapshot(&blob.into()).unwrap().media_type, "");
    assert!(from_bytes(vec![0; MAX_BYTES + 1], "", &mut context).is_err());
    assert!(snapshot(&JsValue::null()).is_none());
}
#[test]
fn blob_registration_is_explicit_and_rejects_conflicts() {
    let mut context = Context::builder()
        .instructions_remaining(10_000_000)
        .build()
        .unwrap();
    assert_eq!(
        context
            .eval(Source::from_bytes("typeof Blob"))
            .unwrap()
            .as_string()
            .unwrap()
            .to_std_string_lossy(),
        "undefined"
    );
    context
        .eval(Source::from_bytes("globalThis.File=123"))
        .unwrap();
    assert!(register(&mut context).is_err());
    assert_eq!(
        context
            .eval(Source::from_bytes("typeof Blob"))
            .unwrap()
            .as_string()
            .unwrap()
            .to_std_string_lossy(),
        "undefined"
    );
}

#[test]
fn blob_webidl_long_long_preserves_signed_precision_and_wraps() {
    let mut context = context();
    for (number, expected) in [
        (-1.9, -1),
        (-3.0, -3),
        (-0.5, 0),
        (-9223372036854775808.0, i64::MIN),
        (9223372036854775808.0, i64::MIN),
        (18446744073709549568.0, -2048),
        (-18446744073709549568.0, 2048),
        (18446744073709551616.0, 0),
        (-18446744073709551616.0, 0),
        (f64::INFINITY, 0),
        (f64::NAN, 0),
    ] {
        assert_eq!(
            long_long(&JsValue::from(number), &mut context).unwrap(),
            expected,
            "{number}"
        );
    }
}
