use super::*;
use boa_engine::{Source, builtins::promise::PromiseState};
fn context() -> Context {
    let mut c = Context::builder()
        .instructions_remaining(10_000_000)
        .build()
        .unwrap();
    crate::web_text::register(&mut c).unwrap();
    web_abort::register(&mut c).unwrap();
    super::super::register(&mut c).unwrap();
    writable::register(&mut c).unwrap();
    crate::web_transform::register(&mut c).unwrap();
    c
}
fn run(c: &mut Context, body: &str) {
    let source = format!(
        r#"(async()=>{{
      function assert(v){{if(!v)throw Error('Assertion failed');}}
      function eq(a,b){{if(JSON.stringify(a)!==JSON.stringify(b))throw Error(JSON.stringify({{actual:a,expected:b}}));}}
      function throws(fn,name='TypeError'){{try{{fn()}}catch(e){{eq(e.name,name);return e}}throw Error('Expected throw');}}
      async function rejects(p,reason='TypeError'){{try{{await p}}catch(e){{if(typeof reason==='string')eq(e.name,reason);else assert(e===reason);return e}}throw Error('Expected rejection');}}
      async function until(predicate){{for(let i=0;i<100;i++){{if(predicate())return;await Promise.resolve()}}throw Error('Expected progress');}}
      {body}
      return true;
    }})()"#
    );
    let value = c.eval(Source::from_bytes(&source)).unwrap();
    let promise = JsPromise::from_object(value.as_object().unwrap()).unwrap();
    c.run_jobs().unwrap();
    match promise.state() {
        PromiseState::Fulfilled(v) => assert_eq!(v.as_boolean(), Some(true)),
        PromiseState::Rejected(e) => panic!(
            "Pipe test rejected: {}",
            e.to_string(c).unwrap().to_std_string_escaped()
        ),
        PromiseState::Pending => panic!("Pipe test remained pending after draining jobs"),
    }
}
fn check(body: &str) {
    run(&mut context(), body)
}
#[test]
fn normal_pipe_preserves_identity_order_closes_and_unlocks() {
    check(
        r#"
    const one={},two={},out=[];let closed=0;
    const s=new ReadableStream({start(c){c.enqueue(one);c.enqueue(two);c.close()}});
    const w=new WritableStream({write(v){out.push(v)},close(){closed++}});
    const p=s.pipeTo(w);assert(s.locked&&w.locked);await p;
    assert(out[0]===one&&out[1]===two);eq(closed,1);assert(!s.locked&&!w.locked);
    eq(await s.getReader().read(),{done:true});await w.getWriter().closed;
    "#,
    )
}
#[test]
fn writer_backpressure_allows_queue_capacity_without_serializing_writes() {
    check(
        r#"
    let controller,finish;const seen=[],sizes=[];
    const s=new ReadableStream({start(c){controller=c;c.enqueue(1);c.enqueue(2);c.enqueue(3);c.close()}});
    const w=new WritableStream({write(v){seen.push(v);if(v===1)return new Promise(r=>finish=r)}},{highWaterMark:2,size(v){sizes.push(v);return 1}});
    const p=s.pipeTo(w);await until(()=>sizes.length===2);eq(sizes,[1,2]);eq(seen,[1]);
    for(let i=0;i<5;i++)await Promise.resolve();eq(sizes,[1,2]);finish();await p;eq(seen,[1,2,3]);
    "#,
    )
}
#[test]
fn source_error_propagates_and_abort_failure_overrides() {
    check(
        r#"
    const sourceError={},abortError={};let actual;
    const s=new ReadableStream({start(c){c.error(sourceError)}});
    const w=new WritableStream({abort(e){actual=e;throw abortError}});
    await rejects(s.pipeTo(w),abortError);assert(actual===sourceError);assert(!s.locked&&!w.locked);
    const a=new ReadableStream({start(c){c.error(sourceError)}}),b=new WritableStream();
    await rejects(a.pipeTo(b,{preventAbort:true}),sourceError);const writer=b.getWriter();await writer.write('still writable');await writer.close();
    "#,
    )
}
#[test]
fn destination_error_cancels_and_cancel_failure_overrides() {
    check(
        r#"
    const error={},cancelError={};let reason;
    const s=new ReadableStream({cancel(e){reason=e;throw cancelError}},{highWaterMark:0});
    const w=new WritableStream({start(c){c.error(error)}});
    await rejects(s.pipeTo(w),cancelError);assert(reason===error);assert(!s.locked&&!w.locked);
    let cancelled=0;const a=new ReadableStream({cancel(){cancelled++}},{highWaterMark:0});
    const b=new WritableStream({start(c){c.error(error)}});
    await rejects(a.pipeTo(b,{preventCancel:true}),error);eq(cancelled,0);await a.cancel();eq(cancelled,1);
    "#,
    )
}
#[test]
fn simultaneous_initial_errors_use_ordered_shutdown_and_action_override() {
    // https://streams.spec.whatwg.org/#readable-stream-pipe-to: forward error
    // propagation runs first, but shutdown-with-action step 6 replaces its error
    // when WritableStreamAbort rejects. FinishErroring step 11 rejects abort
    // with the stored destination error when it was already erroring.
    check(
        r#"
    const a={},b={};let controller,forwarded;
    const s=new ReadableStream({start(c){c.error(a)}});
    const w=new WritableStream({start(c){controller=c;c.signal.addEventListener('abort',()=>forwarded=c.signal.reason);c.error(b)}});
    const pending=s.pipeTo(w);
    assert(forwarded===a);assert(controller.signal.aborted);
    await rejects(pending,b);assert(!s.locked&&!w.locked);
    // Once fully errored, abort fulfills: the original source error survives.
    const s2=new ReadableStream({start(c){c.error(a)}}),w2=new WritableStream({start(c){c.error(b)}});
    const writer=w2.getWriter();await rejects(writer.closed,b);writer.releaseLock();
    await rejects(s2.pipeTo(w2),a);assert(!s2.locked&&!w2.locked);
    // Preventing abort also preserves the source error while the sink is erroring.
    const s3=new ReadableStream({start(c){c.error(a)}}),w3=new WritableStream({start(c){c.error(b)}});
    await rejects(s3.pipeTo(w3,{preventAbort:true}),a);assert(!s3.locked&&!w3.locked);
    "#,
    )
}
#[test]
fn destination_closed_or_closing_cancels_source() {
    check(
        r#"
    let reason,finish;const w=new WritableStream({close(){return new Promise(r=>finish=r)}});
    const writer=w.getWriter();const closing=writer.close();writer.releaseLock();
    const s=new ReadableStream({cancel(e){reason=e}},{highWaterMark:0});
    const error=await rejects(s.pipeTo(w));assert(reason===error);finish();await closing;
    const s2=new ReadableStream({cancel(e){reason=e}},{highWaterMark:0});const error2=await rejects(s2.pipeTo(w));assert(reason===error2);
    "#,
    )
}
#[test]
fn prevent_close_keeps_destination_open_and_releases_locks() {
    check(
        r#"
    const out=[];const s=new ReadableStream({start(c){c.enqueue(1);c.close()}}),w=new WritableStream({write(v){out.push(v)}});
    await s.pipeTo(w,{preventClose:true});assert(!s.locked&&!w.locked);
    const writer=w.getWriter();await writer.write(2);await writer.close();eq(out,[1,2]);
    "#,
    )
}
#[test]
fn preaborted_signal_propagates_before_initial_errors() {
    check(
        r#"
    const reason={},sourceError={};const signal=AbortSignal.abort(reason);let abort;
    const s=new ReadableStream({start(c){c.error(sourceError)}}),w=new WritableStream({abort(e){abort=e}});
    await rejects(s.pipeTo(w,{signal}),reason);assert(abort===reason);assert(!s.locked&&!w.locked);
    const s2=new ReadableStream({}, {highWaterMark:0}),w2=new WritableStream();
    await rejects(s2.pipeTo(w2,{signal,preventAbort:true,preventCancel:true}),reason);
    assert(!s2.locked&&!w2.locked);await w2.getWriter().write(1);await s2.cancel();
    "#,
    )
}
#[test]
fn abort_waits_for_write_then_starts_both_actions_and_ignores_event_stopping() {
    check(
        r#"
    const a=new AbortController(),reason={},events=[];let writeDone,abortDone,cancelDone;
    a.signal.addEventListener('abort',e=>{events.push('event');e.stopImmediatePropagation()});
    const s=new ReadableStream({start(c){c.enqueue(1)},cancel(e){assert(e===reason);events.push('cancel');return new Promise(r=>cancelDone=r)}},{highWaterMark:0});
    const w=new WritableStream({write(){events.push('write');return new Promise(r=>writeDone=r)},abort(e){assert(e===reason);events.push('abort');return new Promise(r=>abortDone=r)}});
    const p=s.pipeTo(w,{signal:a.signal});await until(()=>writeDone);a.abort(reason);
    eq(events,['write','event']);assert(s.locked&&w.locked);writeDone();
    await until(()=>abortDone&&cancelDone);eq(events,['write','event','abort','cancel']);
    let settled=false;p.catch(()=>settled=true);abortDone();await Promise.resolve();assert(!settled);cancelDone();
    await rejects(p,reason);assert(!s.locked&&!w.locked);
    "#,
    )
}
#[test]
fn abort_failure_overrides_reason_and_all_actions_are_started() {
    check(
        r#"
    const a=new AbortController(),reason={},failure={},seen=[];
    const s=new ReadableStream({cancel(){seen.push('cancel');return Promise.reject(failure)}},{highWaterMark:0});
    const w=new WritableStream({
      start(c){c.signal.addEventListener('abort',()=>{assert(c.signal.reason===reason);seen.push('abort-signal')})},
      abort(){seen.push('abort');return new Promise(()=>{})}
    });
    const p=s.pipeTo(w,{signal:a.signal});a.abort(reason);
    // WritableStreamAbort signals its controller before cancellation, but the
    // sink abort callback waits for the writable controller's start reaction.
    eq(seen,['abort-signal','cancel']);
    await rejects(p,failure);eq(seen,['abort-signal','cancel','abort']);assert(!s.locked&&!w.locked);
    "#,
    )
}
#[test]
fn pending_read_is_released_without_cancel_when_requested() {
    check(
        r#"
    let controller,cancelled=0;const a=new AbortController(),reason={};
    const s=new ReadableStream({start(c){controller=c},cancel(){cancelled++}},{highWaterMark:0});
    const w=new WritableStream();const p=s.pipeTo(w,{signal:a.signal,preventCancel:true});
    await Promise.resolve();a.abort(reason);await rejects(p,reason);eq(cancelled,0);
    controller.enqueue('retained');const reader=s.getReader();eq(await reader.read(),{value:'retained',done:false});reader.releaseLock();await s.cancel();
    "#,
    )
}
#[test]
fn byte_stream_default_reader_transfers_and_preserves_byte_views() {
    check(
        r#"
    let original;const seen=[];
    const s=new ReadableStream({type:'bytes',start(c){original=new Uint8Array([1,2,3]);c.enqueue(original);c.close()}});
    await s.pipeTo(new WritableStream({write(v){assert(v instanceof Uint8Array);seen.push(...v)}}));
    eq(original.byteLength,0);eq(seen,[1,2,3]);assert(!s.locked);
    "#,
    )
}
#[test]
fn no_public_stream_methods_or_thenable_chunk_assimilation() {
    check(
        r#"
    let touched=0;const chunk={get then(){touched++;throw Error('chunk assimilated')}};let seen;
    const s=new ReadableStream({start(c){c.enqueue(chunk);c.close()}}),w=new WritableStream({write(v){seen=v}});
    s.getReader=()=>{throw Error('public getReader')};w.getWriter=()=>{throw Error('public getWriter')};
    const old=Object.getOwnPropertyDescriptor(Object.prototype,'then');
    Object.defineProperty(Object.prototype,'then',{configurable:true,get(){if(Object.prototype.hasOwnProperty.call(this,'done'))throw Error('read result assimilated');return undefined}});
    try{await s.pipeTo(w)}finally{if(old)Object.defineProperty(Object.prototype,'then',old);else delete Object.prototype.then}
    assert(seen===chunk);eq(touched,0);
    "#,
    )
}
#[test]
fn dictionary_conversion_order_and_lock_failures() {
    check(
        r#"
    const seen=[],s=new ReadableStream({}, {highWaterMark:0}),w=new WritableStream(),reader=s.getReader();
    const options=new Proxy({}, {get(o,k){seen.push(k);return undefined}});
    const p=s.pipeTo(w,options);await rejects(p);eq(seen,['preventAbort','preventCancel','preventClose','signal']);reader.releaseLock();
    await rejects(s.pipeTo(w,{signal:null}));await rejects(ReadableStream.prototype.pipeTo.call({},w));
    let touched=false;await rejects(s.pipeTo({}, {get preventAbort(){touched=true}}));assert(!touched);
    const writer=w.getWriter();await rejects(s.pipeTo(w));assert(!s.locked);writer.releaseLock();await s.cancel();
    "#,
    )
}
#[test]
fn pipe_through_pair_order_identity_and_transform_pipeline() {
    check(
        r#"
    const order=[],t=new TransformStream({transform(v,c){c.enqueue(v*2)}});
    const source=new ReadableStream({start(c){c.enqueue(2);c.enqueue(3);c.close()}});
    const pair={get readable(){order.push('readable');return t.readable},get writable(){order.push('writable');return t.writable}};
    const options=new Proxy({}, {get(o,k){order.push(k);return undefined}});
    const output=source.pipeThrough(pair,options);assert(output===t.readable);
    eq(order,['readable','writable','preventAbort','preventCancel','preventClose','signal']);
    const result=[];await output.pipeTo(new WritableStream({write(v){result.push(v)}}));eq(result,[4,6]);
    assert(!source.locked&&!t.writable.locked&&!output.locked);
    throws(()=>source.pipeThrough({readable:{},get writable(){throw Error('must not read')}}));
    "#,
    )
}
#[test]
fn source_close_waits_inflight_write_and_close_failure_replaces_success() {
    check(
        r#"
    let finish,closeCalled=false;const reason={};
    const s=new ReadableStream({start(c){c.enqueue(1);c.close()}});
    const w=new WritableStream({write(){return new Promise(r=>finish=r)},close(){closeCalled=true;throw reason}});
    const p=s.pipeTo(w);await until(()=>finish);assert(!closeCalled);assert(s.locked&&w.locked);finish();
    await rejects(p,reason);assert(closeCalled);assert(!s.locked&&!w.locked);
    "#,
    )
}
#[test]
fn native_pipe_marks_disturbed_even_with_no_reads_and_gc_keeps_locks() {
    let mut c = context();
    c.eval(Source::from_bytes("globalThis.s=new ReadableStream({}, {highWaterMark:0});globalThis.w=new WritableStream();globalThis.a=new AbortController();globalThis.p=s.pipeTo(w,{signal:a.signal,preventAbort:true,preventCancel:true});")).unwrap();
    let source = c.global_object().get(JsString::from("s"), &mut c).unwrap();
    assert!(super::super::is_disturbed(&source).unwrap());
    boa_gc::force_collect();
    run(
        &mut c,
        r#"const reason={};a.abort(reason);await rejects(p,reason);assert(!s.locked&&!w.locked);await s.cancel();await w.getWriter().close();"#,
    );
}
