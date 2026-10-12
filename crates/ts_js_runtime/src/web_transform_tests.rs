use super::*;
use boa_engine::{Source, builtins::promise::PromiseState};
fn context() -> Context {
    let mut c = Context::builder()
        .instructions_remaining(10_000_000)
        .build()
        .unwrap();
    crate::web_text::register(&mut c).unwrap();
    crate::web_abort::register(&mut c).unwrap();
    web_streams::register(&mut c).unwrap();
    web_writable::register(&mut c).unwrap();
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
    async function until(predicate){{for(let i=0;i<50;i++){{if(predicate())return;await tick()}}throw Error('Expected observable progress');}}
    {body}
    return true;
    }})()"#
    );
    let value = c.eval(Source::from_bytes(&source)).unwrap();
    let p = JsPromise::from_object(value.as_object().unwrap()).unwrap();
    c.run_jobs().unwrap();
    match p.state() {
        PromiseState::Fulfilled(v) => assert_eq!(v.as_boolean(), Some(true)),
        PromiseState::Rejected(e) => panic!(
            "Transform test rejected: {}",
            e.to_string(c).unwrap().to_std_string_escaped()
        ),
        PromiseState::Pending => panic!("Transform test remained pending after jobs drained"),
    }
}
fn check(body: &str) {
    run(&mut context(), body)
}

#[test]
fn constructor_order_types_brands_and_same_object_sides() {
    check(
        r#"
    const log=[];const strategy=name=>({get highWaterMark(){log.push(name+':hwm');return {valueOf(){log.push(name+':number');return 1}}},get size(){log.push(name+':size');return undefined}});
    const transformer=new Proxy({start(c){log.push('start:call');assert(this===transformer);assert(c instanceof TransformStreamDefaultController)}},{get(o,k){log.push(k);return o[k]}});
    const t=new TransformStream(transformer,strategy('w'),strategy('r'));
    eq(log,['w:hwm','w:number','w:size','r:hwm','r:number','r:size','cancel','flush','readableType','start','transform','writableType','start:call']);
    assert(t.readable===t.readable&&t.writable===t.writable);assert(t.readable instanceof ReadableStream&&t.writable instanceof WritableStream);
    throws(()=>new TransformStream({readableType:null}),'RangeError');throws(()=>new TransformStream({writableType:'bytes'}),'RangeError');
    throws(()=>new TransformStream({cancel:null}));throws(()=>new TransformStream({},null,{highWaterMark:-1}),'RangeError');
    const reason={};try{new TransformStream({start(){throw reason}});throw Error('missing')}catch(e){assert(e===reason)}
    throws(()=>TransformStream());throws(()=>new TransformStreamDefaultController());throws(()=>Object.getOwnPropertyDescriptor(TransformStream.prototype,'readable').get.call({}));
    throws(()=>TransformStreamDefaultController.prototype.enqueue.call({}));
    eq(Object.prototype.toString.call(t),'[object TransformStream]');
"#,
    )
}

#[test]
fn identity_is_demand_driven_and_retains_chunk_identity() {
    check(
        r#"
    const t=new TransformStream(),w=t.writable.getWriter(),r=t.readable.getReader();
    const chunk={same:1};let done=false;const written=w.write(chunk).then(()=>done=true);
    await tick();await tick();await tick();assert(!done);eq(w.desiredSize,0);
    const first=await r.read();assert(first.value===chunk&&!first.done);await written;eq(w.desiredSize,1);
    const closing=w.close();eq(await r.read(),{done:true});await closing;await r.closed;await w.closed;
"#,
    )
}

#[test]
fn start_promise_gates_transform_and_rejection_errors_both_sides() {
    check(
        r#"
    let start,called=0;const t=new TransformStream({start(){return new Promise(r=>start=r)},transform(v,c){called++;c.enqueue(v*2)}}),w=t.writable.getWriter(),r=t.readable.getReader();
    const reading=r.read(),writing=w.write(3);await tick();await tick();eq(called,0);start();eq(await reading,{value:6,done:false});await writing;await w.close();
    const reason={start:1};let rejectStart;const bad=new TransformStream({start(){return new Promise((_,r)=>rejectStart=r)}}),bw=bad.writable.getWriter(),br=bad.readable.getReader();
    const bwriting=bw.write('x'),breading=br.read();rejectStart(reason);await rejects(bwriting,reason);await rejects(breading,reason);await rejects(bw.closed,reason);await rejects(br.closed,reason);
"#,
    )
}

