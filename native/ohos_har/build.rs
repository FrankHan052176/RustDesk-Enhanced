use std::{env, path::PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let enhanced_root = manifest_dir.join("../..");
    let engine_dir = enhanced_root.join("crates/rd-engine");
    let hbb_common_dir = enhanced_root.join("libs/hbb_common");
    let build_marker = env::var("RUSTDESK_ENHANCED_BUILD_MARKER")
        .unwrap_or_else(|_| "local-unattributed".to_owned());

    println!("cargo:rerun-if-env-changed=RUSTDESK_ENHANCED_BUILD_MARKER");
    println!("cargo:rerun-if-changed={}", engine_dir.display());
    println!("cargo:rerun-if-changed={}", hbb_common_dir.display());
    println!(
        "cargo:rustc-env=BUILD_RUSTDESK_SNAPSHOT_PRESENT={}",
        engine_dir.exists()
    );
    println!(
        "cargo:rustc-env=BUILD_HBB_COMMON_PRESENT={}",
        hbb_common_dir.exists()
    );
    println!(
        "cargo:rustc-env=BUILD_RUSTDESK_PATH={}",
        enhanced_root.display()
    );
    println!(
        "cargo:rustc-env=BUILD_HBB_COMMON_PATH={}",
        hbb_common_dir.display()
    );
    println!("cargo:rustc-env=BUILD_MARKER={build_marker}");

    napi_build_ohos::setup();
}
