mod config;
mod fuzzy;
#[cfg(target_os = "macos")]
mod macos_trust;
mod serial;
mod session;
mod ssh;
mod tui;

const HELP: &str = "\
gukab — a terminal (TUI) SSH connection manager for network devices

USAGE:
    gukab               Launch the interactive host list
    gukab -h            Show this help
    gukab -V            Show the version
    gukab --trust-keychain   (macOS) Sign the binary with a local identity so the
                             keychain stops re-prompting after updates — run once
                             after each update, then click 'Always Allow' once

HOST LIST KEYS:
    ↑/↓              Navigate            Enter   Connect / toggle group
    Ctrl/Shift+↑↓    Move host in group  Ctrl+N  Add host
    Ctrl+E           Edit host           Ctrl+D  Delete host
    Ctrl+K           Add credential      Ctrl+G  Manage macros
    Ctrl+L           Console (serial)    Esc     Quit
    (type)           Fuzzy-filter the list

IN A SESSION:
    Ctrl+A           Macro picker        Ctrl+A Ctrl+A   Send a literal Ctrl+A
    Ctrl+B           Cycle baud (serial console only)

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
    if args.iter().any(|a| a == "--trust-keychain") {
        trust_keychain();
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

/// `--trust-keychain`: sign the binary with a stable per-machine identity so the
/// macOS keychain stops re-prompting after each update. macOS-only.
fn trust_keychain() {
    #[cfg(target_os = "macos")]
    match macos_trust::run() {
        Ok(()) => println!(
            "gukab: signed with the local 'gukab-local-signing' identity.\n\n\
             What to expect now:\n\
             \x20 1. Open gukab and connect to your hosts.\n\
             \x20 2. The keychain asks once per DISTINCT credential (i.e. per\n\
             \x20    credential_ref / send_credential, not per host) — click\n\
             \x20    \"Always Allow\" (not just \"Allow\"). Hosts that share a\n\
             \x20    credential are covered by a single grant.\n\
             \x20 3. After that gukab never asks again — close and reopen it freely.\n\n\
             After a future update, just run `gukab --trust-keychain` again; your\n\
             \"Always Allow\" grants stay valid, so no further clicks are needed.\n\n\
             (The very first run may also ask to let codesign use the new signing\n\
             key — click \"Always Allow\" there too.)"
        ),
        Err(e) => {
            eprintln!("gukab --trust-keychain failed: {e}");
            std::process::exit(1);
        }
    }
    #[cfg(not(target_os = "macos"))]
    eprintln!("gukab: --trust-keychain is macOS-only.");
}
