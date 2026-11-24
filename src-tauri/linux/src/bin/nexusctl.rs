use clap::Parser;

fn main() {
    let cli = nexus_linux::cli::Cli::parse();
    if let Err(e) = nexus_linux::cli::run(cli) {
        eprintln!("[nexusctl] 错误: {}", e);
        std::process::exit(1);
    }
}
