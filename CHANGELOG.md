# Changelog

All notable user-visible changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) (pre-1.0: anything
may change).

Evidence words follow the README: "Cloud-verified" means a controlled write
to a test note was confirmed by a full readback; "seen on device" means the
result was also checked in the official app on a phone or tablet.

## [Unreleased]

### Added

- Dark theme: Settings → Tema (Sistem / Açık / Koyu). "Sistem" follows the
  desktop colour scheme live (XDG portal on Linux, system appearance on macOS).
- "Not sayfalarını koyu göster": optional dark note pages, display only — saved
  text and ink colours are never changed.

### Fixed

- Card dates and untitled note names show the year when it is not the current
  one.

### Security

- The local engine serves static UI files only on exact-case paths.

### Documentation

- All public documentation is in English; README gained install, privacy
  and status sections; this changelog was added.

## [0.0.1-alpha] - 2026-10-28

First public pre-release. Alpha quality: keep backups of notes that matter.
The user interface is in Turkish.

### Added

- Samsung account sign-in in the system browser and Samsung Cloud sync into a
  local SQLite library; notes stay readable offline.
- Note list, folder tree, favorites and trash (with the remaining days shown),
  right-click menus, configurable keyboard shortcuts close to the Windows app,
  drag (press and move) to move notes, a folder picker.
- Text editing: plain and rich text (bold, italic, colour, alignment), titles,
  also in notes that contain handwriting and in notes whose text has a picture
  inserted on a phone (the picture anchor is kept). Cloud-verified.
- Pen: draw strokes with colour and thickness, first stroke on an empty page,
  highlighter, eraser, lasso select / move / delete, undo/redo, automatic save
  shortly after the last stroke, drafts kept across restarts. Works on PDF
  pages and on notes with pictures, shapes, text boxes and freely placed
  pictures, which are carried over unchanged. Cloud-verified, seen on device.
- Shapes: rectangle, ellipse, triangle, right triangle, diamond, line and arrow with colour,
  thickness and fill. Cloud-verified and seen on device for a rectangle and a
  line.
- Pages: background colour, add / move / delete pages, insert a PDF, page
  templates. Cloud-verified, seen on device.
- Page previews for every page type (handwriting, PDF, pictures, shapes, text
  boxes) and card thumbnails, rendered in sandboxed helper processes; a live
  pen preview while drawing.
- Save as file (PDF from page images, or plain text) to the Downloads folder;
  copy note text to the clipboard; duplicate text notes.
- Every Cloud write is sent once and confirmed by reading the whole note back;
  a write with an unknown outcome is never retried automatically.
- Packages: AppImage, .deb, .rpm and a Flatpak bundle (x86_64), plus unsigned,
  not notarized macOS disk images (arm64, x86_64) that have not yet been
  tested on a Mac. `SHA256SUMS` covers every file.

### Known limitations

- Not supported: permanent delete, trashing a folder that still contains
  notes, locked/encrypted notes, editing existing shapes, new pen presets,
  duplicating handwritten notes.
- Token renewal is implemented and tested against recorded exchanges but has
  not yet been observed live.
- Previews of pictures, shapes and text boxes are drawn by this client and are
  close to, not identical with, the official renderer.

[Unreleased]: https://github.com/Tunahanyrd/notes/compare/v0.0.1-alpha...HEAD
[0.0.1-alpha]: https://github.com/Tunahanyrd/notes/releases/tag/v0.0.1-alpha
