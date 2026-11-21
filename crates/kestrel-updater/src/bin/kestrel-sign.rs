//! Release tool: writes manifest.json and manifest.json.sig for the files in a directory.
//! Usage: KESTREL_SIGNING_KEY=<64 hex chars> kestrel-sign <version> <dir>

use ed25519_dalek::{Signer, SigningKey};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(version), Some(dir)) = (args.next(), args.next()) else {
        eprintln!("usage: kestrel-sign <version> <dir>");
        std::process::exit(2);
    };
    let seed = std::env::var("KESTREL_SIGNING_KEY").expect("KESTREL_SIGNING_KEY not set");
    let seed: [u8; 32] = (0..64)
        .step_by(2)
        .map(|i| u8::from_str_radix(&seed[i..i + 2], 16).expect("bad key"))
        .collect::<Vec<_>>()
        .try_into()
        .expect("key must be 32 bytes");
    let sk = SigningKey::from_bytes(&seed);
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name().into_string().unwrap())
        .filter(|n| !n.starts_with("manifest.json"))
        .collect();
    files.sort();
    let entries: Vec<_> = files
        .iter()
        .map(|n| {
            let data = std::fs::read(std::path::Path::new(&dir).join(n)).unwrap();
            serde_json::json!({ "name": n, "sha256": kestrel_updater::sha256_hex(&data) })
        })
        .collect();
    let manifest = serde_json::to_vec_pretty(&serde_json::json!({ "version": version, "files": entries })).unwrap();
    let dir = std::path::Path::new(&dir);
    std::fs::write(dir.join("manifest.json"), &manifest).unwrap();
    std::fs::write(dir.join("manifest.json.sig"), sk.sign(&manifest).to_bytes()).unwrap();
    let pk: String = sk.verifying_key().to_bytes().iter().map(|b| format!("{b:02x}")).collect();
    println!("signed {} files; public key {pk}", files.len());
}
