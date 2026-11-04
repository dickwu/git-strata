#!/usr/bin/env python3
"""Deterministic 36-unit tray alpha masks. Requires only Python and librsvg."""
import argparse
import subprocess
from pathlib import Path

STATES = ('normal', 'checking', 'waiting', 'conflict', 'syncing', 'alert', 'down')
# Each knockout is the badge grown ~2.5 units; only the glyph is cut away.
SHAPES = {
    'normal': ('', ''),
    'checking': (''.join(f'<circle cx="{x}" cy="24" r="1.6"/>' for x in (24.4, 29, 33.6)), '<rect x="20.3" y="19.9" width="17.4" height="8.2" rx="4.1"/>'),
    'waiting': ('<circle cx="29" cy="22" r="5.5"/>', '<circle cx="29" cy="22" r="8"/>'),
    'syncing': ('<circle cx="29" cy="22" r="4.25" fill="none" stroke="black" stroke-width="2.5"/>', '<circle cx="29" cy="22" r="8"/>'),
    'conflict': ('<path d="M29 16 L35 22 L29 28 L23 22 Z" stroke="black" stroke-width="1" stroke-linejoin="round"/>', '<path d="M29 16 L35 22 L29 28 L23 22 Z" stroke="black" stroke-width="6" stroke-linejoin="round"/>'),
    'alert': ('<path d="M29 16 L35 27 L23 27 Z" stroke="black" stroke-width="1.2" stroke-linejoin="round" mask="url(#bang)"/>', '<path d="M29 16 L35 27 L23 27 Z" stroke="black" stroke-width="6.2" stroke-linejoin="round"/>'),
    'down': ('<path d="M4 4 L32 32" stroke="black" stroke-width="3" stroke-linecap="round"/>', '<path d="M4 4 L32 32" stroke="black" stroke-width="8" stroke-linecap="round"/>'),
}

def svg(content):
    return '<svg xmlns="http://www.w3.org/2000/svg" width="18" height="18" viewBox="0 0 36 36">' + content + '</svg>\n'

def generate(out):
    src = Path(__file__).parent
    raw = (src / 'tray.svg').read_text()
    body = raw[raw.index('<defs>'):raw.rindex('</svg>')]
    out.mkdir(parents=True, exist_ok=True)
    for state in STATES:
        badge, knockout = SHAPES[state]
        cut = '<defs><mask id="badge-cut" maskUnits="userSpaceOnUse" x="0" y="0" width="36" height="36"><rect width="36" height="36" fill="white"/>' + knockout + '</mask></defs>'
        bang = '<defs><mask id="bang"><rect width="36" height="36" fill="white"/><path d="M29 20 L29 22.7" stroke="black" stroke-width="1.8" stroke-linecap="round"/><circle cx="29" cy="25" r="0.9" fill="black"/></mask></defs>'
        for kind, content in [('glyph', cut + '<g mask="url(#badge-cut)">' + body + '</g>'), ('badge', bang + badge)]:
            path = out / f'{kind}-{state}.svg'
            path.write_text(svg(content))
            for n, suffix in [(18, ''), (36, '@2x')]:
                subprocess.run(['rsvg-convert', '-w', str(n), '-h', str(n), str(path), '-o', str(out / f'{kind}-{state}{suffix}.png')], check=True)

if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('output', type=Path)
    generate(parser.parse_args().output)
