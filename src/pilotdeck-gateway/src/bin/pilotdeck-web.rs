fn main() {
    std::process::exit(pilotdeck_gateway::web_cli::main_entry(
        std::env::args().skip(1).collect(),
    ));
}
