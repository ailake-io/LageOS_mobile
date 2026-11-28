//! Stub de libxkbcommon p/ builds musl do LangeOS (ver stub/xkbcommon_stub.c). Host (gnu) usa a lib do sistema.
use std::{env, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=stub/xkbcommon_stub.c");
    let target = env::var("TARGET").unwrap_or_default();
    if !target.ends_with("musl") { return; }
    let (cc, ar) = if target.starts_with("aarch64") { ("aarch64-linux-gnu-gcc", "aarch64-linux-gnu-ar") } else { ("gcc", "ar") };
    let out = env::var("OUT_DIR").unwrap();
    let obj = format!("{out}/xkbcommon_stub.o");
    let ok = Command::new(cc)
        .args(["-O2", "-fPIC", "-ffreestanding", "-c", "stub/xkbcommon_stub.c", "-o", &obj])
        .status().map(|s| s.success()).unwrap_or(false)
        && Command::new(ar).args(["rcs", &format!("{out}/libxkbcommon.a"), &obj])
            .status().map(|s| s.success()).unwrap_or(false);
    assert!(ok, "falha ao compilar stub/xkbcommon_stub.c (precisa de {cc})");
    println!("cargo:rustc-link-search=native={out}");
}
