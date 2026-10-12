//! Added byte/BYOB and Fetch-primitives cases; the earlier default stream cases
//! remain in web_streams_tests.rs (only obsolete bytes-unsupported assertion changed).
use super::*;
#[test]
fn bytes_enqueue_detaches_entire_buffer_and_preserves_slice() {
    check(
        r#"
        let c;const stream=new ReadableStream({type:'bytes',start(v){c=v}});
        assert(c instanceof ReadableByteStreamController);eq(c.desiredSize,0);eq(c.byobRequest,null);
        const input=new Uint8Array([0,1,2,3]);c.enqueue(input.subarray(1,3));eq(input.byteLength,0);
        eq(c.desiredSize,-2);c.close();const r=stream.getReader();const first=await r.read();
        assert(first.value instanceof Uint8Array);eq([...first.value],[1,2]);eq(first.value.byteOffset,1);
        eq(first.value.buffer.byteLength,4);eq(await r.read(),{done:true});
        throws(()=>new ReadableStream({type:'bytes'},{size(){return 1}}),'RangeError');
        throws(()=>new ReadableStream({type:'bytes',autoAllocateChunkSize:0}));
        throws(()=>new ReadableByteStreamController());throws(()=>new ReadableStreamBYOBRequest());
    "#,
    )
}
#[test]
fn byob_minimum_alignment_and_multiple_queue_entries() {
    check(
        r#"
        let c;const s=new ReadableStream({type:'bytes',start(v){c=v}}),r=s.getReader({mode:'byob'});
        assert(r instanceof ReadableStreamBYOBReader);const buffer=new ArrayBuffer(12),view=new Uint16Array(buffer,2,4);
        const pending=r.read(view,{min:2});eq(buffer.byteLength,0);
        let settled=false;pending.then(()=>settled=true);
        c.enqueue(new Uint8Array([1]));await Promise.resolve();assert(!settled);
        c.enqueue(new Uint8Array([2,3,4,5]));const result=await pending;
        assert(result.value instanceof Uint16Array);eq(result.value.byteOffset,2);eq(result.value.length,2);
        eq([...new Uint8Array(result.value.buffer,2,4)],[1,2,3,4]);eq(result.done,false);
        const last=await r.read(new DataView(new ArrayBuffer(4)));assert(last.value instanceof DataView);
        eq(last.value.byteLength,1);eq(last.value.getUint8(0),5);c.close();
        const done=await r.read(new Uint8Array(2));eq(done.done,true);eq(done.value.length,0);await r.closed;
    "#,
    )
}
#[test]
fn byob_request_respond_invalidates_and_remainder_is_retained() {
    check(
        r#"
        let c;const s=new ReadableStream({type:'bytes',start(v){c=v}}),r=s.getReader({mode:'byob'});
        const p=r.read(new Uint16Array(3));const request=c.byobRequest,view=request.view;
        assert(request===c.byobRequest);view.set([9,8,7]);request.respond(3);
        eq(request.view,null);eq(view.byteLength,0);throws(()=>request.respond(1));
        const v=(await p).value;eq([...new Uint8Array(v.buffer,0,v.byteLength)],[9,8]);
        eq([...(await r.read(new Uint8Array(2))).value],[7]);
        const pending=r.read(new Uint8Array(2));const closing=c.byobRequest;
        c.close();await r.closed;let settled=false;pending.then(()=>settled=true);await Promise.resolve();assert(!settled);
        closing.respond(0);const done=await pending;eq(done.done,true);eq(done.value.length,0);eq(closing.view,null);
    "#,
    )
}
#[test]
fn byob_new_view_after_transfer_and_intrinsic_constructor() {
    let mut c = context();
    // Boa 0.22 gates the JS ArrayBuffer.transfer method behind experimental.
    // Exercise real host transfer with its public detach/re-home API instead;
    // retain the buffer-ownership and replacement-view assertions below.
    c.register_global_builtin_callable(
        JsString::from("transferForTest"),
        1,
        NativeFunction::from_fn_ptr(|_, args, c| {
            let buffer = JsArrayBuffer::from_object(obj(args.get_or_undefined(0))?)?;
            let bytes = buffer.detach(&JsValue::undefined())?;
            Ok(JsArrayBuffer::from_byte_block(bytes, c)?.into())
        }),
    )
    .unwrap();
    run(
        &mut c,
        r#"
        let c;const s=new ReadableStream({type:'bytes',start(v){c=v}}),r=new ReadableStreamBYOBReader(s);
        const v=new Uint8Array(8);Object.defineProperty(v,'constructor',{get(){throw Error('not an intrinsic')}});
        const p=r.read(v);const request=c.byobRequest;
        const transferred=transferForTest(request.view.buffer);eq(request.view.byteLength,0);const replacement=new Uint8Array(transferred,0,3);replacement.set([1,2,3]);
        request.respondWithNewView(replacement);eq(replacement.byteLength,0);eq([...(await p).value],[1,2,3]);
        const p2=r.read(new Uint8Array(4));const q=c.byobRequest;
        throws(()=>q.respondWithNewView(new Uint8Array(2)),'RangeError');
        q.view[0]=4;q.respond(1);eq([...(await p2).value],[4]);c.close();
    "#,
    )
}
#[test]
fn byte_auto_allocate_and_byob_release_recovery() {
    check(
        r#"
        let pullCount=0;const source={type:'bytes',autoAllocateChunkSize:8,pull(c){pullCount++;const q=c.byobRequest;assert(q);q.view.set([1,2]);q.respond(2);c.close()}};
        const s=new ReadableStream(source);const r=s.getReader();eq([...(await r.read()).value],[1,2]);eq(await r.read(),{done:true});eq(pullCount,1);
        let c;const second=new ReadableStream({type:'bytes',start(v){c=v}}),first=second.getReader({mode:'byob'});
        const pending=first.read(new Uint8Array(4));const request=c.byobRequest;first.releaseLock();await rejects(pending,'TypeError');
        const next=second.getReader();const result=next.read();request.view.set([4,5]);request.respond(2);
        eq([...(await result).value],[4,5]);c.close();eq(await next.read(),{done:true});
    "#,
    )
}
#[test]
fn byte_errors_cancel_and_partial_element_close() {
    check(
        r#"
        let c,reason;const s=new ReadableStream({type:'bytes',start(v){c=v},cancel(v){reason=v}}),r=s.getReader({mode:'byob'});
        const p=r.read(new Uint8Array(2)),request=c.byobRequest;const marker={};await r.cancel(marker);
        assert(reason===marker);eq(await p,{done:true});eq(request.view,null);
        let d;const broken=new ReadableStream({type:'bytes',start(v){d=v}}),br=broken.getReader({mode:'byob'});
        const pending=br.read(new Uint16Array(1));d.enqueue(new Uint8Array([1]));throws(()=>d.close());await rejects(pending,'TypeError');await rejects(br.closed,'TypeError');
        const invalid=new ReadableStream({type:'bytes'}).getReader({mode:'byob'});
        await rejects(invalid.read(new Uint8Array(0)),'TypeError');await rejects(invalid.read(new Uint8Array(2),{min:3}),'RangeError');
        await rejects(invalid.read(new Uint8Array(2),{min:0}),'RangeError');
    "#,
    )
}
#[test]
fn byte_tee_clones_buffers_and_forwards_byob_only_sources() {
    check(
        r#"
        let calls=0;const source=new ReadableStream({type:'bytes',pull(c){calls++;const request=c.byobRequest;
            assert(request);request.view.set([1,2]);request.respond(2);c.close();}});
        const [a,b]=source.tee();const ra=a.getReader({mode:'byob'}),rb=b.getReader();
        const first=await ra.read(new Uint8Array(8));first.value[0]=99;
        const second=await rb.read();eq([...second.value],[1,2]);assert(second.value.buffer!==first.value.buffer);
        const da=await ra.read(new Uint8Array(1));eq(da.done,true);eq(da.value.length,0);eq(await rb.read(),{done:true});eq(calls,1);
        let reasons;const [x,y]=new ReadableStream({type:'bytes',cancel(v){reasons=v}}).tee();
        await Promise.all([x.cancel('left'),y.cancel('right')]);eq(reasons,['left','right']);
    "#,
    )
}
#[test]
fn native_byte_source_supports_byob_and_fetch_read_keeps_lock() {
    let mut c = context();
    let stream = from_bytes(vec![1, 2, 3, 4, 5], &mut c).unwrap();
    c.register_global_property(JsString::from("nativeBytes"), stream, Attribute::all())
        .unwrap();
    run(
        &mut c,
        r#"
        const r=nativeBytes.getReader({mode:'byob'});let v=new Uint8Array(3);const p=r.read(v);eq(v.byteLength,0);
        eq([...(await p).value],[1,2,3]);eq([...(await r.read(new Uint8Array(3))).value],[4,5]);
        eq((await r.read(new Uint8Array(3))).done,true);
    "#,
    );
    let stream = from_bytes(vec![7, 8], &mut c).unwrap();
    let promise = fully_read_bytes(&stream.clone().into(), &mut c).unwrap();
    c.run_jobs().unwrap();
    let PromiseState::Fulfilled(value) = promise.state() else {
        panic!("Fetch byte collection did not fulfill")
    };
    assert_eq!(copy_uint8_chunk(&value, &mut c).unwrap(), vec![7, 8]);
    assert!(is_locked(&stream.clone().into()).unwrap());
    assert!(is_disturbed(&stream.into()).unwrap());
    let stream=c.eval(Source::from_bytes("globalThis.canceled=false;new ReadableStream({start(c){c.enqueue('bad')},cancel(){canceled=true}})")).unwrap();
    let failed = fully_read_bytes(&stream, &mut c).unwrap();
    c.run_jobs().unwrap();
    assert!(matches!(failed.state(), PromiseState::Rejected(_)));
    assert!(is_locked(&stream).unwrap());
    assert_eq!(
        c.eval(Source::from_bytes("canceled")).unwrap().as_boolean(),
        Some(false)
    );
}
#[test]
fn fetch_tee_preserves_typed_view_offsets_and_clones_backing_storage() {
    let mut c = context();
    let stream=c.eval(Source::from_bytes("new ReadableStream({start(c){const b=new Uint8Array([4,5,6,7]);c.enqueue(b.subarray(1,3));c.close()}})")).unwrap();
    let (left, right) = tee_for_fetch(&stream, &mut c).unwrap();
    c.register_global_property(JsString::from("left"), left, Attribute::all())
        .unwrap();
    c.register_global_property(JsString::from("right"), right, Attribute::all())
        .unwrap();
    run(
        &mut c,
        r#"
        const a=(await left.getReader().read()).value,b=(await right.getReader().read()).value;
        eq([...a],[5,6]);eq([...b],[5,6]);eq(b.byteOffset,1);eq(b.buffer.byteLength,4);
        a[0]=99;eq(b[0],5);assert(a.buffer!==b.buffer);
    "#,
    );
}
#[test]
fn identity_proxy_immediately_transfers_and_propagates_cancel() {
    let mut c = context();
    let stream=c.eval(Source::from_bytes("globalThis.pulls=0;globalThis.reason=undefined;new ReadableStream({pull(c){c.enqueue(++pulls)},cancel(v){reason=v}},{highWaterMark:0})")).unwrap();
    let output = identity_proxy(&stream, &mut c).unwrap();
    assert!(is_locked(&stream).unwrap());
    assert!(is_disturbed(&stream).unwrap());
    c.run_jobs().unwrap();
    assert_eq!(
        c.eval(Source::from_bytes("pulls")).unwrap().as_number(),
        Some(1.0)
    );
    c.register_global_property(JsString::from("proxied"), output, Attribute::all())
        .unwrap();
    run(
        &mut c,
        r#"const r=proxied.getReader();eq((await r.read()).value,1);const why={};await r.cancel(why);assert(reason===why);"#,
    );
    assert!(!is_locked(&stream).unwrap());
}
