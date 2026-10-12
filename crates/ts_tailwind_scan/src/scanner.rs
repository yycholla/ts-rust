//! Exact extension dispatch from Tailwind v4.2.1 oxide/src/scanner/mod.rs.
//! MIT, copyright Tailwind Labs. No filesystem or source discovery integration.
use std::cell::Cell;
thread_local! {
    static DEPTH: Cell<usize> = const { Cell::new(0) };
    static WORK: Cell<usize> = const { Cell::new(0) };
    static EXCEEDED: Cell<bool> = const { Cell::new(false) };
}
pub fn bounded_pre_process(content: Vec<u8>, extension: &str) -> Result<Vec<u8>, &'static str> {
    let result = pre_process_input(content, extension);
    if EXCEEDED.get() {
        Err("Embedded preprocessor recursion/work limit exceeded")
    } else {
        Ok(result)
    }
}
// Vendored preprocessors call this same entry point for embedded languages.
// A failure is latched until the top-level bounded operation reports it.
pub fn pre_process_input(content: Vec<u8>, extension: &str) -> Vec<u8> {
    if DEPTH.get() == 0 {
        WORK.set(0);
        EXCEEDED.set(false);
    }
    WORK.set(WORK.get().saturating_add(content.len()));
    if DEPTH.get() >= 64 || WORK.get() > 64 * 1024 * 1024 || EXCEEDED.get() {
        EXCEEDED.set(true);
        return vec![];
    }
    struct DepthGuard;
    impl Drop for DepthGuard {
        fn drop(&mut self) {
            DEPTH.set(DEPTH.get() - 1);
        }
    }
    DEPTH.set(DEPTH.get() + 1);
    let _guard = DepthGuard;
    dispatch(content, extension)
}
fn dispatch(content: Vec<u8>, extension: &str) -> Vec<u8> {
    use crate::extractor::pre_processors::*;
    match extension {
        "clj" | "cljs" | "cljc" => Clojure.process(&content),
        "heex" | "eex" | "ex" | "exs" => Elixir.process(&content),
        "cshtml" | "razor" => Razor.process(&content),
        "haml" => Haml.process(&content),
        "json" => Json.process(&content),
        "md" | "mdx" => Markdown.process(&content),
        "pug" => Pug.process(&content),
        "rb" | "erb" => Ruby.process(&content),
        "slim" | "slang" => Slim.process(&content),
        "svelte" => Svelte.process(&content),
        "rs" => Rust.process(&content),
        "vue" => Vue.process(&content),
        _ => content,
    }
}
