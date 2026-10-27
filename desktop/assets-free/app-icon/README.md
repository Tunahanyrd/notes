# App icon

`notes-icon.svg` is the original source of the application icon (drawn for
this project; GPL-3.0-or-later like the rest of the code). It deliberately
avoids the look of Samsung Notes' icon: a round indigo-to-teal badge, a
tilted sheet with a folded corner and a handwritten ink stroke ending in an
amber pen nib.

Derived files (regenerate after editing the SVG):

    rsvg-convert -w 1024 -h 1024 desktop/assets-free/app-icon/notes-icon.svg -o /tmp/notes-icon-1024.png
    (cd desktop && npx tauri icon /tmp/notes-icon-1024.png -o /tmp/notes-icons)
    cp /tmp/notes-icons/{32x32,64x64,128x128,128x128@2x,icon}.png \
       /tmp/notes-icons/icon.icns /tmp/notes-icons/icon.ico desktop/src-tauri/icons/
    cp desktop/assets-free/app-icon/notes-icon.svg desktop/public/icon.svg

`desktop/src-tauri/icons/` is what Tauri, the Linux packages (hicolor
32/64/128/256/512) and the macOS bundle (`icon.icns`) use; `desktop/public/icon.svg`
is the window/browser favicon.
