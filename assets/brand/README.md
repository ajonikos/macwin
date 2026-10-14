# Name and logo (proposal)

Status: proposal. The logo is ready to use; the name needs the owner's decision. Nothing in
the app uses these files yet (see "Using it" below).

## Logo

![preview](preview.png)

**Idea.** One screen shared by two computers: the left half is one machine, the right half the
other, and the pointer crosses the seam between them — the moment the product is about. The
two halves are deliberately not Apple's or Microsoft's shapes or colours (no apple, no four
panes, no Windows blue).

| File | Use |
|---|---|
| `icon.svg` | App icon (macOS icon grid: 824 px body in a 1024 px canvas). Readable down to 16 px. |
| `glyph.svg` | Monochrome glyph for the macOS menu bar (template image: black on transparent) and small monochrome uses. 18 x 18 pt. |
| `alt-hop.svg` | Alternative direction: two screens and a dotted hop. Clear when large, muddy at 32 px and below, so not recommended for the tray. |
| `preview.png` | Rendered sizes, the glyph on light and dark menu bars, name candidates. |

Colours

| | Hex |
|---|---|
| Left half (indigo) | `#6366F1` -> `#4338CA` |
| Right half (teal) | `#2DD4BF` -> `#0D9488` |
| Background (navy) | `#1E293B` -> `#0F172A` |
| Stand | `#94A3B8` |
| Pointer | `#FFFFFF`, outline `#0F172A` |

## Using it (after the name is decided)

- macOS: `AppIcon.icns` built from `icon.svg` (`iconutil`) and `CFBundleIconFile` in
  `Info.plist`; the menu bar item shows `glyph.svg` as a template image instead of the SF
  Symbols, with the state as a small badge.
- Windows: an `.ico` (16-256 px) embedded in `macwin.exe` and `macwin-tray.exe`; the tray
  icon is the small app icon with a state dot instead of the coloured circles.
