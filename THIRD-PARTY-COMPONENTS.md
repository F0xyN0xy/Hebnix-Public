# Third-party components

Third-party components used by `hebnix.exe`. Not ours, not covered by Hebnix's
licence.

## egui-winit

`hebnix_rs/patches/egui-winit` copy of egui-winit 0.35.0 from
crates.io with one addition, marked "hebnix patch" this allows for transparent overlay to work properley.
The published crate carries no licence files, but they can be found here <https://github.com/emilk/egui>.

## vgmstream

Original anthem previews download the official vgmstream r2168 Windows x64
decoder on first use. The pinned archive is verified with SHA-256 before use;
its `COPYING` file and dependency notices are retained beside the decoder in
the application data folder. See [vgmstream](https://github.com/vgmstream/vgmstream).

The anthem bank reader follows the inspected Wwise 150 layout documented by
[wwiser](https://github.com/bnnm/wwiser/blob/master/wwiser/parser/wparser.py).
