use std::process::Command;

fn main() {
    slint_build::compile("ui/app_window.slint").expect("compile the Slint UI");

    // The executable icon and version resource are Windows-only.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let res_obj = format!("{}/icon.o", out_dir);
    if Command::new("windres").args(["-i", "icon.rc", "-o", &res_obj]).status().map(|s| s.success()).unwrap_or(false) {
        println!("cargo:rustc-link-arg={}", res_obj);
    }
}
