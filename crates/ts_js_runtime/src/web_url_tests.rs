//! Focused host integration tests. JavaScript below is test input executed by
//! Boa against Rust methods; it is not an implementation of either web class.
use super::*;
use boa_engine::Source;

fn check(body: &str) {
    let mut context = Context::builder()
        .instructions_remaining(10_000_000)
        .build()
        .unwrap();
    register(&mut context).unwrap();
    let source = format!(
        r#"(() => {{
        function eq(actual, expected) {{
            if (JSON.stringify(actual) !== JSON.stringify(expected))
                throw new Error(JSON.stringify({{actual,expected}}));
        }}
        function throws(fn, kind = TypeError) {{
            try {{ fn(); }} catch(error) {{ if(error instanceof kind) return; throw error; }}
            throw new Error('Expected exception');
        }}
        {body}
        return true;
    }})()"#
    );
    assert_eq!(
        context
            .eval(Source::from_bytes(&source))
            .unwrap()
            .as_boolean(),
        Some(true)
    );
}

#[test]
fn capability_is_explicit_and_existing_globals_are_preserved() {
    let mut context = Context::builder()
        .instructions_remaining(10_000_000)
        .build()
        .unwrap();
    assert_eq!(
        context
            .eval(Source::from_bytes(
                "typeof URL + '/' + typeof URLSearchParams"
            ))
            .unwrap()
            .as_string()
            .unwrap()
            .to_std_string_lossy(),
        "undefined/undefined"
    );
    context
        .eval(Source::from_bytes("globalThis.URL = 42"))
        .unwrap();
    assert!(register(&mut context).is_err());
    assert_eq!(
        context.eval(Source::from_bytes("URL")).unwrap().as_number(),
        Some(42.0)
    );
    assert_eq!(
        context
            .eval(Source::from_bytes("typeof URLSearchParams"))
            .unwrap()
            .as_string()
            .unwrap()
            .to_std_string_lossy(),
        "undefined"
    );
}

#[test]
fn live_params_identity_and_url_replacement_work_in_both_directions() {
    check(
        r#"
        const url = new URL('https://example.test/a?x=hello%20~#frag');
        const params = url.searchParams;
        eq(url.searchParams === params,true);
        params.sort();
        eq(url.href,'https://example.test/a?x=hello+%7E#frag');
        url.search='?a=1&a=2';
        eq(params.getAll('a'),['1','2']);
        url.href='http://other.test:80/new?z=3';
        eq(url.searchParams === params,true);
        eq([...params],[['z','3']]);
        params.append('a b','+😀');
        eq(url.href,'http://other.test/new?z=3&a+b=%2B%F0%9F%98%80');
        params.delete('z'); params.delete('a b');
        eq(url.href,'http://other.test/new');
        url.search='?'; eq(url.search,''); eq(url.href,'http://other.test/new?');
        params.sort(); eq(url.href,'http://other.test/new');
    "#,
    );
}

#[test]
fn component_setters_and_whatwg_normalization() {
    check(
        r#"
        const url=new URL('../x','https://us:pw@EXAMPLE.test:443/a/b');
        eq([url.href,url.origin,url.protocol,url.username,url.password,url.host,url.hostname,url.port,url.pathname],
           ['https://us:pw@example.test/x','https://example.test','https:','us','pw','example.test','example.test','','/x']);
        url.host='[::1]:8080'; url.protocol='http:'; url.username='a b'; url.password='c@d';
        url.pathname='/a b/../c'; url.hash='some space'; url.port='80';
        eq(url.href,'http://a%20b:c%40d@[::1]/c#some%20space');
        url.hostname='bad host'; url.port='70000';
        eq(url.host,'[::1]');
        throws(()=>url.href='http://[');
        eq(url.host,'[::1]');
        eq(url.toString(),url.href); eq(url.toJSON(),url.href); eq(JSON.parse(JSON.stringify(url)),url.href);
        eq(new URL('https://bücher.example/').hostname,'xn--bcher-kva.example');
        eq(new URL('http://0x7f000001/').hostname,'127.0.0.1');
        const opaque=new URL('data:text/plain,hi'); opaque.host='example.test'; opaque.pathname='/changed';
        eq(opaque.href,'data:text/plain,hi'); eq(opaque.origin,'null');
    "#,
    );
}

