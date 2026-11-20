use std::{env, fs, io, path::Path};

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-changed=webui/dist");
    println!("cargo:rerun-if-env-changed=NUJIJUSHI_REQUIRE_WEBUI");
    let dist = Path::new("webui/dist");
    let manifest = env::current_dir()?.join(dist);
    let mut definitions = String::new();
    if manifest.join("index.html").is_file() {
        definitions.push_str(&format!(
            "const SHELL: Option<&str> = Some(include_str!({:?}));\n",
            manifest.join("index.html")
        ));
        definitions.push_str("const ASSETS: &[(&str, &[u8])] = &[\n");
        for entry in fs::read_dir(manifest.join("assets"))? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                definitions.push_str(&format!(
                    "({:?}, include_bytes!({:?})),\n",
                    entry.file_name().to_string_lossy(),
                    entry.path()
                ));
            }
        }
        definitions.push_str("];\n");
    } else {
        assert!(
            env::var_os("NUJIJUSHI_REQUIRE_WEBUI").is_none(),
            "Build the admin with npm ci --prefix webui && npm run build --prefix webui"
        );
        definitions
            .push_str("const SHELL: Option<&str> = None;\nconst ASSETS: &[(&str, &[u8])] = &[];\n");
    }
    let out = env::var_os("OUT_DIR").ok_or_else(|| io::Error::other("missing Cargo OUT_DIR"))?;
    fs::write(Path::new(&out).join("admin_assets.rs"), definitions)
}
