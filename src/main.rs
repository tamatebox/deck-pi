//! Placeholder entry point. Prints what the file layer makes of each path
//! given to it, which is the same judgement the browser will show on the
//! highlighted row.

use std::path::PathBuf;

use deck_pi::file::{OpenError, Track};

fn main() {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if args.is_empty() {
        eprintln!("usage: deck-pi <file>...");
        eprintln!("  reports what the file layer makes of each path");
        std::process::exit(2);
    }

    for path in args {
        match Track::open(&path) {
            Ok(track) => {
                let i = track.info();
                let secs = i.duration_secs();
                print!(
                    "PLAYS   {}  {} {} Hz {} {}ch  {:.0}:{:05.2}",
                    path.display(),
                    i.container,
                    i.rate,
                    i.depth,
                    i.channels,
                    (secs / 60.0).floor(),
                    secs % 60.0
                );
                if i.declared_length_is_suspect() {
                    print!("  [declared length suspect: past the 2 GiB ceiling]");
                }
                println!();
            }
            Err(OpenError::Rejected(why)) => println!("REFUSED {}  {}", path.display(), why),
            Err(OpenError::Unreadable(e)) => println!("UNREAD  {}  {}", path.display(), e),
        }
    }
}
