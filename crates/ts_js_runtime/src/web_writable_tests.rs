use super::*;
use boa_engine::Source;
fn context() -> Context {
    let mut c = Context::builder()
        .instructions_remaining(10_000_000)
        .build()
        .unwrap();
    crate::web_text::register(&mut c).unwrap();
    crate::web_abort::register(&mut c).unwrap();
    register(&mut c).unwrap();
    c
}
fn run(c: &mut Context, body: &str) {
    let source = format!(
        r#"(async()=>{{
    function assert(v){{if(!v)throw Error('Assertion failed');}}
    function eq(a,b){{if(JSON.stringify(a)!==JSON.stringify(b))throw Error(JSON.stringify({{actual:a,expected:b}}));}}
    function throws(fn,name='TypeError'){{try{{fn()}}catch(e){{eq(e.name,name);return e}}throw Error('Expected throw');}}
    async function rejects(p,reason='TypeError'){{try{{await p}}catch(e){{if(typeof reason==='string')eq(e.name,reason);else assert(e===reason);return e}}throw Error('Expected rejection');}}
    const tick=()=>Promise.resolve();
    {body}
    return true;
    }})()"#
    );
    let value = c.eval(Source::from_bytes(&source)).unwrap();
    let p = JsPromise::from_object(value.as_object().unwrap()).unwrap();
    c.run_jobs().unwrap();
    match p.state() {
        PromiseState::Fulfilled(value) => assert_eq!(value.as_boolean(), Some(true)),
        PromiseState::Rejected(e) => panic!(
            "Writable test rejected: {}",
            e.to_string(c).unwrap().to_std_string_escaped()
        ),
        PromiseState::Pending => panic!("Writable test remained pending after jobs drained"),
    }
}
fn check(body: &str) {
    run(&mut context(), body)
}

#[test]
fn constructor_conversion_order_and_start_exception() {
    check(
        r#"
    const log=[];
    const strategy={get highWaterMark(){log.push('hwm');return {valueOf(){log.push('number');return 2}}},get size(){log.push('size');return x=>x.length}};
    const sink=new Proxy({start(c){log.push('start:call');assert(this===sink);assert(c instanceof WritableStreamDefaultController);assert(c.signal instanceof AbortSignal)}},{get(t,k){log.push(k);return t[k]}});
    new WritableStream(sink,strategy);
    eq(log,['hwm','number','size','abort','close','start','type','write','start:call']);
    const reason={};try{new WritableStream({start(){throw reason}});throw Error('missing')}catch(e){assert(e===reason)}
    throws(()=>new WritableStream({type:'bytes'}),'RangeError');throws(()=>new WritableStream({type:null}),'RangeError');
    throws(()=>new WritableStream({write:null}));throws(()=>new WritableStream({}, {size:null}));
    throws(()=>new WritableStream({}, {highWaterMark:-1}),'RangeError');throws(()=>new WritableStream({}, {highWaterMark:NaN}),'RangeError');
    eq(new WritableStream({}, {highWaterMark:Infinity}).getWriter().desiredSize,Infinity);
"#,
    )
}

#[test]
fn queued_writes_are_serial_and_ready_tracks_backpressure() {
    check(
        r#"
    let finish;const events=[];
    const sink={write(chunk,c){assert(this===sink);assert(c instanceof WritableStreamDefaultController);events.push(chunk);return new Promise(resolve=>finish=resolve)},close(){assert(this===sink);events.push('close')}};
    const s=new WritableStream(sink,{highWaterMark:3,size(chunk){return chunk.length}}),w=s.getWriter();
    assert(s.locked);throws(()=>s.getWriter());await rejects(s.close());await rejects(s.abort());
    const initial=w.ready;await initial;
    const first=w.write('ab');eq(w.desiredSize,1);assert(w.ready===initial);
    const second=w.write('cd');eq(w.desiredSize,-1);const pressure=w.ready;assert(pressure!==initial);
    let ready=false;pressure.then(()=>ready=true);await tick();eq(events,['ab']);assert(!ready);
    finish();await first;await pressure;assert(ready);eq(events,['ab','cd']);eq(w.desiredSize,1);
    finish();await second;eq(w.desiredSize,3);
    await w.close();await w.closed;eq(events,['ab','cd','close']);eq(w.desiredSize,0);
    await rejects(w.write('x'));await rejects(w.close());await w.abort('ignored');w.releaseLock();assert(!s.locked);await s.abort();await rejects(s.close());
"#,
    )
}

#[test]
fn start_promise_gates_writes_and_rejection_preserves_reason() {
    check(
        r#"
    let start,called=0;const s=new WritableStream({start(){return new Promise(r=>start=r)},write(){called++}}),w=s.getWriter();
    const p=w.write(1);await tick();eq(called,0);start();await p;eq(called,1);await w.close();
    const reason={start:'failed'};let rejectStart;
    const bad=new WritableStream({start(){return new Promise((_,r)=>rejectStart=r)}}),bw=bad.getWriter();
    const writing=bw.write(1),closing=bw.close();rejectStart(reason);
    await rejects(writing,reason);await rejects(closing,reason);await rejects(bw.closed,reason);await rejects(bw.ready,reason);eq(bw.desiredSize,null);
"#,
    )
}

