# gukab README demo — render with:  vhs docs/demo.tape
# Uses fake hosts under docs/demo-home (no real data). Run from the repo root
# after `cargo build --release`.

Output docs/demo.gif

Set Shell "bash"
Set FontFamily "FiraCode Nerd Font Mono"
Set FontSize 18
Set Width 1200
Set Height 700
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
Down Sleep 250ms
Sleep 800ms

# Fuzzy search: substring filtering by group.
Type "core"   Sleep 1.6s
Backspace 4   Sleep 300ms

# Ranking: "r3" floats router-03 to the top.
Type "r3"     Sleep 1.8s
Backspace 2   Sleep 300ms

# Another fuzzy filter.
Type "acc"    Sleep 1.6s
Backspace 3   Sleep 500ms

# Navigate the list.
Down Sleep 350ms
Down Sleep 350ms
Down Sleep 350ms
Up   Sleep 450ms

# Open the Add Host form, then close it.
Ctrl+N        Sleep 2s
Escape        Sleep 700ms

# Quit.
Type "q"      Sleep 1.2s
