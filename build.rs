fn main() {
    // libsndfile is a system C library, bound by a hand-written FFI
    // (docs/implementation.md). pkg-config resolves it on both the
    // development Mac (Homebrew) and the Pi (Debian), so the link line is
    // not hardcoded anywhere.
    pkg_config::Config::new()
        .atleast_version("1.0.28") // RF64 read support
        .probe("sndfile")
        .expect("libsndfile not found by pkg-config (brew install libsndfile / apt install libsndfile1-dev)");
}
