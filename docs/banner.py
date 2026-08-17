#!/usr/bin/env python3
"""Generate docs/banner.svg — animated terminal-style GUKAB banner for the README.

Run `python3 docs/banner.py` after editing; the SVG is generated, not hand-written.
"""

import os

GLYPHS = {
    "G": [".####", "#....", "#....", "#..##", "#...#", "#...#", ".####"],
    "U": ["#...#", "#...#", "#...#", "#...#", "#...#", "#...#", ".###."],
    "K": ["#...#", "#..#.", "#.#..", "##...", "#.#..", "#..#.", "#...#"],
    "A": [".###.", "#...#", "#...#", "#####", "#...#", "#...#", "#...#"],
    "B": ["####.", "#...#", "#...#", "####.", "#...#", "#...#", "####."],
}

W, H = 900, 420
CELL, PIX, LETTER_GAP = 22, 19, 18
WORD = "GUKAB"

WORD_W = len(WORD) * 5 * CELL + (len(WORD) - 1) * LETTER_GAP
X0 = (W - WORD_W) / 2
Y0 = 92
WORD_H = 7 * CELL
WORD_BOTTOM = Y0 + WORD_H              # 246
REFL_TOP = WORD_BOTTOM + 8             # 254
MIRROR_K = (WORD_BOTTOM + REFL_TOP) / 2
REFL_BOTTOM = 292

CYCLE = 5.5                            # master loop length (s)
CMD = "gukab"
CMD_X = 52.0
CHAR_W = 8.1


def pixels():
    x = X0
    for ch in WORD:
        for r, row in enumerate(GLYPHS[ch]):
            for c, v in enumerate(row):
                if v == "#":
                    px, py = x + c * CELL, Y0 + r * CELL
                    yield px, py, (px - X0) / WORD_W
        x += 5 * CELL + LETTER_GAP


PX = list(pixels())

pix_rects = "\n      ".join(
    f'<rect class="p" x="{px:.0f}" y="{py:.0f}" width="{PIX}" height="{PIX}" rx="3" '
    f'style="animation-delay:{-2.6 * (1 - ratio):.2f}s"/>'
    for px, py, ratio in PX
)
plain_rects = "\n      ".join(
    f'<rect x="{px:.0f}" y="{py:.0f}" width="{PIX}" height="{PIX}" rx="3"/>'
    for px, py, _ in PX
)

# --------------------------------------------------------------- typing effect
reveal_end = CYCLE * 0.22
type_vals = ";".join(f"{i * CHAR_W:.1f}" for i in range(len(CMD) + 1))
cursor_vals = ";".join(f"{CMD_X + i * CHAR_W:.1f}" for i in range(len(CMD) + 1))
full_w = len(CMD) * CHAR_W

# -------------------------------------------------- title-bar window buttons
# Breeze glyph order — chevron down / chevron up / cross — bare, no button chrome,
# on the right. Never mac traffic lights.
BTN_PITCH, BTN_CY, R = 22, 19.0, 3.8
btn_defs = []
for n, (kind, color) in enumerate(
    [("min", "#3f9dc4"), ("max", "#2f9e93"), ("close", "#c96b7e")]
):
    cx = W - 20 - (2 - n) * BTN_PITCH
    if kind == "min":
        glyph = (f'<polyline points="{cx - R:.1f},{BTN_CY - R / 2:.1f} {cx:.1f},'
                 f'{BTN_CY + R / 2:.1f} {cx + R:.1f},{BTN_CY - R / 2:.1f}"/>')
    elif kind == "max":
        glyph = (f'<polyline points="{cx - R:.1f},{BTN_CY + R / 2:.1f} {cx:.1f},'
                 f'{BTN_CY - R / 2:.1f} {cx + R:.1f},{BTN_CY + R / 2:.1f}"/>')
    else:
        glyph = (f'<path d="M{cx - R:.1f} {BTN_CY - R:.1f}l{2 * R:.1f} {2 * R:.1f}'
                 f'M{cx + R:.1f} {BTN_CY - R:.1f}l{-2 * R:.1f} {2 * R:.1f}"/>')
    btn_defs.append(f'<g stroke="{color}">{glyph}</g>')