#[test]
fn multiple_outputs_backpressure_and_async_transform_are_serial() {
    check(
        r#"
    const events=[];let finish;const transformer={transform(v,c){assert(this===transformer);events.push(v);c.enqueue(v);c.enqueue(v+1);return new Promise(r=>finish=r)}};
    const t=new TransformStream(transformer),w=t.writable.getWriter(),r=t.readable.getReader();
    const a=r.read(),first=w.write(1),second=w.write(3);eq(await a,{value:1,done:false});eq(events,[1]);
    finish();await first;eq(events,[1]);eq(await r.read(),{value:2,done:false});await tick();eq(events,[1]);
    const next=r.read();eq(await next,{value:3,done:false});eq(events,[1,3]);finish();await second;eq(await r.read(),{value:4,done:false});await w.close();
"#,
    )
}

#[test]
fn positive_readable_hwm_and_custom_sizes_control_demand() {
    check(
        r#"
    let controller;const t=new TransformStream({start(c){controller=c},transform(v,c){c.enqueue(v)}},{highWaterMark:5,size:v=>v.length},{highWaterMark:3,size:v=>v.length});
    const w=t.writable.getWriter();await w.write('ab');eq(controller.desiredSize,1);await w.write('c');eq(controller.desiredSize,0);
    let finished=false;const blocked=w.write('d').then(()=>finished=true);await tick();await tick();assert(!finished);
    const r=t.readable.getReader();eq(await r.read(),{value:'ab',done:false});await blocked;eq(controller.desiredSize,1);eq(await r.read(),{value:'c',done:false});eq(await r.read(),{value:'d',done:false});await w.close();
"#,
    )
}

#[test]
fn flush_waits_for_transform_and_can_enqueue_final_output() {
    check(
        r#"
    let flushFinish;const events=[];const transformer={transform(v,c){events.push('transform');c.enqueue(v)},flush(c){assert(this===transformer);events.push('flush');c.enqueue('end');return new Promise(r=>flushFinish=r)}};
    const t=new TransformStream(transformer),w=t.writable.getWriter(),r=t.readable.getReader();
    const read=r.read();await w.write('start');eq(await read,{value:'start',done:false});let closed=false;const closing=w.close().then(()=>closed=true);
    eq(await r.read(),{value:'end',done:false});eq(events,['transform','flush']);assert(!closed);const end=r.read();flushFinish();eq(await end,{done:true});await closing;
"#,
    )
}

#[test]
fn transform_and_flush_rejection_preserve_error_identity() {
    check(
        r#"
    const reason={transform:1};const t=new TransformStream({transform(){return Promise.reject(reason)}}),w=t.writable.getWriter(),r=t.readable.getReader();
    const read=r.read();await rejects(w.write(1),reason);await rejects(read,reason);await rejects(w.closed,reason);await rejects(r.closed,reason);
    const flushError={flush:1};const bad=new TransformStream({flush(){throw flushError}}),bw=bad.writable.getWriter(),br=bad.readable.getReader();
    const pending=br.read();await rejects(bw.close(),flushError);await rejects(pending,flushError);await rejects(bw.closed,flushError);
"#,
    )
}

#[test]
fn controller_error_unblocks_blocked_write_without_transform() {
    check(
        r#"
    let controller,called=0;const reason={error:1};const t=new TransformStream({start(c){controller=c},transform(){called++}}),w=t.writable.getWriter(),r=t.readable.getReader();
    const pending=w.write(1);await tick();await tick();controller.error(reason);await rejects(pending,reason);await rejects(w.closed,reason);await rejects(r.read(),reason);eq(called,0);eq(controller.desiredSize,null);
    controller.error('later');throws(()=>controller.enqueue(2));await rejects(r.closed,reason);
"#,
    )
}

