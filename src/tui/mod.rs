pub mod app;
pub mod ui;

use std::time::Duration;

use crossterm::event::{self, Event, KeyEventKind};
use thiserror::Error;

use crate::{
    config::{Automations, Group, Host},
    serial, ssh,
};
use app::App;

#[derive(Error, Debug)]
pub enum TuiError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("SSH error: {0}")]
    Ssh(#[from] ssh::SshError),
    #[error("settings.toml: {0}")]
    Settings(String),
}

pub async fn run(
    hosts: Vec<Host>,
    groups: Vec<Group>,
    automations: Automations,
) -> Result<(), TuiError> {
    let settings = crate::config::load_settings().unwrap_or_default();
    // A bad escape key is refused up front rather than silently replaced: the user
    // would otherwise find their picker key "not working" mid-session.
    let escape = crate::session::EscapeKey::parse(&settings.session.escape_key)
        .map_err(TuiError::Settings)?;
    // The App outlives the terminal so that a failed connection can drop the user back
    // into the list with their selection, search filter, collapsed groups and any host
    // edits from this session still intact.
    let mut app = App::new(hosts, groups, automations, settings);

    loop {
        let mut terminal = ratatui::init();
        let result = event_loop(&mut terminal, &mut app).await;
        ratatui::restore();
        result?;

        // `app.automations` may have been edited in the macro manager this session, so
        // the connection always reads the live copy.
        let outcome = if let Some(host) = app.pending_connect.take() {
            ssh::client::connect(&host, &app.automations, escape).await
        } else if let Some(params) = app.pending_serial.take() {
            serial::client::connect_serial(&params, &app.automations, escape).await
        } else {
            break; // the user quit
        };

        // A session that ran and ended exits gukab, leaving its scrollback on screen.
        // A connection that never came up is recoverable: show why and go back to the
        // list rather than dumping the user out to the shell over one wrong password.
        match outcome {
            Ok(()) => break,
            Err(e) => app.show_error(e.to_string()),
        }
    }

    Ok(())
}

/// Runs the TUI until the user quits or asks for a connection. Returns with the
/// request left in `app.pending_connect` / `app.pending_serial` for `run` to act on.
async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
) -> Result<(), TuiError> {
    loop {
        terminal.draw(|f| ui::draw(f, app))?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            // Windows delivers a Release event per keystroke (and the Enter that
            // launched gukab from the shell arrives as a stray Release, which used
            // to "press" the selected row on startup). Only act on Press/Repeat.
            && key.kind != KeyEventKind::Release
        {
            app.update(key);
        }

        // A note wants editing: release the terminal, run the user's editor
        // (blocking), then re-enter the TUI and re-read the notes folder.
        if let Some(path) = app.pending_note_edit.take() {
            ratatui::restore();
            // ratatui::restore() leaves the alternate screen but not the cursor-hide
            // from draw(); old editors (macOS's stock nano 2.0.6) never re-show the
            // cursor themselves, so make it visible before handing the terminal over.
            {
                use std::io::Write;
                let mut out = std::io::stdout();
                let _ = out.write_all(b"\x1b[?25h");
                let _ = out.flush();
            }
            let result = crate::notes::open_in_editor(&path);
            *terminal = ratatui::init();
            if let Err(e) = result {
                app.set_status(format!("Editor failed: {e}"));
            }
            app.reload_notes();
        }

        if app.should_quit || app.pending_connect.is_some() || app.pending_serial.is_some() {
            break;
        }
    }

    Ok(())
}
