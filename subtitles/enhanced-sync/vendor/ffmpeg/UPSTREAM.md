# FFmpeg Source Snapshot

Vendored from FFmpeg upstream:

- repository: `https://github.com/FFmpeg/FFmpeg.git`
- commit: `946fcce07b6dcd0331c8cc609192aeff5e1924f8`
- source date: `2026-09-18`
- vendored for: targeted AC-3, E-AC-3, DTS/DCA, DTS-HD MA core fallback,
  and TrueHD/MLP decode-to-FLAC support

The plugin build configures this vendored tree as a narrow static FFmpeg
`avformat`/`avcodec`/`swresample`/`avutil` build and links it into the final
Rust `wasm32-wasip2` component artifact. FFmpeg source files are licensed by
FFmpeg under LGPL-2.1-or-later unless the individual file states otherwise.

Keep the configured build narrow: no programs, only the targeted audio
demuxers/muxer, no filters, and no network support.
