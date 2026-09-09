mod fixtures;
use fixtures::{signal, Bits, Kind};

/// Not a test — a generator, run with `--ignored`, so the demo files the
/// binary is exercised against come from the same hand-written writers the
/// null test uses.
#[test]
#[ignore]
fn emit_demo_files() {
    let dir = std::path::PathBuf::from(
        std::env::var("DECK_PI_DEMO_DIR").expect("set DECK_PI_DEMO_DIR"),
    );
    std::fs::create_dir_all(&dir).unwrap();
    let cases: &[(&str, Kind, Bits, u32, usize)] = &[
        ("44k1-16-stereo", Kind::Wav, Bits::S16, 44_100, 2),
        ("192k-24-stereo", Kind::Wav, Bits::S24, 192_000, 2),
        ("96k-24-aiff", Kind::Aiff, Bits::S24, 96_000, 2),
        ("88k2-24-sowt", Kind::AiffcSowt, Bits::S24, 88_200, 2),
        ("48k-16-rf64", Kind::Rf64, Bits::S16, 48_000, 2),
        ("44k1-24-mono", Kind::Wav, Bits::S24, 44_100, 1),
        ("REFUSED-32k", Kind::Wav, Bits::S16, 32_000, 2),
        ("REFUSED-5ch", Kind::Wav, Bits::S24, 48_000, 5),
    ];
    for (name, kind, bits, rate, ch) in cases {
        let s = signal(*bits, *ch, 4410);
        let bytes = fixtures::build(*kind, &s, *bits, *rate, *ch);
        fixtures::write(&dir, name, *kind, &bytes);
    }
    // Something libsndfile cannot open at all — the DSD / corrupt shape.
    std::fs::write(dir.join("UNREADABLE-not-audio.wav"), b"this is not a RIFF file").unwrap();
}
