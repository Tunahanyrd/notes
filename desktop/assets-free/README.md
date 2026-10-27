# Free UI icon set

Drop-in replacement for the icon files the UI loads from
`/windows-assets/<name>.svg` (see `WinIcon` in `src/WindowsChrome.tsx`).
Every file is rendered from [Lucide](https://lucide.dev) 0.577 (ISC, with
portions from Feather under MIT; see `LICENSE` in this directory). The file
names are kept so components do not change. The public repository ships
these files as `desktop/public/windows-assets/`.

Regenerate after changing the table in `generate.mjs`:

    cd desktop && node assets-free/generate.mjs

| File | Lucide icon | Colour / notes |
|---|---|---|
| `ac_ic_favorite_off.svg` | `Star` | #6b6b70 |
| `ac_ic_favorite_on.svg` | `Star` | #f5b800 {fill: '#ffcc00'} |
| `ac_ic_main_more_composer.svg` | `Ellipsis` | #3d3d42 {strokeWidth: 2.6} |
| `ac_ic_main_more.svg` | `EllipsisVertical` | #3d3d42 {strokeWidth: 2.6} |
| `ac_ic_main_pdf.svg` | `FilePlus2` | #3d3d42 |
| `ac_ic_redo.svg` | `Redo2` | #3d3d42 |
| `ac_ic_save_as.svg` | `Download` | #3d3d42 |
| `ac_ic_search_search.svg` | `Search` | #3d3d42 |
| `ac_ic_undo.svg` | `Undo2` | #3d3d42 |
| `back.svg` | `ChevronLeft` | #1d1d1f |
| `Canvas_lock.svg` | `FileLock` | #3d3d42 |
| `compose.svg` | `SquarePen` | #ffffff {badge: #f26549} |
| `ic_drawer_all.svg` | `NotebookText` | #6b6b70 |
| `ic_drawer_delete.svg` | `Trash2` | #6b6b70 |
| `ic_drawer_favorite.svg` | `Star` | #6b6b70 |
| `ic_drawer_folder.svg` | `Folder` | #6b6b70 |
| `ic_drawer_frequently.svg` | `Bookmark` | #6b6b70 |
| `ic_drawer_lock.svg` | `Lock` | #6b6b70 |
| `ic_drawer_recently.svg` | `Clock` | #6b6b70 |
| `ic_drawer_setting.svg` | `Settings` | #6b6b70 |
| `ic_drawer_sharednotebooks.svg` | `Users` | #6b6b70 |
| `list_ic_newnote_add.svg` | `Plus` | #ffffff {circle: #f26549, strokeWidth: 2.4} |
| `richtext_tb_align_center.svg` | `AlignCenter` | #3d3d42 |
| `richtext_tb_align_left.svg` | `AlignLeft` | #3d3d42 |
| `richtext_tb_align_right.svg` | `AlignRight` | #3d3d42 |
| `richtext_tb_bold.svg` | `Bold` | #3d3d42 |
| `richtext_tb_bulletpoint.svg` | `List` | #3d3d42 |
| `richtext_tb_checkbox.svg` | `SquareCheck` | #3d3d42 |
| `richtext_tb_indent.svg` | `IndentIncrease` | #3d3d42 |
| `richtext_tb_italic.svg` | `Italic` | #3d3d42 |
| `richtext_tb_numbering.svg` | `ListOrdered` | #3d3d42 |
| `richtext_tb_outdent.svg` | `IndentDecrease` | #3d3d42 |
| `richtext_tb_text_extra.svg` | `Type` | #3d3d42 |
| `richtext_tb_textsize.svg` | `ALargeSmall` | #3d3d42 |
| `richtext_tb_throught.svg` | `Strikethrough` | #3d3d42 |
| `richtext_tb_underline.svg` | `Underline` | #3d3d42 |
| `Search.svg` | `Search` | #1d1d1f |
| `sort_by.svg` | `ArrowDownWideNarrow` | #838383 |
| `sort_descending.svg` | `ArrowDown` | #838383 |
| `SPenToText.svg` | `PencilLine` | #3d3d42 |
| `tb_ic_alinment.svg` | `WrapText` | #3d3d42 |
| `tb_ic_canvas_setting.svg` | `Settings` | #6b6b70 |
| `tb_ic_change_style.svg` | `Palette` | #6b6b70 |
| `tb_ic_easy_writing_pad.svg` | `Signature` | #3d3d42 |
| `tb_ic_eraser.svg` | `Eraser` | #3d3d42 |
| `tb_ic_highlight.svg` | `Highlighter` | #6b6b70 |
| `tb_ic_keyboard.svg` | `Keyboard` | #3d3d42 |
| `tb_ic_lasso.svg` | `LassoSelect` | #3d3d42 |
| `tb_ic_pen.svg` | `Pencil` | #6b6b70 |
