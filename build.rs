use std::{env, path::PathBuf, process::Command};

fn main() {
    // Compile GSettings schemas into the cargo output directory so that
    // cargo run works without a system install. The binary sets
    // GSETTINGS_SCHEMA_DIR to this directory at startup when not installed.
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    // Walk up from OUT_DIR (typically target/debug/build/matx-*/out) to
    // target/debug (or target/release) so the compiled schema sits next
    // to the binary.
    let target_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("unexpected OUT_DIR structure")
        .to_path_buf();

    let status = Command::new("glib-compile-schemas")
        .arg("data/")
        .arg("--targetdir")
        .arg(&target_dir)
        .status()
        .expect("glib-compile-schemas not found — install glib2-devel / libglib2.0-dev");

    assert!(status.success(), "glib-compile-schemas failed");

    println!("cargo:rerun-if-changed=data/me.ramkrishna.hikyaku.gschema.xml");
}
