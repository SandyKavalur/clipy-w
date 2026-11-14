# Clipy-W

Clipy-W is a Windows clipboard manager written in Rust. It keeps a local history of copied text and lets you organize reusable snippets into folders.

## Features

- Captures Unicode text copied to the Windows clipboard.
- Keeps the 30 most recent items, grouped into ranges of up to 10.
- Promotes repeated text instead of storing duplicate history entries.
- Lets you create snippet folders and add, edit, rename, or delete snippets.
- Can start automatically when the current Windows user signs in.
- Copies a selected history item or snippet to the clipboard and sends `Ctrl+V` to paste it into the previously active app.

Clipboard history is limited to text. Empty text and clipboard entries larger than 100 KB are ignored.

## Download and Install

On this repository's GitHub page, open **Releases** and download the latest `Clipy-W-Setup-<version>-x64.exe`. Run the installer and follow its prompts. It creates a Start Menu shortcut; no Rust or other development tools are required.

## Use Clipy-W

Press `Win+Shift+V` to open the picker. If Windows cannot register that shortcut, Clipy-W falls back to `Ctrl+Shift+V`. The active shortcut is printed when debug logging is enabled.

Choose a history range to open its entries beside the picker. Choose a snippet folder to open its snippets. Selecting an item copies it and attempts to paste it into the app that had focus before the picker opened.

Choose **Edit Snippets...** to manage folders and snippets. Select a folder, use **Add new snippet**, edit its title and text, then save. Changes are stored automatically when switching items or closing the editor.

Choose **Start at sign-in** in the main popup to toggle automatic startup for the current Windows user. This setting uses the user's Run registry key and does not require administrator privileges. The installer removes the startup entry when Clipy-W is uninstalled.

## Build and Run

Install the Rust stable toolchain and a Windows linker for your Rust target. The MSVC target requires Visual Studio Build Tools with the C++ workload and Windows SDK; the GNU target requires MinGW-w64 tools, including `dlltool`.

From the repository root, run:

```powershell
cargo run --release
```

To build without starting the app:

```powershell
cargo build --release
```

The executable is written to `target\release\clipy-w.exe`.

Run the test suite with:

```powershell
cargo test
```

For startup and hotkey diagnostics:

```powershell
$env:CLIPY_W_DEBUG = "1"
cargo run --release
```

The development hotkey can be changed to `F9` by setting `CLIPY_W_TEST=1` before starting Clipy-W.

## Local Data

Clipy-W stores data in the current Windows user's application data folder:

- Clipboard history: `%APPDATA%\clipy-w\history.db`
- Snippet folders: `%APPDATA%\clipy-w\snippets.json`

## Stop Clipy-W

Clipy-W currently runs in the background without a system-tray menu or in-app quit command. Stop a foreground development run with `Ctrl+C` in its terminal, or close the `clipy-w` process from Task Manager.
