mod config;
mod fuzzy;
mod ssh;
mod tui;

const HELP: &str = "\
gukab — a terminal (TUI) SSH connection manager for network devices

USAGE:
    gukab            Launch the interactive host list
    gukab -h         Show this help
    gukab -V         Show the version

HOST LIST KEYS:
    ↑/↓              Navigate            Enter   Connect / toggle group
    Ctrl+↑ / Ctrl+↓  Move host in group  Ctrl+N  Add host
    Ctrl+E           Edit host           Ctrl+D  Delete host
    Ctrl+K           Add credential      Esc     Quit
    (type)           Fuzzy-filter the list

IN A SESSION:
    Ctrl+A           Macro picker        Ctrl+A Ctrl+A   Send a literal Ctrl+A

CONFIG (~/.config/gukab/):
    hosts.toml        Hosts and groups
    automations.toml  Reusable macros and expect rules
    known_hosts       Trusted SSH host-key fingerprints
    log/<host>/       Per-session transcripts

Docs: https://github.com/GokhanTurk/gukab";

#[tokio::main]
async fn main() {
    // Handle --help/--version before touching config or the terminal.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{HELP}");
        return;
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("gukab {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if let Some(unknown) = args.first() {
        eprintln!("gukab: unexpected argument '{unknown}'\nTry 'gukab --help'.");
        std::process::exit(2);
    }

    let (hosts, groups) = config::load_hosts().unwrap_or_default();
    let automations = config::load_automations().unwrap_or_default();
    if let Err(e) = tui::run(hosts, groups, automations).await {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
