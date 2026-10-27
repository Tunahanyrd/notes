# Windows Samsung Notes parity reference (observed 2026-10-26)

Observed in the user's WinBoat VM: Samsung Notes for Windows 4.3.831.0, One UI 7,
English UI. Screens were captured privately (personal notes, not in the repo).
This file records structure only. The Linux client shows Turkish text.

## Settings (left rail, bottom: gear "Settings")
Title "Settings" with a back arrow. Grouped cards:
1. **Cloud sync**
   - Large on/off toggle row (accent colour when on).
   - Account e-mail row (secondary text) with a sign-out action at the right.
   - Manual sync row with secondary lines: storage used and last sync time.
2. **General**
   - Default note style (default colour, style and layout for new notes) → sub-page.
   - Note lock password.
   - Restore.
   - Auto-save toggle.
3. **Privacy** — privacy notice link.
4. **Dark mode** — radio list: On / Off / Use system settings.
5. About block (no card): app name, version, One UI version, two short notices
   (uninstalling loses unsynced notes; device optimisation note), open-source licences link.
There is no keyboard-shortcut page in Settings.

### Default note style (sub-page, back arrow)
- **Style**
  - Page style: Split pages / Infinite scroll (radio cards with page pictograms).
  - Layout: Normal / Landscape / Long (1:2).
  - Scroll direction: Vertical / Horizontal / 2-page horizontal.
- **Template**
  - Images › Default: 15 built-in backgrounds (blank, 3 line spacings, 3 grids, 3 dot grids,
    checklist, Cornell-like margin line, 2 table layouts, music/lines); downloaded list and add action.
  - PDFs › Default: 15 downloadable PDF templates (download badge); downloaded list and add action.
- **Color**: 18 swatches (same palette as Page settings → Background colour), checkmark on selected.

## Trash
- Title "Trash"; a subtitle explains that each item shows its remaining days before permanent deletion.
- Top-right text buttons: "Edit", "Empty".
- Trashed folders first, as small folder cards (folder-shaped tab, item count, name, days).
- Note cards like All notes, with a dark translucent band at the bottom of the thumbnail:
  remaining days. Title and date under the card.
- Right-click a trashed note: **Restore**, **Delete** (permanent).

## Context menus
- Note card (All notes / folder view), opens at the pointer: Move, Lock, Share, Delete,
  Save as file, Duplicate note, Add to favorites (Remove from favorites when favourite).
- Left-rail folder: right-click selects/opens the folder; no context menu observed.

## All notes header
"All notes" title; actions "Create note", "Import PDF", "Edit", "View"; search box; sort
"Date modified" with direction arrow. Left rail: All notes (count), Trash (count), Folders
(count, collapsible tree with counts), Settings at the bottom.
