const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let arg = std::env::args().nth(1);
    match arg.as_deref() {
        Some("--version" | "-V") => println!("demerzel {VERSION}"),
        _ => {
            eprintln!("demerzel {VERSION} — pre-alpha, nothing to run yet; see docs/plan.md");
            std::process::exit(1);
        }
    }
}
