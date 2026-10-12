//! Regression coverage for the vendored Boa JSON grammar/UTF-16 repair.
//! Runs the actual engine intrinsic, with no host implementation of JSON.
use boa_engine::{Context, Source};

fn check(source: &str) {
    let mut context = Context::builder()
        .instructions_remaining(10_000_000)
        .build()
        .unwrap();
    let source = format!(
        r#"(() => {{
        function assert(value, message='assertion failed') {{ if (!value) throw Error(message); }}
        function eq(a,b) {{ assert(Object.is(a,b), 'actual '+String(a)+' expected '+String(b)); }}
        function invalid(text) {{ try {{JSON.parse(text)}} catch(error) {{assert(error instanceof SyntaxError);return}} throw Error('Accepted invalid JSON '+text); }}
        {source}
        return true;
    }})()"#
    );
    match context.eval(Source::from_bytes(&source)) {
        Ok(value) => assert_eq!(value.as_boolean(), Some(true)),
        Err(error) => panic!("JSON intrinsic failed: {error}"),
    }
}

#[test]
fn json_number_overflow_underflow_and_negative_zero() {
    check(
        r#"
        eq(JSON.parse('1e999'), Infinity);
        eq(JSON.parse('-1e999'), -Infinity);
        eq(JSON.parse('9'.repeat(500)), Infinity);
        eq(JSON.parse('-0'), -0);
        eq(JSON.parse('-0.0e9'), -0);
        eq(JSON.parse('1e-999'), 0);
        eq(JSON.parse('-1e-999'), -0);
        eq(JSON.parse('9007199254740993'), 9007199254740992);
        eq(JSON.parse('1e'+'9'.repeat(400)), Infinity);
    "#,
    );
}

#[test]
fn json_escaped_and_literal_lone_surrogates_are_exact_utf16() {
    check(
        r#"
        for (const unit of [0xd800,0xdbff,0xdc00,0xdfff]) {
            const value=String.fromCharCode(unit);
            eq(JSON.parse('"'+value+'"'),value);
            eq(JSON.parse('"\\u'+unit.toString(16)+'"'),value);
            const object=JSON.parse('{"'+value+'":"'+value+'"}');
            eq(Object.keys(object)[0],value);eq(object[value],value);
        }
        eq(JSON.parse('"\\ud800x\\udc00"'),'\ud800x\udc00');
        eq(JSON.parse('"\\ud83d\\ude00"'),'😀');
        eq(JSON.parse('"\u2028\u2029"'),'\u2028\u2029');
    "#,
    );
}

#[test]
fn json_reviver_preserves_original_utf16_source_and_distinct_keys() {
    check(
        r#"
        const hi='\ud800', replacement='\ufffd', slash='\\ud800';
        const text='{"'+hi+'":"'+hi+'","'+replacement+'":1,"\\\\ud800":2,"escaped":"\\ud800"}';
        const seen=[];
        const value=JSON.parse(text,function(key,value,context){seen.push([key,context.source]);return value});
        eq(value[hi],hi);eq(value[replacement],1);eq(value[slash],2);eq(value.escaped,hi);
        eq(seen[0][0],hi);eq(seen[0][1],'"'+hi+'"');
        eq(seen[1][0],replacement);eq(seen[1][1],'1');
        eq(seen[2][0],slash);eq(seen[2][1],'2');
        eq(seen[3][1],'"\\ud800"');eq(seen[4][1],undefined);
    "#,
    );
}

#[test]
fn json_reviver_context_uses_same_value_and_original_container_identity() {
    check(
        r#"
        const seen={};
        JSON.parse('{"first":0,"zero":-0,"positive":0,"node":{"x":1},"array":[2]}', function(key,value,context){
            if(key==='first'){this.zero=0;this.positive=-0;this.node={x:1};this.array=[2]}
            seen[key]=context.source;
            return value;
        });
        eq(seen.first,'0');eq(seen.zero,undefined);eq(seen.positive,undefined);
        eq(seen.x,undefined);eq(seen['0'],undefined);
        const unchanged=[];
        JSON.parse('[-0,1e999,-1e999,1.2300e+02]',(k,v,c)=>{if(k!=='')unchanged.push(c.source);return v});
        eq(unchanged.join('|'),'-0|1e999|-1e999|1.2300e+02');
    "#,
    );
}

#[test]
fn json_duplicate_keys_proto_and_reviver_property_order() {
    check(
        r#"
        const names=[],sources=[];
        const object=JSON.parse('{"b":1,"2":2,"1":3,"b":4,"__proto__":0,"__proto__":{"safe":true}}',function(k,v,c){names.push(k);sources.push(c.source);return v});
        eq(names.join('|'),'1|2|b|safe|__proto__|');
        eq(sources[2],'4');eq(object.b,4);
        assert(Object.hasOwn(object,'__proto__'));eq(object.__proto__.safe,true);
        eq(Object.getPrototypeOf(object),Object.prototype);
        const hi='\ud800',src=[];
        const result=JSON.parse('{"'+hi+'":0,"\\ud800":1}',(k,v,c)=>{src.push(c.source);return v});
        eq(Object.keys(result).length,1);eq(result[hi],1);eq(src[0],'1');
    "#,
    );
}

#[test]
fn json_grammar_validation_rejects_executable_and_extended_syntax() {
    check(
        r#"
        globalThis.jsonExecuted=false;
        for(const text of ['', ' ', 'undefined', 'NaN', 'Infinity', '-Infinity', '+1','01','-01','.1','1.','1e','1e+','0x1','1_0','1n','[1,]','[,1]','{"a":1,}','{a:1}','{"a"=1}',"'x'",'/* hi */1','1// hi','1 2','true false','(1)','{};globalThis.jsonExecuted=true','{"x":(globalThis.jsonExecuted=true)}','{"get x":function(){}}','[...[]]','"\\x61"','"\\v"','"\\u{61}"','"\\u00xz"','"\\u123"','"a\nb"','"a\tb"','\ufeff1','\u00a01','\ud800','1\ud800','"\\u'+ '\ud800'+'000"']) invalid(text);
        eq(globalThis.jsonExecuted,false);
        eq(JSON.parse(' \t\r\n [1,true,null] \t\r\n').length,3);
        eq(JSON.parse('"\\/\\b\\f\\n\\r\\t\\\\\\\""'),'\/\b\f\n\r\t\\"');
    "#,
    );
}

#[test]
fn json_reviver_mutation_deletion_and_abrupt_completion_are_preserved() {
    check(
        r#"
        const seen=[];
        const object=JSON.parse('{"a":1,"b":2,"c":[3,4]}',function(k,v,c){
            seen.push(k);if(k==='a') {delete this.b;return undefined} if(k==='0') return undefined;return v;
        });
        assert(!Object.hasOwn(object,'a'));assert(!Object.hasOwn(object,'b'));
        eq(object.c.length,2);assert(!(0 in object.c));eq(object.c[1],4);
        eq(seen.join('|'),'a|b|0|1|c|');
        const sentinel={};try{JSON.parse('1',()=>{throw sentinel})}catch(error){eq(error,sentinel)}
        let count=0;eq(JSON.parse({toString(){count++;return '1e999'}}),Infinity);eq(count,1);
        eq(JSON.parse('1',{}),1);
    "#,
    );
}

#[test]
fn json_existing_nesting_resource_bound_is_retained() {
    check(
        r#"
        eq(JSON.parse('['.repeat(20)+'0'+']'.repeat(20)).length,1);
        invalid('['.repeat(128)+'0'+']'.repeat(128));
        eq(JSON.parse('"'+ '['.repeat(256)+'"').length,256);
    "#,
    );
}
