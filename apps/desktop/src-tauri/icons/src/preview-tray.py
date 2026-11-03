#!/usr/bin/env python3
"""Render offline previews from production masks/palette, then assemble contact sheets."""
import base64
from pathlib import Path
import subprocess
import sys

output = Path(sys.argv[1]).resolve()
crate = Path(__file__).resolve().parents[2]
subprocess.run(['cargo', 'run', '--quiet', '--manifest-path', str(crate / 'Cargo.toml'), '--example', 'tray_preview', '--', str(output)], check=True)
states = ('down', 'alert', 'checking', 'waiting', 'conflict', 'syncing', 'normal')
backgrounds = (('light', 255), ('light', 224), ('light', 176), ('dark', 0), ('dark', 38), ('dark', 80))
for gray in (False, True):
    svg = ['<svg xmlns="http://www.w3.org/2000/svg" width="1060" height="690" viewBox="0 0 1060 690">', '<rect width="1060" height="690" fill="#f4f3f0"/>', '<g font-family="Helvetica, sans-serif" fill="#252525">', '<text x="24" y="30" font-size="20">GIT STRATA · tray states' + (' · grayscale' if gray else '') + '</text>', '<text x="24" y="54" font-size="12">Each cell: native 1x (18 px), native 2x (36 px), enlarged 2x (54 px). No animation.</text>']
    for col, (theme, bg) in enumerate(backgrounds):
        x = 150 + col * 150
        svg.append(f'<text x="{x}" y="80" font-size="12">{theme} #{bg:02X}{bg:02X}{bg:02X}</text>')
        for row, state in enumerate(states):
            y = 94 + row * 82
            if col == 0:
                svg.append(f'<text x="24" y="{y+39}" font-size="16">{state.title()}</text>')
            svg.append(f'<rect x="{x}" y="{y}" width="142" height="72" rx="8" fill="rgb({bg},{bg},{bg})"/>')
            for scale, offset, size in [('1x', 7, 18), ('2x', 32, 36), ('2x', 77, 54)]:
                name = f'{state}-{theme}-{bg}-{scale}' + ('-gray' if gray else '') + '.png'
                data = base64.b64encode((output / name).read_bytes()).decode()
                svg.append(f'<image x="{x+offset}" y="{y+(72-size)/2}" width="{size}" height="{size}" href="data:image/png;base64,{data}"/>')
    svg.append('</g></svg>')
    path = output / ('states-grayscale.svg' if gray else 'states.svg')
    path.write_text('\n'.join(svg))
    subprocess.run(['rsvg-convert', str(path), '-o', str(path.with_suffix('.png'))], check=True)
print('Contact sheets: states.png, states-grayscale.png')
