use std::process::ExitCode;

fn main() -> ExitCode {
    mac_cpy::initialize();
    match mac_cpy::run(std::env::args_os().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ERROR: {error}");
            ExitCode::from(if mac_cpy::interrupted() { 130 } else { 1 })
        }
    }
}
