fn main() {
    if std::env::var("CARGO_CFG_TARGET_FAMILY").as_deref() == Ok("wasm") {
        // Match the existing checker WASM module's shadow stack. This is a
        // linker setting, independent of the JavaScript worker's host stack.
        println!("cargo:rustc-cdylib-link-arg=-zstack-size=33554432");
    }
}
