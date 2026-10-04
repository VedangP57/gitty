fn main() {
    let code = match gitty::run(std::env::args().skip(1).collect()) {
        Ok(code) => code,
        Err(e) => {
            gitty::term::restore();
            eprintln!("gitty: {e:#}");
            1
        }
    };
    std::process::exit(code);
}
