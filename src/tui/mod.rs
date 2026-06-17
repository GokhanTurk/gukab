pub mod app;
pub mod ui;

use std::time::Duration;

use crossterm::event::{self, Event};
use thiserror::Error;

use crate::{
    config::{Automations, Group, Host},
    ssh,
};
use app::App;

#[derive(Error, Debug)]
pub enum TuiError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("SSH error: {0}")]
    Ssh(#[from] ssh::SshError),
}

pub async fn run(
    hosts: Vec<Host>,
    groups: Vec<Group>,
    automations: Automations,
) -> Result<(), TuiError> {
    let mut terminal = ratatui::init();
    let pending = event_loop(&mut terminal, hosts, groups).await;
    ratatui::restore();

    if let Some(host) = pending? {
        ssh::client::connect(&host, &automations).await?;
    }

    Ok(())
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    hosts: Vec<Host>,
    groups: Vec<Group>,
) -> Result<Option<Host>, TuiError> {
    let mut app = App::new(hosts, groups);

    loop {
        terminal.draw(|f| ui::draw(f, &app))?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            app.update(key);
        }

        if app.should_quit || app.pending_connect.is_some() {
            break;
        }
    }

    Ok(app.pending_connect)
}