#[test]
fn close_is_queued_and_releases_zero_hwm_ready() {
    check(
        r#"
    const events=[];let finish;
    const w=new WritableStream({write(v){events.push(v);return new Promise(r=>finish=r)},close(){events.push('close')}},{highWaterMark:0}).getWriter();
    let ready=false;w.ready.then(()=>ready=true);await tick();assert(!ready);
    const p=w.write(1),closing=w.close();await w.ready;assert(ready);eq(events,[1]);await rejects(w.write(2));await rejects(w.close());
    finish();await p;await closing;await w.closed;eq(events,[1,'close']);
"#,
    )
}

#[test]
fn abort_waits_inflight_write_and_sink_abort_completion() {
    check(
        r#"
    let finishWrite,finishAbort,controller;const events=[],reason={abort:1};
    const s=new WritableStream({start(c){controller=c;c.signal.addEventListener('abort',()=>events.push('signal'))},write(v){events.push(v);return new Promise(r=>finishWrite=r)},abort(r){assert(r===reason);events.push('abort');return new Promise(r=>finishAbort=r)}}),w=s.getWriter();await tick();
    const a=w.write('a'),b=w.write('b'),closed=w.close();const abort=w.abort(reason);assert(abort===w.abort({other:1}));
    assert(controller.signal.aborted&&controller.signal.reason===reason);eq(events,['a','signal']);eq(w.desiredSize,null);await rejects(w.ready,reason);
    let aborted=false;abort.then(()=>aborted=true);finishWrite();await a;await rejects(b,reason);eq(events,['a','signal','abort']);assert(!aborted);
    finishAbort();await abort;await rejects(closed,reason);await rejects(w.closed,reason);eq(events,['a','signal','abort']);
"#,
    )
}

#[test]
fn already_erroring_abort_does_not_call_sink_abort() {
    check(
        r#"
    let controller,finish,aborts=0;const error={error:1},abortReason={abort:1};
    const w=new WritableStream({start(c){controller=c},write(){return new Promise(r=>finish=r)},abort(){aborts++}}).getWriter();await tick();
    const p=w.write(1);controller.error(error);const abort=w.abort(abortReason);assert(controller.signal.reason===abortReason);
    await rejects(w.ready,error);finish();await p;await rejects(abort,error);await rejects(w.closed,error);eq(aborts,0);
"#,
    )
}

#[test]
fn inflight_close_success_wins_over_abort_or_controller_error() {
    check(
        r#"
    let finish,controller,aborts=0;const reason={abort:1};
    const w=new WritableStream({start(c){controller=c},close(){return new Promise(r=>finish=r)},abort(){aborts++}}).getWriter();await tick();
    const closing=w.close();const abort=w.abort(reason);assert(controller.signal.aborted);await rejects(w.ready,reason);
    finish();await closing;await abort;await w.closed;eq(w.desiredSize,0);eq(aborts,0);
    let close2,c2;const other=new WritableStream({start(c){c2=c},close(){return new Promise(r=>close2=r)}}).getWriter();await tick();
    const p=other.close();const error={error:1};c2.error(error);close2();await p;await other.closed;
"#,
    )
}

#[test]
fn inflight_rejections_keep_stored_error_and_reject_abort_correctly() {
    check(
        r#"
    let rejectClose;const abortReason={},closeError={};
    const w=new WritableStream({close(){return new Promise((_,r)=>rejectClose=r)}}).getWriter();await tick();
    const p=w.close(),abort=w.abort(abortReason);rejectClose(closeError);
    await rejects(p,closeError);await rejects(abort,closeError);await rejects(w.closed,abortReason);
    let rejectWrite,c;const stored={},writeError={};
    const q=new WritableStream({start(v){c=v},write(){return new Promise((_,r)=>rejectWrite=r)}}).getWriter();await tick();
    const writing=q.write('a'),queued=q.write('b');c.error(stored);rejectWrite(writeError);
    await rejects(writing,writeError);await rejects(queued,stored);await rejects(q.closed,stored);
"#,
    )
}

#[test]
fn writer_release_preserves_pending_writes_and_replaces_promises() {
    check(
        r#"
    let finish;const events=[];const s=new WritableStream({write(v){events.push(v);if(v===1)return new Promise(r=>finish=r)}});const w=s.getWriter();await tick();
    const originalReady=w.ready;await originalReady;const writing=w.write(1);const ready=w.ready,closed=w.closed;w.releaseLock();assert(!s.locked);w.releaseLock();
    await rejects(ready);await rejects(closed);throws(()=>w.desiredSize);await rejects(w.write(2));await rejects(w.abort());await rejects(w.close());
    const q=s.getWriter(),second=q.write(2);finish();await writing;await second;eq(events,[1,2]);await q.close();
    const fulfilled=q.closed;await fulfilled;q.releaseLock();assert(q.closed!==fulfilled);await rejects(q.closed);await fulfilled;
"#,
    )
}

