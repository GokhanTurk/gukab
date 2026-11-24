//! Drawing gukab's in-session popups (macro picker, baud chooser) over the live
//! remote screen and handing the screen back afterwards.
//!
//! Normally the popup runs on the alternate screen and leaving it restores the
//! remote's screen untouched. When the remote program is itself on the alternate
//! screen (nvim, htop, a ratatui panel) that does not work: entering is a no-op and
//! leaving drops the terminal back to the main screen under the program. In that
//! case the popup draws directly over the program's screen, and the session asks
//! the remote to repaint once it closes (`Transport::redraw_start`).

use std::io::Write as _;

/// The remote program's terminal modes that matter for drawing over it, tracked
/// from its output stream.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RemoteScreen {
    /// The remote switched to the alternate screen (`?1049h` / `?1047h` / `?47h`).
    pub alt: bool,
    /// The remote hid the cursor (`?25l`).
    pub cursor_hidden: bool,
}

// Reset SGR after switching: the remote (e.g. nvim mid-redraw) may have left an
// attribute such as undercurl set, and ratatui assumes a clean pen. Leaving the
// alternate screen restores the remote's cursor and attributes.
const ENTER_ALT: &[u8] = b"\x1b[?1049h\x1b[0m";
const LEAVE_ALT: &[u8] = b"\x1b[?1049l";
// Remote already on the alternate screen: save its cursor (DECSC), reset SGR and
// clear, so ratatui's blank initial buffer matches what is on screen. The remote
// repaints everything after the popup closes.
const ENTER_OVER: &[u8] = b"\x1b7\x1b[0m\x1b[2J";
const LEAVE_OVER: &[u8] = b"\x1b[0m\x1b8";

/// The bytes that open the popup's screen.
fn enter_bytes(remote: RemoteScreen) -> &'static [u8] {
    if remote.alt { ENTER_OVER } else { ENTER_ALT }
}

/// The bytes that hand the screen back to the remote.
fn leave_bytes(remote: RemoteScreen) -> Vec<u8> {
    let mut out = if remote.alt { LEAVE_OVER } else { LEAVE_ALT }.to_vec();
    // ratatui shows the cursor at the search caret; put back the remote's choice.
    if remote.cursor_hidden {
        out.extend_from_slice(b"\x1b[?25l");
    }
    out
}

fn write_raw(bytes: &[u8]) {
    let mut out = std::io::stdout();
    let _ = out.write_all(bytes);
    let _ = out.flush();
}

/// Prepare the screen for a popup.
pub fn open(remote: RemoteScreen) {
    write_raw(enter_bytes(remote));
}

/// Give the screen back to the remote. With the remote on the alternate screen the
/// popup's cells are still visible until it repaints.
pub fn close(remote: RemoteScreen) {
    write_raw(&leave_bytes(remote));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_screen_remote_gets_the_alternate_screen() {
        let remote = RemoteScreen::default();
        assert_eq!(enter_bytes(remote), ENTER_ALT);
        assert_eq!(leave_bytes(remote), LEAVE_ALT);
    }

    #[test]
    fn full_screen_remote_is_never_switched_to_the_main_screen() {
        let remote = RemoteScreen { alt: true, cursor_hidden: true };
        let all = [enter_bytes(remote), &leave_bytes(remote)].concat();
        assert!(!String::from_utf8_lossy(&all).contains("?1049"), "{all:?}");
        assert!(leave_bytes(remote).ends_with(b"\x1b[?25l"));
    }
}