win_buttons = "\n    ".join(btn_defs)

# ------------------------------------------------------------------- topology
NODE_Y = 372
NODES = [(110, "you"), (292, "core-sw"), (474, "dist-sw"), (656, "console"), (806, "edge-rtr")]

parts = []
for i in range(len(NODES) - 1):
    x1, x2 = NODES[i][0] + 22, NODES[i + 1][0] - 22
    serial = i == 2
    parts.append(
        f'<line class="link{" ser" if serial else ""}" x1="{x1}" y1="{NODE_Y}" '
        f'x2="{x2}" y2="{NODE_Y}"{" stroke-dasharray=\"2 5\"" if serial else ""}/>'
    )
for i, (x, label) in enumerate(NODES):
    leds = "".join(
        f'<rect class="led" x="{x - 14 + j * 9}" y="{NODE_Y - 4}" width="5" height="5" '
        f'rx="1" style="animation-delay:{i * 0.37 + j * 0.55:.2f}s"/>'
        for j in range(3)
    )
    parts.append(
        f'<g class="node"><rect x="{x - 22}" y="{NODE_Y - 12}" width="44" height="24" rx="6"/>'
        f'{leds}</g><text class="nlabel" x="{x}" y="{NODE_Y + 27}">{label}</text>'
    )
for i in range(len(NODES) - 1):
    x1, x2 = NODES[i][0] + 22, NODES[i + 1][0] - 22
    color = "#fbbf24" if i == 2 else "#7dd3fc"
    parts.append(
        f'<circle cy="{NODE_Y}" r="2.6" fill="{color}">'
        f'<animate attributeName="cx" from="{x1}" to="{x2}" dur="1.6s" '
        f'begin="{i * 0.4:.2f}s" repeatCount="indefinite"/>'
        f'<animate attributeName="opacity" values="0;1;1;0" dur="1.6s" '
        f'begin="{i * 0.4:.2f}s" repeatCount="indefinite"/></circle>'
    )
topology = "\n    ".join(parts)

