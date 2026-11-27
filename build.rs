use std::{env, fs, io, path::Path, process::Command};

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-changed=webui/dist");
    println!("cargo:rerun-if-env-changed=NUJIJUSHI_REQUIRE_WEBUI");
    build_revision();
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

fn build_revision() {
    println!("cargo:rerun-if-env-changed=NUJIJUSHI_BUILD_REVISION");
    println!("cargo:rerun-if-env-changed=NUJIJUSHI_BUILD_DIRTY");
    // Reevaluate dirty state when inputs change, including edits that don't move HEAD.
    for path in [
        "Cargo.toml",
        "Cargo.lock",
        "src",
        "crates",
        "templates",
        "migrations",
        "webui/src",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    let checkout = Path::new(".git").exists();
    if checkout {
        for name in ["HEAD", "index", "refs", "packed-refs"] {
            if let Some(path) =
                git(&["rev-parse", "--git-path", name]).filter(|path| Path::new(path).exists())
            {
                println!("cargo:rerun-if-changed={path}");
            }
        }
    }
    let revision = env::var("NUJIJUSHI_BUILD_REVISION")
        .ok()
        .filter(|revision| !revision.is_empty())
        .or_else(|| checkout.then(|| git(&["rev-parse", "HEAD"])).flatten());
    if let Some(revision) = revision.filter(|revision| {
        matches!(revision.len(), 40 | 64) && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) {
        println!("cargo:rustc-env=NUJIJUSHI_REVISION={revision}");
    }
    let dirty = env::var("NUJIJUSHI_BUILD_DIRTY")
        .ok()
        .and_then(|value| value.parse::<bool>().ok())
        .or_else(|| {
            checkout
                .then(|| git(&["status", "--porcelain"]))
                .flatten()
                .map(|status| !status.is_empty())
        });
    if let Some(dirty) = dirty {
        println!("cargo:rustc-env=NUJIJUSHI_DIRTY={dirty}");
    }
}

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(["-c", "core.fsmonitor=false"])
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
