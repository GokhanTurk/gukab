# gukab README demo — render with:  vhs docs/demo.tape
# Uses fake hosts under docs/demo-home (no real data). Run from the repo root
# after `cargo build --release`.

Output docs/demo.gif

Set Shell "bash"
Set FontFamily "FiraCode Nerd Font Mono"
Set FontSize 18
Set Width 1200
Set Height 820
Set Theme "Catppuccin Mocha"
Set Padding 18
Set TypingSpeed 85ms

# Launch gukab against the sample config (HOME points at docs/demo-home).
Hide
Type "HOME=$PWD/docs/demo-home ./target/release/gukab" Enter
Show
Sleep 2.5s

# Glance over the grouped host list (Core / Distribution / Access / Servers).
Down Sleep 250ms
Down Sleep 250ms
Down Sleep 250ms
Down Sleep 250ms
Down Sleep 250ms
Down Sleep 250ms
Sleep 700ms

# Reorder a host within its group (Ctrl+↑/↓; Shift+↑/↓ also works on macOS).
Up Sleep 200ms
Up Sleep 200ms
Up Sleep 200ms
Up Sleep 200ms
Up Sleep 200ms
Up Sleep 200ms
Sleep 400ms
Down Sleep 350ms
Ctrl+Down Sleep 700ms
Ctrl+Down Sleep 700ms
Ctrl+Up   Sleep 800ms

# Fuzzy search: substring filtering by group.
Type "core"   Sleep 1.6s
Backspace 4   Sleep 300ms

# Ranking: "r3" floats router-03 to the top.
Type "r3"     Sleep 1.8s
Backspace 2   Sleep 300ms

# SSH key auth: open a key-authenticated host in the edit form — note the
# "SSH key file" field pointing at a private key (only the path is stored).
Type "web"    Sleep 1.2s
Ctrl+E        Sleep 2.6s
Escape        Sleep 600ms
Backspace 3   Sleep 400ms

# Quit (Esc — the search box is always live, so q types into it).
Escape        Sleep 1.2s
