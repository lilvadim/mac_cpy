use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("build output directory"));
    // OUT_DIR is <target>/<profile>/build/<package>/out, also for cross builds.
    let profile = output.ancestors().nth(3).expect("profile directory");
    for name in ["exclude.mac_cpy", "root_exclude.mac_cpy", "include.mac_cpy"] {
        println!("cargo:rerun-if-changed={name}");
        fs::copy(manifest.join(name), profile.join(name))
            .unwrap_or_else(|e| panic!("Cannot copy {name} beside binary: {e}"));
    }
}
