// Regenerates desktop/assets-free/windows-assets/*.svg from Lucide icons
// (ISC, see ./LICENSE). The file names match the icon names the UI asks for
// (WinIcon in src/WindowsChrome.tsx loads /windows-assets/<name>.svg), so the
// free set is a drop-in replacement for the reference artwork.
//
//   cd desktop && node assets-free/generate.mjs
//
// Icons are rendered as <img>, where currentColor does not inherit, so each
// entry carries its own colour.
import {createElement} from 'react';
import {renderToStaticMarkup} from 'react-dom/server';
import * as lucide from 'lucide-react';
import {mkdirSync, writeFileSync, readdirSync, unlinkSync} from 'node:fs';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';

const out = join(dirname(fileURLToPath(import.meta.url)), 'windows-assets');
const ACCENT = '#f26549';
const GREY = '#6b6b70';
const DARK = '#3d3d42';
const MUTED = '#838383';

// name: [lucide icon, stroke colour, options]
export const ICONS = {
  ac_ic_favorite_off: ['Star', GREY],
  ac_ic_favorite_on: ['Star', '#f5b800', {fill: '#ffcc00'}],
  ac_ic_main_more_composer: ['Ellipsis', DARK, {strokeWidth: 2.6}],
  ac_ic_main_more: ['EllipsisVertical', DARK, {strokeWidth: 2.6}],
  ac_ic_main_pdf: ['FilePlus2', DARK],
  ac_ic_redo: ['Redo2', DARK],
  ac_ic_save_as: ['Download', DARK],
  ac_ic_search_search: ['Search', DARK],
  ac_ic_undo: ['Undo2', DARK],
  back: ['ChevronLeft', '#1d1d1f'],
  Canvas_lock: ['FileLock', DARK],
  compose: ['SquarePen', '#ffffff', {badge: ACCENT}],
  ic_drawer_all: ['NotebookText', GREY],
  ic_drawer_delete: ['Trash2', GREY],
  ic_drawer_favorite: ['Star', GREY],
  ic_drawer_folder: ['Folder', GREY],
  ic_drawer_frequently: ['Bookmark', GREY],
  ic_drawer_lock: ['Lock', GREY],
  ic_drawer_recently: ['Clock', GREY],
  ic_drawer_setting: ['Settings', GREY],
  ic_drawer_sharednotebooks: ['Users', GREY],
  list_ic_newnote_add: ['Plus', '#ffffff', {circle: ACCENT, strokeWidth: 2.4}],
  richtext_tb_align_center: ['AlignCenter', DARK],
  richtext_tb_align_left: ['AlignLeft', DARK],
  richtext_tb_align_right: ['AlignRight', DARK],
  richtext_tb_bold: ['Bold', DARK],
  richtext_tb_bulletpoint: ['List', DARK],
  richtext_tb_checkbox: ['SquareCheck', DARK],
  richtext_tb_indent: ['IndentIncrease', DARK],
  richtext_tb_italic: ['Italic', DARK],
  richtext_tb_numbering: ['ListOrdered', DARK],
  richtext_tb_outdent: ['IndentDecrease', DARK],
  richtext_tb_text_extra: ['Type', DARK],
  richtext_tb_textsize: ['ALargeSmall', DARK],
  richtext_tb_throught: ['Strikethrough', DARK],
  richtext_tb_underline: ['Underline', DARK],
  Search: ['Search', '#1d1d1f'],
  sort_by: ['ArrowDownWideNarrow', MUTED],
  sort_descending: ['ArrowDown', MUTED],
  SPenToText: ['PencilLine', DARK],
  tb_ic_alinment: ['WrapText', DARK],
  tb_ic_canvas_setting: ['Settings', GREY],
  tb_ic_change_style: ['Palette', GREY],
  tb_ic_easy_writing_pad: ['Signature', DARK],
  tb_ic_eraser: ['Eraser', DARK],
  tb_ic_highlight: ['Highlighter', GREY],
  tb_ic_keyboard: ['Keyboard', DARK],
  tb_ic_lasso: ['LassoSelect', DARK],
  tb_ic_pen: ['Pencil', GREY],
};

function inner(name, color, opts) {
  const Icon = lucide[name];
  if (!Icon) throw new Error(`lucide-react has no icon ${name}`);
  const markup = renderToStaticMarkup(createElement(Icon, {
    size: 24, color, strokeWidth: opts.strokeWidth ?? 1.8, fill: opts.fill ?? 'none',
  }));
  // Drop the React class attribute; keep a plain standalone SVG.
  const svg = markup.replace(/ class="[^"]*"/, '').replace(/ aria-hidden="true"/, '');
  return svg.includes('xmlns=') ? svg : svg.replace('<svg ', '<svg xmlns="http://www.w3.org/2000/svg" ');
}

function svgFor(name, color, opts = {}) {
  const icon = inner(name, color, opts);
  if (!opts.badge && !opts.circle) return icon + '\n';
  // Coloured tile/circle behind a white glyph, glyph inset to 60%.
  const shape = opts.badge
    ? `<rect width="24" height="24" rx="4" fill="${opts.badge}"/>`
    : `<circle cx="12" cy="12" r="12" fill="${opts.circle}"/>`;
  const glyph = icon.replace(' xmlns="http://www.w3.org/2000/svg"', '').replace('<svg ', '<svg x="4.8" y="4.8" ').replace(/width="24" height="24"/, 'width="14.4" height="14.4"');
  return `<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24">${shape}${glyph}</svg>\n`;
}

mkdirSync(out, {recursive: true});
for (const file of readdirSync(out)) if (file.endsWith('.svg')) unlinkSync(join(out, file));
for (const [file, [icon, color, opts]] of Object.entries(ICONS)) {
  writeFileSync(join(out, `${file}.svg`), svgFor(icon, color, opts));
}
console.log(`wrote ${Object.keys(ICONS).length} icons to ${out}`);
