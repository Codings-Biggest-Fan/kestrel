use std::{env, fmt::Write, fs, path::Path};

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models");
    // Verified maps come first so they win over the generated msi-ec maps.
    let mut files = Vec::new();
    for sub in ["verified", "ec"] {
        let dir = root.join(sub);
        println!("cargo:rerun-if-changed={}", dir.display());
        let mut part: Vec<_> = fs::read_dir(&dir)
            .unwrap_or_else(|_| panic!("{} missing", dir.display()))
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            .collect();
        part.sort();
        files.extend(part);
    }
    let mut out = String::from("pub static EMBEDDED_MODELS: &[&str] = &[\n");
    for f in files {
        println!("cargo:rerun-if-changed={}", f.display());
        writeln!(out, "    include_str!({:?}),", f.canonicalize().unwrap()).unwrap();
    }
    out.push_str("];\n");
    fs::write(
        Path::new(&env::var("OUT_DIR").unwrap()).join("models.rs"),
        out,
    )
    .unwrap();
}