#[test]
fn terminate_closes_readable_after_queue_but_errors_writable() {
    check(
        r#"
    let controller;const t=new TransformStream({start(c){controller=c;c.enqueue('queued')}},{},{highWaterMark:2});
    const w=t.writable.getWriter(),r=t.readable.getReader();controller.terminate();throws(()=>controller.enqueue('no'));
    eq(await r.read(),{value:'queued',done:false});eq(await r.read(),{done:true});await r.closed;await rejects(w.closed);await rejects(w.write('x'));controller.terminate();
    const active=new TransformStream({transform(v,c){c.enqueue(v);c.terminate()}}),aw=active.writable.getWriter(),ar=active.readable.getReader();
    const reading=ar.read();await aw.write(1);eq(await reading,{value:1,done:false});eq(await ar.read(),{done:true});await rejects(aw.closed);
"#,
    )
}

#[test]
fn readable_cancel_waits_algorithm_and_unblocks_writable() {
    check(
        r#"
    let cancelFinish,calls=0;const reason={cancel:1};const transformer={cancel(v){assert(this===transformer&&v===reason);calls++;return new Promise(r=>cancelFinish=r)}};
    const t=new TransformStream(transformer),w=t.writable.getWriter();const writing=w.write(1);await tick();await tick();
    let cancelled=false;const cancel=t.readable.cancel(reason).then(()=>cancelled=true);eq(calls,1);assert(!cancelled);cancelFinish();await cancel;await rejects(writing,reason);await rejects(w.closed,reason);
    eq(await t.readable.getReader().read(),{done:true});
"#,
    )
}

#[test]
fn writable_abort_and_cancel_rejection_propagate_correct_reasons() {
    check(
        r#"
    const reason={abort:1};let calls=0;const t=new TransformStream({cancel(v){assert(v===reason);calls++}});const r=t.readable.getReader();
    const reading=r.read();await t.writable.abort(reason);eq(calls,1);await rejects(reading,reason);await rejects(r.closed,reason);
    const failure={cancelFailed:1};const bad=new TransformStream({cancel(){throw failure}});const w=bad.writable.getWriter();await rejects(bad.readable.cancel('cancel'),failure);await rejects(w.closed,failure);
    const other=new TransformStream({cancel(){return Promise.reject(failure)}}),reader=other.readable.getReader();await rejects(other.writable.abort('abort'),failure);await rejects(reader.read(),failure);
"#,
    )
}

#[test]
fn flush_cancel_race_shares_one_finish_operation() {
    check(
        r#"
    let finish,cancels=0,flushes=0;const t=new TransformStream({flush(){flushes++;return new Promise(r=>finish=r)},cancel(){cancels++}}),w=t.writable.getWriter();
    const closing=w.close();await until(()=>flushes===1);let cancelled=false;const cancel=t.readable.cancel('ignored').then(()=>cancelled=true);await tick();assert(!cancelled);eq(cancels,0);finish();await closing;await cancel;await w.closed;
    let cancelFinish;const other=new TransformStream({cancel(){cancels++;return new Promise(r=>cancelFinish=r)},flush(){throw Error('flush must not run')}}),ow=other.writable.getWriter();
    const cancelledRead=other.readable.cancel('shared');const close=ow.close();cancelFinish();await cancelledRead;await close;await ow.closed;eq(cancels,1);
"#,
    )
}

#[test]
fn readable_size_failure_and_reentrant_termination_error_the_writer() {
    check(
        r#"
    const reason={size:1};const t=new TransformStream({}, {},{highWaterMark:1,size(){throw reason}}),w=t.writable.getWriter(),r=t.readable.getReader();
    await rejects(w.write('x'),reason);await rejects(w.closed,reason);await rejects(r.read(),reason);
    let controller;const other=new TransformStream({start(c){controller=c}}, {},{highWaterMark:1,size(){controller.terminate();return 1}}),ow=other.writable.getWriter();
    await ow.write('x');await rejects(ow.closed);eq(await other.readable.getReader().read(),{done:true});
"#,
    )
}

