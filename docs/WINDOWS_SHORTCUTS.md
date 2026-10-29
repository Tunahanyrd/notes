# Windows Samsung Notes keyboard shortcuts (reference, 2026-10-26)

Reference: Samsung Notes for Windows 4.3.831.0 (x64). The list below records which key
combinations the Windows app binds and for which purpose (an interoperability/UX
fact), described in our own words. It was derived from static inspection of the
installed app's shortcut-helper data; it was not confirmed by pressing keys in the
Windows app. No Samsung code, resource text or resource identifiers are reproduced.

- The Windows app groups its shortcut help into four groups: text-editing navigation,
  text formatting, view controls, and settings/panels.
- Trash retention is 30 days (the trash view shows the remaining days per item).
- Undo/redo and Delete for selections exist in code but are not listed in the help
  dialog; they are standard editor behaviour here as well.

## List and Linux mapping

Scope: **list** = note list, **view** = open note, **text** = rich text editor focused.
"Linux" = default binding in `crates/rs-store/src/shortcuts.rs` (schema 2); all are remappable in
Settings → Keyboard shortcuts (UI text: "Klavye kısayolları") except rows marked *fixed*.

| Windows key | Windows action | Group | Linux | Linux action id / note |
|---|---|---|---|---|
| Tab | Focus next control | text 1 | Tab (*fixed*) | browser focus navigation |
| Shift+Tab | Focus previous control | text 1 | Shift+Tab (*fixed*) | browser focus navigation |
| Ctrl+N | Add page | text 1 | — | not bound: Windows lists Ctrl+N twice; Linux keeps Ctrl+N = new note. Page add stays in the page organiser (UI text: "Sayfaları düzenle", Edit pages) |
| Ctrl+N | New note | text 1 | Ctrl+N | `newNote` |
| Ctrl+Shift+T | New text box | text 1 | — | no text boxes in Linux |
| Ctrl+/ | Strikethrough | text 2 | Ctrl+/ | `strikethrough` |
| Ctrl+B / I / U | Bold / Italic / Underline | text 2 | same | `bold` `italic` `underline` |
| Ctrl+E / L / R | Align center / left / right | text 2 | same | `alignCenter` `alignLeft` `alignRight` |
| Ctrl+Shift+7 | Numbered list | text 2 | same | `numberedList` |
| Ctrl+Shift+8 | Bullets | text 2 | same | `bulletList` |
| Ctrl+Shift+C | Checklist | text 2 | same | `checklist` |
| Ctrl+[ / Ctrl+] | Decrease / increase font size | text 2 | same | `fontSmaller` `fontLarger` (steps 8…64, unstyled = 32) |
| Ctrl+M / Ctrl+Shift+M | Indent / outdent | text 2 | same | `indent` `outdent` |
| Ctrl++ / Ctrl+- | Zoom in / out | view 3 | Ctrl+Plus / Ctrl+Minus | `zoomIn` `zoomOut` (±10 %, 50–150 %) |
| Ctrl+mouse wheel | Zoom in/out | view 3 | *fixed* | over the page area |
| Ctrl+Alt+L | Zoom lock | view 3 | — | no zoom lock in Linux |
| Enter | Leave reading mode | view 3 | Enter (*fixed*) | leaves reading mode when focus is not in a control |
| Alt+Enter | Toggle reading mode | view 3 | Alt+Enter | `readingMode` (UI text: "Okuma modu", Reading mode) |
| Alt+F | Toggle full screen | view 3 | Alt+F | `fullScreen` |
| Ctrl+T | Toggle pen panel | view 4 | Ctrl+T | `penSettings`: shows/hides the right tool panel (pen panel on ink pages) |
| Alt+P | Toggle page settings panel | view 4 | Alt+P | `pageSettings` (UI menu "Sayfa ayarları", Page settings) |
| Alt+R | Toggle audio controls | view 4 | — | no audio |
| Ctrl+' / Ctrl+, / Ctrl+. | Audio play/pause, seek ±10 s | view 4 | — | no audio |
| Ctrl+Shift+P | Print | view 4 | Ctrl+Shift+P | `printNote` |

Linux-only (no Windows shortcut exists): `searchNotes` Ctrl+K (focus list search),
`saveNote` Ctrl+S (explicit local save; Windows auto-saves).

Not in Windows, therefore not added: find in note, select all notes, delete note from the
list, page navigation, pen/eraser toggles.

## Storage contract

`GET/PUT /api/shortcuts` → `{"schemaVersion":2,"bindings":{<24 action ids>}}`. A binding is
`""` (unassigned) or `(Ctrl[+Alt]|Alt)[+Shift]+KEY`, KEY ∈ A–Z, 0–9, Plus, Minus, Slash,
BracketLeft, BracketRight, Comma, Period, Quote, Semicolon, Enter. Reserved: Ctrl+A/C/V/X/
Z/Y/Q/W, Ctrl+Shift+Z/N/K/I/V. Non-empty bindings are unique. Stored under metadata
`keyboard_shortcuts_v2`; an old `keyboard_shortcuts_v1` document (and a schema 1 PUT) is
upgraded by keeping its three bindings and unassigning colliding new defaults. Rust
`crates/rs-store/src/shortcuts.rs` (+ `notes-engine` backend; the frozen Python reference
is not part of the public repository), UI `desktop/src/useShortcuts.ts`.
