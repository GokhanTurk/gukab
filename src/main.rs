mod config;
mod fuzzy;
mod ssh;
mod tui;

#[tokio::main]
async fn main() {
    let (hosts, groups) = config::load_hosts().unwrap_or_default();
    let automations = config::load_automations().unwrap_or_default();
    if let Err(e) = tui::run(hosts, groups, automations).await {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
