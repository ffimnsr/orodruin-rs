fn main() {
    if let Err(error) = orodruin_cli::app::run(std::env::args_os()) {
        eprintln!("{error}");
        std::process::exit(error.exit_code());
    }
}
