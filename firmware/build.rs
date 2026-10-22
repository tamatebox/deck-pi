//! Puts `memory.x` where the linker looks, which is the standard
//! `cortex-m-rt` arrangement rather than anything this project invented.

use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    File::create(out.join("memory.x"))
        .unwrap()
        .write_all(include_bytes!("memory.x"))
        .unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rerun-if-changed=build.rs");

    // What the panel does, as two names rather than the same three-feature
    // condition written at every item. `link`: it serves the deck's frames
    // over USB, which is the shipped firmware. `card`: it draws the test card,
    // because a bench build owns the serial port or asked for the card.
    // `selftest` touches no GPIO, so it has neither.
    let on = |f: &str| env::var_os(format!("CARGO_FEATURE_{f}")).is_some();
    let (selftest, faderprobe, paneltest) = (on("SELFTEST"), on("FADERPROBE"), on("PANELTEST"));
    println!("cargo::rustc-check-cfg=cfg(link)");
    println!("cargo::rustc-check-cfg=cfg(card)");
    if !selftest && !faderprobe && !paneltest {
        println!("cargo:rustc-cfg=link");
    }
    if !selftest && (faderprobe || paneltest) {
        println!("cargo:rustc-cfg=card");
    }
}
