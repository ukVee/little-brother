//! cement-ir-rt — the Rust runtime core of the IR face-auth path.
//!
//! Skeleton for slice 013 of the `ir-face-root-cause` milestone: the
//! workspace, the byte-identical `ip3y` port with its golden frame, and
//! install discipline. Nothing here is on the auth path yet — the shell
//! `./cement-ir` still dispatches everything, and `lib/irface.py` stays the
//! working runtime until later slices cut verbs over one by one
//! (`decision-cement-ir-rust-runtime-core`).
use clap::Parser;

#[derive(Parser)]
#[command(name = "cement-ir-rt", version, about = "Rust runtime core of the IR face-auth path")]
struct Cli {
    /// placeholder — the verbs land in this slice's later commits
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

fn main() {
    let cli = Cli::parse();
    println!("cement-ir-rt skeleton (verbosity {})", cli.verbose);
}