#[test]
fn size_errors_and_reentrant_release_or_close_observe_order() {
    check(
        r#"
    let w;const events=[];const s=new WritableStream({write(){events.push('write')}},{size(){events.push('size');w.releaseLock();return 1}});w=s.getWriter();await rejects(w.write('x'));eq(events,['size']);assert(!s.locked);
    let closePromise,q;const closing=new WritableStream({}, {size(){closePromise=q.close();return 1}});q=closing.getWriter();await rejects(q.write(1));await closePromise;
    for(const value of [-1,NaN,Infinity]){const a=new WritableStream({}, {size(){return value}}).getWriter();await rejects(a.write(1),'RangeError');await rejects(a.closed,'RangeError')}
    const reason={size:1};const b=new WritableStream({}, {size(){throw reason}}).getWriter();await rejects(b.write(1),reason);await rejects(b.closed,reason);
    let c,called=0;const r=new WritableStream({start(v){c=v}}, {size(){called++;return 1}}).getWriter();c.error(reason);await rejects(r.write(1),reason);eq(called,0);
"#,
    )
}

#[test]
fn abort_signal_reentrancy_and_default_reason_are_native() {
    check(
        r#"
    let controller;const reason={event:1};const s=new WritableStream({start(c){controller=c;c.signal.addEventListener('abort',()=>c.error(reason))},abort(){throw Error('must not run')}});await tick();
    await s.abort('outside');assert(controller.signal.aborted);const w=s.getWriter();await rejects(w.closed,reason);
    let other;const t=new WritableStream({start(c){other=c},abort(reason){eq(reason,undefined)}});await t.abort();assert(other.signal.reason instanceof DOMException);eq(other.signal.reason.name,'AbortError');
"#,
    )
}

#[test]
fn native_brand_descriptors_poisoned_globals_and_promise_species() {
    check(
        r#"
    const S=WritableStream,W=WritableStreamDefaultWriter,C=WritableStreamDefaultController;
    throws(()=>new C());throws(()=>new W({}));throws(()=>S());await rejects(S.prototype.close.call({}));await rejects(W.prototype.write.call({}));
    eq(W.prototype.write.length,0);assert(Object.getOwnPropertyDescriptor(W.prototype,'write').enumerable);
    const promise=Promise.resolve();const species=Object.getOwnPropertyDescriptor(Promise,Symbol.species);Object.defineProperty(Promise,Symbol.species,{configurable:true,get(){throw Error('species')}});
    const values=[];const s=new S({write(v){values.push(v);return promise}});const w=new W(s);
    globalThis.WritableStream=globalThis.WritableStreamDefaultWriter=globalThis.AbortController=()=>{throw Error('global')};
    await w.write(1);await w.close();eq(values,[1]);eq(Object.prototype.toString.call(s),'[object WritableStream]');Object.defineProperty(Promise,Symbol.species,species);
"#,
    )
}

#[test]
fn queue_limit_rejects_without_dropping_earlier_requests() {
    check(
        r#"
    let start;const w=new WritableStream({start(){return new Promise(r=>start=r)}}).getWriter();
    const promises=[];for(let i=0;i<10000;i++)promises.push(w.write(i).catch(e=>e));
    const overflow=await rejects(w.write(10000),'RangeError');start();
    const errors=await Promise.all(promises);assert(errors.every(e=>e===overflow));await rejects(w.closed,'RangeError');
"#,
    )
}

#[test]
fn native_algorithms_and_gc_state_survive_without_js_constructor_access() {
    let mut c = context();
    let received = Gc::new(GcRefCell::new(Vec::<JsValue>::new()));
    let write_fn = NativeFunction::from_copy_closure_with_captures(
        |_, args, received, _| {
            received.borrow_mut().push(args.get_or_undefined(0).clone());
            Ok(JsValue::undefined())
        },
        received.clone(),
    )
    .to_js_function(c.realm());
    let mut algorithms = WritableAlgorithms::default();
    algorithms.write = Some(write_fn.into());
    let (stream, controller) = new_writable(algorithms, &mut c).unwrap();
    let value: JsValue = stream.clone().into();
    assert!(is_writable(&value));
    assert_eq!(status(&value).unwrap(), WritableStatus::Writable);
    let writer = acquire_writer(&value, &mut c).unwrap();
    let writer: JsValue = writer.into();
    let pending = write(&writer, JsString::from("native").into(), &mut c).unwrap();
    boa_gc::force_collect();
    c.run_jobs().unwrap();
    assert!(matches!(pending.state(), PromiseState::Fulfilled(_)));
    assert_eq!(
        received.borrow()[0]
            .as_string()
            .unwrap()
            .to_std_string_lossy(),
        "native"
    );
    assert_eq!(desired_size(&writer).unwrap(), Some(1.0));
    assert!(!controller.signal().is_callable());
    let p = close_writer(&writer, &mut c).unwrap();
    c.run_jobs().unwrap();
    assert!(matches!(p.state(), PromiseState::Fulfilled(_)));
    assert_eq!(controller.status(), WritableStatus::Closed);
    release_writer(&writer, &mut c).unwrap();
    assert!(!is_locked(&value).unwrap());
}
