use super::*;

#[test]
fn resolve_matches_node_path_resolve() {
    let root = Path::new("/app");
    assert_eq!(resolve(root, "./src/lib"), PathBuf::from("/app/src/lib"));
    assert_eq!(resolve(root, "../x/./y"), PathBuf::from("/x/y"));
    assert_eq!(resolve(root, "/abs/z"), PathBuf::from("/abs/z"));
}
