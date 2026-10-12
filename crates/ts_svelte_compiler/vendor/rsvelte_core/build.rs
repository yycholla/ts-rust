fn main() {
    let pinned_version = include_str!("svelte-version.txt").trim();
    println!("cargo::rerun-if-changed=svelte-version.txt");
    println!("cargo::rustc-env=SVELTE_VERSION={pinned_version}");
}
