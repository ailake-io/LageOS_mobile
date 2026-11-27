//! Stub de libxkbcommon p/ build aarch64 (ver stub/xkbcommon_stub.c). Host x86 usa a lib do sistema.
use std::{env, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=stub/xkbcommon_stub.c");
    if !env::var("TARGET").unwrap_or_default().starts_with("aarch64") { return; }
    let out = env::var("OUT_DIR").unwrap();
    let obj = format!("{out}/xkbcommon_stub.o");
    let ok = Command::new("aarch64-linux-gnu-gcc")
        .args(["-O2", "-fPIC", "-ffreestanding", "-c", "stub/xkbcommon_stub.c", "-o", &obj])
        .status().map(|s| s.success()).unwrap_or(false)
        && Command::new("aarch64-linux-gnu-ar").args(["rcs", &format!("{out}/libxkbcommon.a"), &obj])
            .status().map(|s| s.success()).unwrap_or(false);
    assert!(ok, "falha ao compilar stub/xkbcommon_stub.c (precisa de aarch64-linux-gnu-gcc)");
    println!("cargo:rustc-link-search=native={out}");
}
