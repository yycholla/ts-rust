use super::*;

const K2: CrawlProfile = CrawlProfile::Kit2_69_2;
const K3: CrawlProfile = CrawlProfile::Kit3_0_1;

#[test]
fn links_keep_order_duplicates_and_changing_base() {
    for profile in [K2, K3] {
        let result = crawl(r#"<a href="one">x</a><base href="../images/"><img src="a.png"><a href="one" href="two" href></a><base href="https://cdn.test/x/"><a href="two"><a href="two"><a href="//[invalid"><a href="http://[bad"><base href="http://[bad"><img src="end">"#, "/start/page", profile).unwrap();
        assert_eq!(
            result.hrefs,
            [
                "/start/one",
                "/images/a.png",
                "/images/two",
                "https://cdn.test/x/two",
                "https://cdn.test/x/two",
                "//[invalid",
                "https://cdn.test/x/end"
            ]
        );
        assert_eq!(result.invalid, ["http://[bad", "http://[bad"]);
    }
}

#[test]
fn scanner_skip_rules_and_raw_text_end_prefix_match_upstream() {
    let result = crawl(r#"<!doctype html><!-- <a href="comment"> --><![CDATA[<img src="cdata">]]><script src="ignored"><a href="script"></scripture><style><img src="style"></STYLE><img src="yes"><a href="after""#, "/", K3).unwrap();
    assert_eq!(result.hrefs, ["/yes", "/after"]);
    for tail in [
        "<!-- <a href='x'>",
        "<!DOCTYPE <a href='x'>",
        "<![CDATA[<a href='x'>",
        "<script><a href='x'>",
        "<style><a href='x'>",
        "<",
    ] {
        assert!(crawl(tail, "/", K3).unwrap().hrefs.is_empty());
    }
}

#[test]
fn attribute_quirks_do_not_turn_into_html5_parser_behavior() {
    let result = crawl("<img src=one/><a href='first' HREF='last' href><a href=\"x\\\"y\"><a href='ignored' rel='external'><a href='yes' rel='externality'><a href='no' rel='éEXTERNALé'><a href='word' rel='_external_'><a\rhref='cr'><a\u{a0}href='nbsp'><a href\u{feff}= 'bom'>", "/", K3).unwrap();
    // CR/NBSP/FEFF count as attribute-name characters, not attribute separators.
    assert_eq!(
        result.hrefs,
        ["/one/", "/last", "/x\\%22y", "/yes", "/word"]
    );
}

#[test]
fn anchor_ids_use_decode_uri_and_are_not_deduplicated() {
    let result = crawl(
        "<a id='caf%C3%A9%2f%23' name='😀'><div name='ignored' id='😀'><a id='&#37;E2%82%AC'>",
        "/",
        K3,
    )
    .unwrap();
    assert_eq!(result.ids, ["café%2f%23", "😀", "😀", "€"]);
    for value in [
        "%",
        "%2",
        "%zz",
        "%C0%AF",
        "%ED%A0%80",
        "%F4%90%80%80",
        "%E2x%82%AC",
        "%80",
    ] {
        assert_eq!(
            decode_uri(value).unwrap_err().kind,
            CrawlErrorKind::InvalidUri
        );
        assert_eq!(
            crawl(&format!("<a id='{value}'>"), "/", K3)
                .unwrap_err()
                .kind,
            CrawlErrorKind::InvalidUri
        );
    }
    assert_eq!(
        decode_uri("%3b%2F%3f%3A%40%26%3d%2b%24%2C%23%25%00😀").unwrap(),
        "%3b%2F%3f%3A%40%26%3d%2b%24%2C%23%\0😀"
    );
}

#[test]
fn complete_entity_table_longest_prefix_and_two_passes() {
    assert_eq!(ENTITIES.len(), 2231);
    for profile in [K2, K3] {
        assert_eq!(
            decode_entities(
                "&CounterClockwiseContourIntegral;&NotEqualTilde;&Afr;&notin;&notit;&AMP;&bogus;",
                profile
            )
            .unwrap(),
            "∳≂̸𝔄∉¬it;&&bogus;"
        );
        assert_eq!(
            decode_entities("&#38;amp; &amp;lt; &amp=", profile).unwrap(),
            "& &lt; &="
        );
        assert_eq!(
            decode_entities("&#12 &#x12 &#xyz;", profile).unwrap(),
            "&#12 &#x12 &#xyz;"
        );
    }
}

#[test]
fn numeric_entity_family_contracts_are_observable() {
    assert_eq!(
        decode_entities("&#65;&#66;&#x1f600;", K2).unwrap(),
        "A&#66;&#x1f600;"
    );
    assert_eq!(decode_entities("&#65;&#66;&#x1f600;", K3).unwrap(), "AB😀");
    assert_eq!(decode_entities("&#x1f600;", K2).unwrap(), "\u{f600}");
    assert_eq!(decode_entities("&#1114112;", K2).unwrap(), "\0");
    assert_eq!(decode_entities("&#1114112;&#abc;", K3).unwrap(), "��");
    assert_eq!(decode_entities("&#abc;", K2).unwrap(), "\0");
    assert_eq!(decode_entities("&#128;&#0;", K3).unwrap(), "\u{80}\0");
    assert_eq!(decode_entities("&#1e3;&#0b10;", K3).unwrap(), "Ϩ\u{2}");
    assert_eq!(decode_entities("&#xD83D;&#xDE00;", K3).unwrap(), "😀");
    assert_eq!(
        decode_entities("&#xD800;", K3).unwrap_err().kind,
        CrawlErrorKind::UnsupportedUtf16
    );
    assert_eq!(
        decode_entities("&#xD83D;&#xDE00;", K2).unwrap_err().kind,
        CrawlErrorKind::UnsupportedUtf16
    );
}

#[test]
fn kit2_unquoted_eof_is_bounded_and_kit3_recovers() {
    for html in ["<a href=x", "<a href=", "<a href=   "] {
        assert_eq!(
            crawl(html, "/", K2).unwrap_err().kind,
            CrawlErrorKind::UpstreamNonTermination
        );
    }
    assert_eq!(crawl("<a href=x", "/", K3).unwrap().hrefs, ["/x"]);
    assert!(crawl("<a href=", "/", K3).unwrap().hrefs.is_empty());
    assert_eq!(crawl("<a href='x", "/", K2).unwrap().hrefs, ["/x"]);
    assert_eq!(crawl("<a href=x ", "/", K2).unwrap().hrefs, ["/x"]);
}

#[test]
fn source_sets_keep_data_commas_and_upstream_reset_quirk() {
    assert_eq!(
        source_set("a.png 1x,b.png 2x, data:image/png;base64,a,b 3x, c.png 4x"),
        ["a.png", "b.png", "data:image/png;base64,a,b", "c.png"]
    );
    // Upstream skips the initial character after resetting the scan index.
    assert_eq!(source_set("a, , b"), ["a", ","]);
    let result = crawl(
        "<img src='first' srcset='a 1x,b 2x, //cdn/x 3x, data:image/png;base64,a,b 4x'>",
        "/images/",
        K3,
    )
    .unwrap();
    assert_eq!(
        result.hrefs,
        [
            "/images/first",
            "/images/a",
            "/images/b",
            "//cdn/x",
            "data:image/png;base64,a,b"
        ]
    );
}

#[test]
fn meta_name_precedence_case_and_empty_value() {
    let result = crawl("<meta property='og:image' content='img'><meta name='twitter:image' content='twitter'><meta name='' property='og:image' content='blocked'><meta name='OG:IMAGE' content='blocked'><meta name='og:video:secure_url' content='video'><meta content='both' src='src' href='href' property='og:url'>", "/", K3).unwrap();
    assert_eq!(
        result.hrefs,
        ["/img", "/twitter", "/video", "/href", "/src", "/both"]
    );
}

#[test]
fn resolution_uses_family_internal_scheme_and_url_search_hash() {
    for profile in [K2, K3] {
        assert_eq!(
            resolve_link("/a/b", "../c?x=é#😀", profile).unwrap(),
            "/c?x=%C3%A9#%F0%9F%98%80"
        );
        assert_eq!(resolve_link("/a/b", "?#", profile).unwrap(), "/a/b");
        assert_eq!(
            resolve_link("https://example.test/a", "?#", profile).unwrap(),
            "https://example.test/a?#"
        );
        assert_eq!(
            resolve_link("invalid", "//[bad host", profile).unwrap(),
            "//[bad host"
        );
        assert_eq!(
            resolve_link("/", "mailto:a@example.test", profile).unwrap(),
            "mailto:a@example.test"
        );
        assert!(resolve_link("mailto:a@example.test", "relative", profile).is_err());
        assert_eq!(
            resolve_link("/", "https://EXAMPLE.test:443/a/../b", profile).unwrap(),
            "https://example.test/b"
        );
    }
    assert_eq!(resolve_link("/", "a://host/path", K3).unwrap(), "/path");
    assert_eq!(
        resolve_link("/", "a://host/path", K2).unwrap(),
        "a://host/path"
    );
    assert_eq!(
        resolve_link("/", "sveltekit-internal://host/path", K2).unwrap(),
        "/path"
    );
    assert_eq!(
        resolve_link("/", "sveltekit-internal://host/path", K3).unwrap(),
        "sveltekit-internal://host/path"
    );
}

#[test]
fn limits_fail_transactionally() {
    let limits = CrawlLimits {
        html_bytes: 100,
        results: 1,
        result_bytes: 10,
    };
    assert_eq!(
        crawl_with_limits("<a href='x'><a href='y'>", "/", K3, limits)
            .unwrap_err()
            .kind,
        CrawlErrorKind::Limit
    );
    assert_eq!(
        crawl_with_limits("<a href='very-long-path'>", "/", K3, limits)
            .unwrap_err()
            .kind,
        CrawlErrorKind::Limit
    );
    assert_eq!(
        crawl_with_limits(&"x".repeat(101), "/", K3, limits)
            .unwrap_err()
            .kind,
        CrawlErrorKind::Limit
    );
}

#[test]
fn raw_text_close_uses_unicode_uppercase_and_utf16_slice() {
    let result = crawl("<script><a href='ignored'></ſcrıpt><a href='yes'>", "/", K3).unwrap();
    assert_eq!(result.hrefs, ["/yes"]);
    assert!(
        crawl("<style></ﬅyle><a href='ignored'>", "/", K3)
            .unwrap()
            .hrefs
            .is_empty()
    );
}
