# figcraft-psd

Reader for Adobe Photoshop `.psd` (version 1) and `.psb` (version 2) files: the header, image
resources, the layer tree with its channels, masks and tagged blocks, ActionDescriptors, vector
paths, and Photoshop's text engine data (`EngineData`, `Txt2`).

It is PhotoCraft's `photocraft-psd` crate, taken from
[storytold/photocraft](https://github.com/storytold/photocraft) (`crates/psd` at commit
`d6b97e5b4b1d9a03c3ef9b904dd5de864f5e8aa8`, MIT OR Apache-2.0, the same licence as FigCraft),
with the parts FigCraft doesn't use left out (brushes, swatches, actions, patterns, gradients,
styles and other preset files) and PhotoCraft's `EngineData` parser added as `engine_data`.
Fixes that apply to both should be made in PhotoCraft too.

FigCraft's Photoshop import (`figcraft_format::psd`) builds documents from what this crate reads.
