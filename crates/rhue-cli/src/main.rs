//! `rhue`, see [`rhue_cli::main`] for everything it does.

fn main() -> std::process::ExitCode {
    rhue_cli::main(std::env::args().skip(1))
}
