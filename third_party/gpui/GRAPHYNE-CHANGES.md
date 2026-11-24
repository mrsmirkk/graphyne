# gpui 0.2.2 — local changes for Graphyne

This is gpui 0.2.2 from crates.io (Apache-2.0, see `LICENSE-APACHE`, © Zed Industries),
used through `[patch.crates-io]` in the workspace `Cargo.toml`. The `examples/` and `docs/`
folders of the published crate are not vendored; their `[[example]]` entries were removed
from `Cargo.toml`. Everything else is unchanged except:

- `src/platform/windows/directx_devices.rs` — the adapter search kept no device: it created
  a Direct3D 11 device per adapter only to test support, dropped it, then created the real
  device again. On hybrid-GPU laptops each creation costs a large part of startup. The probe
  device (with its context and feature level) is now the one that is used.
- `src/platform/windows/platform.rs`, `directx_devices.rs`, `directx_renderer.rs`, `window.rs` —
  optional timing of the platform start-up, renderer and window creation steps, printed to
  stderr when `GRAPHYNE_STARTUP_TRACE` is set.
- `src/platform/windows/directx_devices.rs` — which GPU renders: upstream takes the first
  adapter Windows lists, which on hybrid laptops can be the discrete NVIDIA GPU; creating a
  device there cost ~250 ms of start-up (waking its driver) against ~70–110 ms on the
  integrated GPU. The minimum-power adapter now goes first when a display is connected to it
  (a laptop's own screen); on desktops whose monitors hang on the discrete card the order is
  Windows' own. "High performance" for graphyne.exe in Windows Settings > Display > Graphics
  (`HKCU\Software\Microsoft\DirectX\UserGpuPreferences`) is respected, and `GRAPHYNE_GPU=high`
  / `low` / `default` overrides it. `get_adapter` no longer reaches `unreachable!()` when no
  adapter supports Direct3D 11: it returns an error.
- `src/platform/windows/keyboard.rs` — punctuation keys were named by the active layout, so
  on the Russian (or any non-Latin) layout Ctrl+/ arrived as `ctrl-.`, Ctrl+[ as `ctrl-х`,
  and those shortcuts and terminal control codes never matched (letter keys were fine:
  `MapVirtualKeyW` names them A–Z on every layout). When the layout's letter keys don't
  type Latin letters, `Keystroke.key` is now the US-layout character of the physical key
  (`us_layout_key`, `layout_is_non_latin`, cached per layout); `key_char` and typed text
  still follow the active layout.
- `src/platform/windows/window.rs`, `display.rs` — window placement on monitors with different
  scale factors: the requested bounds are in the target display's logical pixels, but were
  converted with the scale of the monitor the window is created on (the primary). A window
  restored on a 125 % monitor next to a 100 % one opened across both and moved on every
  restore. The position now uses the target display's scale (`WindowsDisplay::scale_factor`)
  and the size takes back the resize-border growth of WM_DPICHANGED. Nothing changes when all
  monitors share one scale.
- `src/platform.rs`, `src/platform/windows/platform.rs` — `PathPromptOptions::directory`: the
  folder an open dialog starts in (File > Open File… opens in the project, not wherever the last
  dialog of any app was). Used on Windows like the existing folder of the save dialog; other
  platforms ignore it.

To update gpui: copy the new release over this folder (minus `examples/`, `docs/`), drop the
`[[example]]` tables again and re-apply the changes above.
