use std::process::ExitCode;

fn main() -> ExitCode {
  match compostbin_cli::run() {
    Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
    Err(error) => {
      eprintln!("compostbin: {error}");
      ExitCode::FAILURE
    }
  }
}