svg = f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}"
     role="img" aria-label="gukab — terminal SSH and serial console connection manager">
  <title>gukab — SSH + serial console manager for the terminal</title>

  <defs>
    <linearGradient id="wordgrad" gradientUnits="userSpaceOnUse"
                    x1="{X0:.0f}" y1="{Y0}" x2="{X0 + WORD_W:.0f}" y2="{WORD_BOTTOM}">
      <stop offset="0%"   stop-color="#2dd4bf"/>
      <stop offset="22%"  stop-color="#22d3ee"/>
      <stop offset="44%"  stop-color="#38bdf8"/>
      <stop offset="66%"  stop-color="#818cf8"/>
      <stop offset="86%"  stop-color="#c084fc"/>
      <stop offset="100%" stop-color="#f0abfc"/>
    </linearGradient>

    <linearGradient id="sheen" x1="0" y1="0" x2="1" y2="0">
      <stop offset="0%"  stop-color="#fff" stop-opacity="0"/>
      <stop offset="42%" stop-color="#fff" stop-opacity="0.7"/>
      <stop offset="58%" stop-color="#fff" stop-opacity="0.7"/>
      <stop offset="100%" stop-color="#fff" stop-opacity="0"/>
    </linearGradient>

    <linearGradient id="bg" x1="0" y1="0" x2="0.65" y2="1">
      <stop offset="0%"   stop-color="#0b1118"/>
      <stop offset="55%"  stop-color="#080c12"/>
      <stop offset="100%" stop-color="#0a1220"/>
    </linearGradient>

    <radialGradient id="halo" cx="0.5" cy="0.42" r="0.62">
      <stop offset="0%"   stop-color="#2dd4bf" stop-opacity="0.20"/>
      <stop offset="50%"  stop-color="#6366f1" stop-opacity="0.11"/>
      <stop offset="100%" stop-color="#000" stop-opacity="0"/>
    </radialGradient>

    <linearGradient id="fade" gradientUnits="userSpaceOnUse"
                    x1="0" y1="{REFL_TOP}" x2="0" y2="{REFL_BOTTOM + 6}">
      <stop offset="0%"   stop-color="#fff" stop-opacity="0.24"/>
      <stop offset="100%" stop-color="#fff" stop-opacity="0"/>
    </linearGradient>

    <linearGradient id="rule" x1="0" y1="0" x2="1" y2="0">
      <stop offset="0%"   stop-color="#22d3ee" stop-opacity="0"/>
      <stop offset="50%"  stop-color="#818cf8" stop-opacity="0.6"/>
      <stop offset="100%" stop-color="#f0abfc" stop-opacity="0"/>
    </linearGradient>

    <pattern id="grid" width="30" height="30" patternUnits="userSpaceOnUse">
      <path d="M30 0H0V30" fill="none" stroke="#7dd3fc" stroke-opacity="0.05"/>
    </pattern>

    <clipPath id="wordclip">
      {plain_rects}
    </clipPath>

    <clipPath id="reflclip">
      <rect x="0" y="{REFL_TOP}" width="{W}" height="{REFL_BOTTOM - REFL_TOP}"/>
    </clipPath>

    <mask id="reflmask">
      <rect x="0" y="{REFL_TOP}" width="{W}" height="{REFL_BOTTOM - REFL_TOP + 6}"
            fill="url(#fade)"/>
    </mask>

    <filter id="glow" x="-30%" y="-30%" width="160%" height="160%">
      <feGaussianBlur stdDeviation="8" result="b"/>
      <feMerge><feMergeNode in="b"/><feMergeNode in="b"/></feMerge>
    </filter>
  </defs>

  <style>
    text {{ font-family: ui-monospace, "SFMono-Regular", "JetBrains Mono", "Fira Code",
            Menlo, Consolas, "DejaVu Sans Mono", monospace; }}
    .p {{ fill: url(#wordgrad); animation: wave 2.6s ease-in-out infinite; }}
    @keyframes wave {{ 0%, 100% {{ opacity: 0.74; }} 50% {{ opacity: 1; }} }}
    .sheen {{ animation: sweep {CYCLE}s cubic-bezier(.45,0,.55,1) infinite; }}
    @keyframes sweep {{
      0%        {{ transform: translateX(-560px); }}
      60%, 100% {{ transform: translateX({W + 160}px); }}
    }}
    .led {{ fill: #22d3ee; stroke: none; animation: wink 2.2s ease-in-out infinite; }}
    @keyframes wink {{ 0%, 100% {{ opacity: 0.16; }} 50% {{ opacity: 1; }} }}
    .link {{ stroke: #1d5c74; stroke-width: 1.6; }}
    .link.ser {{ stroke: #92400e; }}
    .node > rect {{ fill: #0f1a24; stroke: #24576e; stroke-width: 1.2; }}
    .wbtn g {{ fill: none; stroke-width: 1.25; stroke-linecap: round;
               stroke-linejoin: round; }}
    .nlabel {{ fill: #486a80; font-size: 9.5px; letter-spacing: 0.7px; text-anchor: middle; }}
  </style>

  <!-- terminal window -->
  <rect x="0.5" y="0.5" width="{W - 1}" height="{H - 1}" rx="14" fill="url(#bg)" stroke="#1b2b38"/>
  <rect x="1" y="1" width="{W - 2}" height="{H - 2}" rx="13" fill="url(#grid)"/>
  <rect x="1" y="1" width="{W - 2}" height="{H - 2}" rx="13" fill="url(#halo)"/>

  <!-- title bar -->
  <path d="M0.5 14.5A14 14 0 0 1 14.5 0.5H{W - 14}A14 14 0 0 1 {W - 0.5} 14.5V37H0.5Z"
        fill="#0d151d" stroke="#1b2b38"/>

  <!-- left: bare shell glyph, no frame -->
  <text x="20" y="23" font-size="12" fill="#2dd4bf" letter-spacing="0.5">&gt;_</text>

  <text x="{W / 2}" y="23.5" fill="#4c657a" font-size="11.5" text-anchor="middle"
        letter-spacing="1.4">~/gukab — ssh · serial · macros</text>

  <!-- right: minimal Breeze-order window glyphs -->
  <g class="wbtn">
    {win_buttons}
  </g>

  <!-- prompt line, typed on a loop -->
  <text x="34" y="67" font-size="13.5" fill="#34d399">❯</text>
  <clipPath id="typeclip">
    <rect x="{CMD_X}" y="53" height="20" width="{full_w:.1f}">
      <animate attributeName="width" values="{type_vals};{full_w:.1f};0"
               keyTimes="{";".join(f"{i / len(CMD) * (reveal_end / CYCLE):.4f}" for i in range(len(CMD) + 1))};0.94;1"
               dur="{CYCLE}s" calcMode="discrete" repeatCount="indefinite"/>
    </rect>
  </clipPath>
  <text x="{CMD_X}" y="67" font-size="13.5" fill="#d5e2ee"
        clip-path="url(#typeclip)">{CMD}</text>
  <text x="{W - 34}" y="67" font-size="10.5" fill="#3a5265" text-anchor="end"
        letter-spacing="0.8">↑↓ select · ⏎ connect · ^A macros · ^L console</text>
  <rect y="55" width="8" height="15" fill="#22d3ee" x="{CMD_X + full_w:.1f}">
    <animate attributeName="x" values="{cursor_vals};{CMD_X + full_w:.1f};{CMD_X}"
             keyTimes="{";".join(f"{i / len(CMD) * (reveal_end / CYCLE):.4f}" for i in range(len(CMD) + 1))};0.94;1"
             dur="{CYCLE}s" calcMode="discrete" repeatCount="indefinite"/>
    <animate attributeName="opacity" values="1;1;0;0" keyTimes="0;0.49;0.5;1"
             dur="1.1s" calcMode="discrete" repeatCount="indefinite"/>
  </rect>

  <!-- wordmark: bloom pass + crisp pass -->
  <g filter="url(#glow)" opacity="0.55">
    <g>
      {pix_rects}
    </g>
  </g>
  <g>
    {pix_rects}
  </g>

  <!-- light sweeping across the wordmark -->
  <g clip-path="url(#wordclip)">
    <rect class="sheen" x="0" y="{Y0 - 10}" width="230" height="{WORD_H + 20}" fill="url(#sheen)"/>
  </g>

  <!-- reflection -->
  <g clip-path="url(#reflclip)">
    <g mask="url(#reflmask)">
      <g transform="translate(0 {2 * MIRROR_K:.0f}) scale(1 -1)" fill="url(#wordgrad)">
        <g>
          {plain_rects}
        </g>
      </g>
    </g>
  </g>

  <!-- tagline -->
  <line x1="{X0:.0f}" y1="322" x2="{X0 + WORD_W:.0f}" y2="322" stroke="url(#rule)"/>
  <text x="{W / 2}" y="341" font-size="11.5" fill="#6b8598" text-anchor="middle"
        letter-spacing="3">SSH + SERIAL CONSOLE MANAGER · RUST · RATATUI · RUSSH</text>

  <!-- topology strip -->
  <g>
    {topology}
  </g>
</svg>
"""

out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "banner.svg")
with open(out, "w", encoding="utf-8") as f:
    f.write(svg)
print("wrote", out, len(svg), "bytes")
