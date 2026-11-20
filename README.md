# BSDConqueror

A process memory scanner and editor for **FreeBSD**, with a graphical interface
built in Rust (egui/eframe). It relies only on **native system tools**: `ps`,
`procstat`, and `ptrace(2)`. No external tools such as `scanmem` are required.

> Inspired by GameConqueror/scanmem, but written from scratch for FreeBSD.

## What it is for

BSDConqueror helps you find where a program stores a value (lives, coins,
score, etc.) and change or freeze it. The typical workflow is:

1. Select the target process.
2. Search for a value.
3. Change the value in the program and use **Next Search** to narrow the results.
4. Move the final addresses to the bottom list to edit or lock them.

Use it only with programs you own (for example, single-player games). Do not use
it on online games or other people's software.

## Features

- Process list with filtering and column sorting (PID, user, command).
- Search for **1, 2, 4, and 8-byte integers**, **float** (4 bytes), and **double** (8 bytes)
  (default: "2 and 4 Bytes"; "Auto" is also available).
- **Unknown value** search: the first scan takes a memory snapshot, and subsequent scans
  filter by increased, decreased, changed, unchanged, or equal values.
- **Byte-by-byte** scanning of all addresses in exact-value mode. This finds unaligned
  values, which are common in DOS games running inside emulators (such as DOSBox).
  In unknown-value mode, scanning is aligned to the selected value type.
- **Found** list showing the first 20 distinct addresses. If the same address appears
  with multiple widths, only the narrowest one is shown.
- **Real-time value updates** (every 1 s; every 0.3 s when locks are active).
- **Undo** for the last search (up to 5 previous result sets).
- **Bottom list** for addresses of interest:
  - edit the value (double-click or `F2`) and description (double-click);
  - with multiple rows selected, value changes and locks apply to **all selected rows**;
  - **Lock** (padlock): continuously writes the value to keep it fixed;
  - remove rows (`Delete`), select all (`Ctrl+A`), clear the list.
- Interface zoom with **Ctrl + mouse wheel** (also `Ctrl +`, `Ctrl -`, `Ctrl 0`).
- Preferences saved between sessions: zoom, window size, column and panel widths,
  search type, language, and theme.

## Requirements

- FreeBSD (tested on 14.x) with `ps` and `procstat` (part of the base system).
- A graphical environment (X11 or Wayland).
- Recent Rust (1.85+ recommended) and `cargo`.

## Build and run

```sh
git clone git@github.com:Olen1962/bsdconqueror.git
cd bsdconqueror
cargo build --release
./target/release/bsdconqueror
```

Or, using the provided `Makefile` (compatible with `bmake`/FreeBSD and GNU make):

```sh
make
./target/release/bsdconqueror
```

Always use `--release` (or `make`): scanning walks memory byte by byte and is much
slower in debug builds.

## Install (FreeBSD)

To install the binary and desktop integration (`.desktop` file and icons):

```sh
make
make install
```

By default, files are installed under `/usr/local`. You can change the prefix and use
`DESTDIR` for staged installs:

```sh
make PREFIX=/usr install
make DESTDIR=/tmp/pkg PREFIX=/usr/local install
```

To uninstall:

```sh
make uninstall
```

## Permissions

- For processes owned by the **same user**, root is not required as long as
  `security.bsd.unprivileged_proc_debug=1` (the default on FreeBSD).
- For processes owned by **other users** (including `root`), or if that sysctl is set
  to `0`, run the program as root.

If you see `PT_ATTACH failed ... Operation not permitted`, one of these permission
requirements is not met.

## How it works and implications

The program attaches to the target process with `ptrace` (`PT_ATTACH`), reads and
writes memory with `PT_IO`, and detaches immediately afterward.

- **The target process is paused** while attached. The first scan can take several
  seconds, during which the target program remains frozen.
- With active locks, there is a very short pause every 0.3 s.
- Changing a program's memory can break it. Back up your saves.
- Writing 4 bytes to a value that only occupies 2 bytes will also overwrite the next
  2 bytes, which may belong to another variable. When in doubt, use **2 bytes**.

## Shortcuts

| Action | Shortcut |
|---|---|
| Search | `Enter` in the Value box |
| Edit row value | Double-click Value, or `F2` |
| Edit description | Double-click Description |
| Select all (bottom list) | `Ctrl+A` |
| Remove selected rows | `Delete` |
| Deselect | `Esc` |
| Zoom | `Ctrl` + mouse wheel |

## Preferences

Preferences are stored in `~/.config/bsdconqueror/config.ini` (or under
`$XDG_CONFIG_HOME/bsdconqueror/`). It is a plain text file. To restore defaults,
delete it.

## Known limitations

- 1-byte values between 128 and 255 are displayed as negative numbers.
- Exact-value searches are limited to 16 million candidate addresses. Unknown-value
  searches have their own limit (8 million) and also store a value for each address,
  so they use significantly more memory.
- Only readable and writable memory regions are scanned (no device mappings or guard pages).
- **No pointer scan support:** The program does not resolve multi-level pointer chains.
  For the primary target audience on FreeBSD — emulators such as **DOSBox**, ScummVM,
  and retro games — this is generally not necessary, as these environments operate on
  contiguous and static memory blocks (without ASLR or the complex dynamic allocations
  typical of modern 3D engines). Byte-by-byte unaligned scanning and unknown-value
  search cover most needs in these scenarios.

## Project structure

```
src/
├── main.rs          entry point
├── core/
│   └── scanner.rs   ptrace, procstat, memory scanning and writing
├── ui/
│   └── mod.rs       user interface (egui)
└── icons/           PNG icons embedded in the executable (and original SVGs in svg/)
tools/
└── make_icons.py    generates icons (PNG and SVG); requires Python 3 and Pillow
```

The interface is non-blocking: the scanner runs in its own thread and communicates
through channels.

## Roadmap

- Unsigned integer support (e.g. display `0..255` for 1-byte values).
- Integration tests with a real target process (`ptrace` cannot run in unit tests).

## History

| Version | Description |
|---|---|
| 0.30.0 | Float/double, undo, translations, and unknown-value search |
| 0.20.0 | Native scanner (`ptrace`/`procstat`), without scanmem |

## License

**100% free to use.** The original work in this project (code, icons, and
documentation) is in the **public domain** ([Unlicense](LICENSE)):

- anyone may copy, modify, compile, publish, sell, or distribute it, for any purpose,
  commercial or non-commercial;
- **no attribution is required**;
- **no warranty** of any kind: the software is provided "as is", and the authors are
  not liable for any damages, including data loss or corrupted saves.

Any third-party components retain their original rights and licenses, whether obtained,
inherited, or otherwise. This includes libraries fetched via `cargo` (egui/eframe and
others), which are not part of this original work and have their own licenses.
