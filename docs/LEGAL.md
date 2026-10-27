# Legal notes

## Unofficial project

Notes is an independent, unofficial client. It is not
affiliated with, endorsed by, or sponsored by Samsung Electronics Co., Ltd.

## Trademarks

"Samsung", "Samsung Notes" and "Galaxy" are trademarks of Samsung Electronics
Co., Ltd. They are used in this project only to describe compatibility (for
example "compatible with Samsung Notes"). The product name must never contain
these marks; it is defined once in `branding.json`.

## No Samsung material

This client was implemented independently to interoperate with Samsung Notes. The file formats and protocol were documented through interoperability analysis (network observation, file inspection and static analysis of the official clients); the project contains no Samsung code, binaries or assets.

Third-party open-source code used by the project is not Samsung code. In
particular the ink renderer links the [sdocx](https://github.com/twangodev/sdocx)
library (GPL-3.0-only), fetched at a pinned commit and attributed in `NOTICE`.
Our own code is licensed GPL-3.0-or-later; the binaries that link sdocx
(`ink-render` and the `notes-ink-live` WebAssembly module) are GPL-3.0-only
as a whole;
other dependencies and their licenses are listed in the generated third-party
license file.

Contributions must follow the same rule: describe formats and behaviour in
your own words and keep only the facts needed for interoperability (field
names, numeric values, wire shapes). See `CONTRIBUTING.md`.

## Use of third-party services

The client connects to Samsung's online services through private,
undocumented interfaces, which can change at any time. You are solely
responsible for how you use this software and for complying with the terms of
any third-party service you connect it to, including Samsung's terms.

## No warranty, limitation of liability

This program is free software under the GNU General Public License, version 3
or (at your option) any later version. It is provided "as is", without
warranty of any kind, express or implied; see sections 15 (Disclaimer of
Warranty) and 16 (Limitation of Liability) of the license. The authors and
contributors are not liable for account actions taken by a service provider,
data loss, or any other consequences of using the software.
