# MacinDecode AC-4 Player

一个纯 Rust、原生桌面的 AC-4 空间音频播放器外壳。当前里程碑建立 GUI、播放列表、只读
bitstream inspection 和未来平台后端的边界，不解码或提交任何音频。

## 当前能力

- Windows、macOS 原生窗口，基于 `egui`/`eframe` 的 Winit + WGPU 后端。
- 多选或拖入 `.m4a`、`.mp4`、`.ac4` 文件，组成可选择、可移除的播放列表。
- 后台检查当前条目的容器与 AC-4 元数据，展示紧凑摘要和分区详情。
- 展示对象场景、诊断、输出设备和传输控制的占位状态。
- 预留 Windows Spatial Audio 与 macOS AU Spatial Mixer 两个后端选项。

## 明确不做

- 只依赖 `MacinDecode-AC4-Core` 的 inspection 层，不接入 Scene 或 Decode engine。
- 不重建对象场景、PCM，也不应用响度、DRC、Dialogue Enhancement 或 downmix。
- 不创建设备、音频流或播放线程。
- 不包含 WebView、HTML、CSS、JavaScript 或 WebAssembly 构建入口。

## 开发

```bash
cargo run
cargo fmt --all -- --check
cargo test
cargo clippy --all-targets -- -D warnings
```

本仓库当前未选择项目发布许可证。第三方依赖的许可证需在引入和发布前持续审计；GUI 栈使用
MIT/Apache-2.0 的 `egui`/`eframe`，`rfd` 与 MacinDecode inspection 使用 MIT，
不以 GPLv3 作为发布基础。

架构边界见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)。
