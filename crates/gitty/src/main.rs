fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // run by git or ssh as GIT_ASKPASS / SSH_ASKPASS: relay the prompt to the TUI that started it
    if let (Some(sock), [prompt]) = (std::env::var_os(gitty::askpass::SOCK_ENV), args.as_slice()) {
        std::process::exit(gitty::askpass::helper_main(std::path::Path::new(&sock), prompt));
    }
    let code = match gitty::run(args) {
        Ok(code) => code,
        Err(e) => {
            gitty::term::restore();
            eprintln!("gitty: {e:#}");
            1
        }
    };
    std::process::exit(code);
}
