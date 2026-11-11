fn main() {
    // Rebuild when the bridge source or the emitted bundle changes, so a stale
    // `dist/bridge-init.js` never stays silently embedded.
    println!("cargo:rerun-if-changed=../build");
    println!("cargo:rerun-if-changed=../dist/bridge-init.js");
    tauri_build::build();
}
