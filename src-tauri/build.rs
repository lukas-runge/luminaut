use std::process::Command;

fn main() {
    // Run tauri-ts-generator before building
    let output = Command::new("tauri-ts-generator")
        .args(["generate"])
        .current_dir("..")
        .output()
        .expect("Failed to run tauri-ts-generator");

    if !output.status.success() {
        eprintln!(
            "tauri-ts-generator stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        panic!("tauri-ts-generator failed");
    }
    println!("cargo:rerun-if-changed=src");

    tauri_build::build()
}
