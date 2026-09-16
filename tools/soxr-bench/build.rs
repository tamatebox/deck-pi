fn main() {
    // `soxr.h` documents variable-rate creation only as "see example # 5", so
    // the struct layouts in `src/main.rs` were read out of the installed
    // header rather than recalled. Pinning a minimum here would be a claim
    // about which version those layouts came from, and 0.1.3 is simply what
    // Debian ships; the structs have not changed across it.
    pkg_config::probe_library("soxr").expect("libsoxr not found: apt install libsoxr-dev");
}
