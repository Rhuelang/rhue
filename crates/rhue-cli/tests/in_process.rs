// the command through the library, no process. only checks exit codes since
// everything else goes straight to stdout and stderr
use std::process::ExitCode;

fn rhue(args: &[&str]) -> ExitCode {
    rhue_cli::main(args.iter().map(|a| a.to_string()))
}

#[test]
fn exit_codes() {
    assert_eq!(rhue(&["-e", "1 + 2"]), ExitCode::SUCCESS);
    assert_eq!(rhue(&["-e", "error('nope')"]), ExitCode::FAILURE);
    assert_eq!(
        rhue(&["--check", "-e", "local x: number = 'a'"]),
        ExitCode::FAILURE
    );
    assert_eq!(rhue(&["--bogus"]), ExitCode::from(2));
    assert_eq!(rhue(&["fmt"]), ExitCode::from(2));
    assert_eq!(rhue(&["compile"]), ExitCode::from(2));
}