#[test]
fn parsing_static_methods_propagate_coercion_errors() {
    check(
        r#"
        eq(URL.canParse('child','https://example.test/root/'),true);
        eq(URL.parse('child','https://example.test/root/').href,'https://example.test/root/child');
        eq(URL.parse('relative'),null); eq(URL.canParse('http://['),false);
        eq(URL.canParse('https://example.test/','invalid base'),false);
        throws(()=>URL()); throws(()=>new URL()); throws(()=>URL.canParse()); throws(()=>URL.parse());
        const marker={}; let observed;
        try { URL.canParse({toString(){throw marker}}) } catch(error) {observed=error}
        eq(observed===marker,true);
        throws(()=>URL.parse(Symbol('x')));
        const calls=[]; new URL({toString(){calls.push('input');return '/x'}},{toString(){calls.push('base');return 'https://example.test'}});
        eq(calls,['input','base']);
    "#,
    );
}

#[test]
fn params_constructors_strings_records_iterables_and_usv_conversion() {
    check(
        r#"
        eq(new URLSearchParams().toString(),'');
        eq(new URLSearchParams(undefined).toString(),'');
        eq(new URLSearchParams(null).toString(),'null=');
        eq(new URLSearchParams('?a+b=%2B&bad=%FF&x=%').get('a b'),'+');
        eq(new URLSearchParams('bad=%FF').get('bad'),'\uFFFD');
        const proto={inherited:1}, object=Object.assign(Object.create(proto),{2:'two',1:'one',b:'bee'});
        Object.defineProperty(object,'hidden',{value:'secret'});
        eq([...new URLSearchParams(object)],[['1','one'],['2','two'],['b','bee']]);
        eq([...new URLSearchParams({'\uD800x':'first','x':'middle','\uDC00x':'last'})],[['\uFFFDx','last'],['x','middle']]);
        eq(new URLSearchParams([['key','\uD800']]).toString(),'key=%EF%BF%BD');
        const original=new URLSearchParams('a=1'); const copy=new URLSearchParams(original); original.append('b','2');
        eq(copy.toString(),'a=1');
        original[Symbol.iterator]=function*(){yield ['custom','yes']};
        eq(new URLSearchParams(original).toString(),'custom=yes');
        eq(new URLSearchParams(new Map([['a',1],['b',false]])).toString(),'a=1&b=false');
        throws(()=>new URLSearchParams([['only']])); throws(()=>new URLSearchParams(['ab']));
        throws(()=>new URLSearchParams({[Symbol('key')]:1}));
    "#,
    );
}

#[test]
fn constructors_follow_web_idl_sequence_conversion() {
    check(
        r#"
        const seen=[];
        function* outer(){yield ['a','b','c']; seen.push('after invalid pair'); yield ['d','e']; seen.push('done')}
        throws(()=>new URLSearchParams(outer()));
        eq(seen,['after invalid pair','done']);
        const closed=[]; const marker={}; let observed;
        function* inner(){try {yield {toString(){throw marker}}} finally {closed.push('inner')}}
        function* source(){try {yield inner()} finally {closed.push('outer')}}
        try {new URLSearchParams(source())} catch(error){observed=error}
        eq(observed===marker,true); eq(closed,[]);
    "#,
    );
}

