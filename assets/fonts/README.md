# Build-time UI font

The font binary is intentionally not stored in this Git repository. During a clean build,
`build.rs` downloads `NotoSansCJKsc-Regular.otf` from a commit-pinned jsDelivr mirror of the
official [`notofonts/noto-cjk`](https://github.com/notofonts/noto-cjk) repository into Cargo's
`OUT_DIR`, verifies its SHA-256 digest, and embeds it in the application binary.

- Source commit: `165c01b46ea533872e002e0785ff17e44f6d97d8c`
- Source path: `Sans/OTF/SimplifiedChinese/NotoSansCJKsc-Regular.otf`
- SHA-256: `2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b`
- License: SIL Open Font License 1.1; see `OFL.txt`

For an offline build, set `MACINDECODE_UI_FONT_PATH` to a local copy with the same digest.
