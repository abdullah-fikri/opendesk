use std::process::Command;

fn main() {
    slint_build::compile("ui/app_window.slint").expect("compile the Slint UI");

    // The executable icon and version resource are Windows-only.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo:rerun-if-changed=icon.rc");
    println!("cargo:rerun-if-changed=icon.ico");
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let res_obj = format!("{}/icon.o", out_dir);
    // windres comes with binutils (MSYS2 mingw64); without it the exe just has no icon.
    match Command::new("windres").args(["-i", "icon.rc", "-o", &res_obj]).status() {
        Ok(s) if s.success() => println!("cargo:rustc-link-arg={}", res_obj),
        Ok(s) => println!("cargo:warning=windres failed ({s}): OpenDesk.exe is built without its icon"),
        Err(e) => println!("cargo:warning=windres not found ({e}): OpenDesk.exe is built without its icon"),
    }
}
