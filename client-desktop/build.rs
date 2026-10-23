use std::process::Command;

fn main() {
    slint_build::compile("ui/app_window.slint").expect("compile the Slint UI");

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let res_obj = format!("{}/icon.o", out_dir);
    if Command::new("windres").args(["-i", "icon.rc", "-o", &res_obj]).status().map(|s| s.success()).unwrap_or(false) {
        println!("cargo:rustc-link-arg={}", res_obj);
    }
}
