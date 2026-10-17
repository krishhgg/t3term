"""Prints the built-in themes for src/tui/palettes.rs from the desktop's themePalettes.ts.

Run it on the file at the commit palettes.rs names, and put what it prints in place of the
constants there:

    git -C T3CODE show f570bd2:packages/shared/src/themePalettes.ts > themePalettes.ts
    python3 src/tui/palettes/table.py themePalettes.ts
"""
import re, sys
src = open(sys.argv[1]).read()
ROLES = ["canvas", "surfaceRaised", "surfaceOverlay", "text", "border", "input", "focus", "secondary",
         "muted", "mutedForeground", "error", "errorForeground", "warning", "warningForeground",
         "accentSurface", "messageSurface", "messageAction", "messageActionForeground", "codeBackground",
         "sidebar", "sidebarForeground", "sidebarMutedForeground", "sidebarRowActive",
         "sidebarRowSelected", "sidebarBorder"]
snake = lambda s: re.sub(r'([A-Z])', lambda m: '_' + m.group(1).lower(), s)
out = []
for m in re.finditer(r'export const (\w+)_THEME: ThemeDefinition = \{(.*?)\n\};', src, re.S):
    const, body = m.group(1), m.group(2)
    tid = re.search(r'id: "([^"]+)"', body).group(1)
    label = re.search(r'label: "([^"]+)"', body).group(1)
    assert re.search(r'appearance: "light"', body)
    light_part, dark_part = body.split('variants:')
    out.append(f'/// {label}, `{tid}`.')
    out.append(f'pub const {const}: Palette = Palette {{')
    for half, part in (('light', light_part), ('dark', dark_part)):
        roles = dict(re.findall(r'(\w+): "oklch\(([^)]*)\)"', part))
        assert len(roles) == 57, (tid, half, len(roles))
        out.append(f'    {half}: Roles {{')
        for role in ROLES:
            l, c, h = (x if '.' in x else x + '.0' for x in roles[role].split())
            out.append(f'        {snake(role)}: Oklch({l}, {c}, {h}),')
        out.append('    },')
    out.append('};')
    out.append('')
print('\n'.join(out).rstrip())