#[test]
fn native_promise_reactions_ignore_species_and_mutated_globals() {
    check(
        r#"
    const descriptor=Object.getOwnPropertyDescriptor(Promise,Symbol.species);Object.defineProperty(Promise,Symbol.species,{configurable:true,get(){throw Error('species')}});
    try{
        const t=new TransformStream({transform(v,c){c.enqueue(v)}},{},{highWaterMark:1}),w=t.writable.getWriter(),r=t.readable.getReader();await tick();await tick();
        globalThis.TransformStream=globalThis.WritableStream=globalThis.ReadableStream=()=>{throw Error('global')};
        const reading=r.read();await w.write('x');eq(await reading,{value:'x',done:false});await w.close();await r.closed;
    }finally{Object.defineProperty(Promise,Symbol.species,descriptor)}
"#,
    )
}

#[test]
fn bounded_enqueues_do_not_fake_success() {
    check(
        r#"
    let controller;const t=new TransformStream({start(c){controller=c}}, {},{highWaterMark:Infinity});
    for(let i=0;i<10000;i++)controller.enqueue(i);const failure=throws(()=>controller.enqueue(10000),'RangeError');
    const w=t.writable.getWriter(),r=t.readable.getReader();await rejects(w.closed,failure);await rejects(r.read(),failure);
"#,
    )
}

#[test]
fn native_controller_gc_and_readable_algorithm_seams() {
    let mut c = context();
    let (stream, controller) = new_transform(TransformAlgorithms::default(), &mut c).unwrap();
    let value: JsValue = stream.into();
    assert!(is_transform(&value));
    let read = readable(&value).unwrap();
    let write = writable(&value).unwrap();
    let reader = web_streams::get_reader(&read.into(), &mut c).unwrap();
    let writer = web_writable::acquire_writer(&write.into(), &mut c).unwrap();
    let reading = web_streams::read(&reader.clone().into(), &mut c).unwrap();
    let writing = web_writable::write(
        &writer.clone().into(),
        JsString::from("native").into(),
        &mut c,
    )
    .unwrap();
    boa_gc::force_collect();
    c.run_jobs().unwrap();
    assert!(matches!(writing.state(), PromiseState::Fulfilled(_)));
    let PromiseState::Fulfilled(result) = reading.state() else {
        panic!("native transform read failed")
    };
    assert_eq!(
        result
            .as_object()
            .unwrap()
            .get(JsString::from("value"), &mut c)
            .unwrap()
            .as_string()
            .unwrap()
            .to_std_string_lossy(),
        "native"
    );
    controller.terminate(&mut c).unwrap();
    c.run_jobs().unwrap();
    assert_eq!(controller.desired_size(), Some(0.0));
    let mut algorithms = ReadableAlgorithms::default();
    algorithms.high_water_mark = 0.0;
    let (native, control) = web_streams::new_readable_with_algorithms(algorithms, &mut c).unwrap();
    c.run_jobs().unwrap();
    assert!(control.can_close_or_enqueue() && control.has_backpressure() && !control.is_errored());
    let reader = web_streams::get_reader(&native.into(), &mut c).unwrap();
    let pending = web_streams::read(&reader.into(), &mut c).unwrap();
    assert!(!control.has_backpressure());
    let reason = JsString::from("native-error");
    control.error(reason.clone().into(), &mut c).unwrap();
    assert!(control.is_errored());
    assert_eq!(control.stored_error().as_string(), Some(reason));
    assert!(matches!(pending.state(), PromiseState::Rejected(_)));
}

#[test]
fn callback_promise_assimilation_preserves_observable_then_and_species() {
    check(
        r#"
    const reason={species:1};const returned=Promise.resolve();
    Object.defineProperty(returned,'constructor',{value:{get [Symbol.species](){throw reason}}});
    const t=new TransformStream({start(){return returned}}),w=t.writable.getWriter(),r=t.readable.getReader();
    await rejects(w.closed,reason);await rejects(r.read(),reason);
    const called=[];const other=new TransformStream({transform(v,c){c.enqueue(v);return {then(resolve){called.push('then');resolve(99)}}}}),ow=other.writable.getWriter(),reader=other.readable.getReader();
    const reading=reader.read();eq(await ow.write('x'),undefined);eq(await reading,{value:'x',done:false});eq(called,['then']);await ow.close();
    "#,
    )
}
