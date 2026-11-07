//! Embed everything under `assets/` as a (path, bytes) table (as mcast-web does).

use std::fmt::Write;
use std::path::Path;

fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .expect("reading assets")
        .map(|e| e.expect("reading assets").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            walk(&path, root, out);
        } else {
            let rel = path.strip_prefix(root).expect("under assets");
            out.push(rel.to_str().expect("UTF-8 asset names").replace('\\', "/"));
        }
    }
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = Vec::new();
    walk(&root, &root, &mut files);
    let mut code = String::from("pub static ASSETS: &[(&str, &[u8])] = &[\n");
    for f in &files {
        println!("cargo:rerun-if-changed={}", root.join(f).display());
        writeln!(
            code,
            "    ({f:?}, include_bytes!({:?})),",
            root.join(f).display().to_string()
        )
        .unwrap();
    }
    code.push_str("];\n");
    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).join("assets.rs");
    std::fs::write(out, code).unwrap();
}