#[test]
fn mutations_optional_value_and_required_arguments() {
    check(
        r#"
        const p=new URLSearchParams('a=1&b=2&a=3&a=1');
        eq(p.size,4); eq(p.get('missing'),null); eq(p.getAll('missing'),[]);
        eq(p.has('a','2'),false); eq(p.has('a',undefined),true);
        p.delete('a','1'); eq([...p],[['b','2'],['a','3']]);
        p.append('a','4'); p.append('z','5'); p.set('a','new');
        eq([...p],[['b','2'],['a','new'],['z','5']]);
        p.set('absent',undefined); eq(p.get('absent'),'undefined');
        p.delete('a',undefined); eq(p.has('a'),false);
        for(const name of ['append','set']) throws(()=>p[name]('one'));
        for(const name of ['delete','get','getAll','has']) throws(()=>p[name]());
        throws(()=>URLSearchParams.prototype.get.call({},'a'));
        throws(()=>URL.prototype.toString.call({}));
        eq(Object.prototype.toString.call(p),'[object URLSearchParams]');
        eq(Object.prototype.toString.call(new URL('https://example.test')),'[object URL]');
    "#,
    );
}

#[test]
fn sorting_is_stable_and_compares_utf16_code_units() {
    check(
        r#"
        const p=new URLSearchParams([['\uE000','private'],['😀','astral'],['a','one'],['a','two'],['z','last']]);
        p.sort(); eq([...p],[['a','one'],['a','two'],['z','last'],['😀','astral'],['\uE000','private']]);
    "#,
    );
}

#[test]
fn iterators_and_foreach_observe_live_mutations() {
    check(
        r#"
        const p=new URLSearchParams('a=1&b=2'); const entries=p.entries();
        eq(p[Symbol.iterator],p.entries); eq(entries[Symbol.iterator]()===entries,true);
        eq(entries.next(),{value:['a','1'],done:false});
        p.append('c','3'); p.delete('a');
        eq(entries.next(),{value:['c','3'],done:false});
        eq(entries.next().done,true); p.append('d','4'); eq(entries.next().done,true);
        eq([...p.keys()],['b','c','d']); eq([...p.values()],['2','3','4']);
        const q=new URLSearchParams('x=1'); const receiver={}; const visited=[];
        q.forEach(function(value,key,owner){
            eq(this===receiver,true); eq(owner===q,true); visited.push([key,value]);
            if(key==='x') q.append('y','2');
        },receiver);
        eq(visited,[['x','1'],['y','2']]);
        const url=new URL('https://example.test/?a=1'); const iterator=url.searchParams.keys();
        url.search='?b=2&c=3'; eq([...iterator],['b','c']);
        eq(Object.prototype.toString.call(p.entries()),'[object URLSearchParams Iterator]');
    "#,
    );
}

#[test]
fn reentrant_coercions_do_not_hold_native_borrows() {
    check(
        r#"
        const url=new URL('https://example.test/?a=1'); const p=url.searchParams;
        p.append({toString(){url.search='?b=2';return 'c'}},{toString(){p.append('d','4');return '3'}});
        eq(p.toString(),'b=2&d=4&c=3');
        url.pathname={toString(){url.hostname='other.test';return '/new'}};
        eq(url.href,'https://other.test/new?b=2&d=4&c=3');
    "#,
    );
}

#[test]
fn gc_preserves_shared_state_and_iterator_prototype() {
    let mut context = Context::builder()
        .instructions_remaining(10_000_000)
        .build()
        .unwrap();
    register(&mut context).unwrap();
    context.eval(Source::from_bytes("globalThis.saved = new URL('https://example.test/?x=1').searchParams; globalThis.iter = saved.entries();")).unwrap();
    boa_gc::force_collect();
    let result = context
        .eval(Source::from_bytes(
            "saved.append('y','2'); JSON.stringify([...iter])",
        ))
        .unwrap();
    assert_eq!(
        result.as_string().unwrap().to_std_string_lossy(),
        r#"[["x","1"],["y","2"]]"#
    );
}

#[test]
fn oversized_mutations_are_explicit_and_atomic() {
    check(
        r#"
        const url=new URL('https://example.test/?a=1'); const p=url.searchParams;
        const before=url.href;
        throws(()=>p.append('large','x'.repeat(4*1024*1024)),RangeError);
        eq(url.href,before); eq(p.toString(),'a=1');
        throws(()=>url.href='https://example.test/'+ 'x'.repeat(4*1024*1024),RangeError);
        eq(url.href,before);
    "#,
    );
}
