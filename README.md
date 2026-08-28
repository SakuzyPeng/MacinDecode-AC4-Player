# MacinDecode AC-4 Player

一个纯 Rust、原生桌面的 AC-4 空间音频播放器外壳。当前里程碑只建立 GUI、状态模型和未来平台
后端的边界，不读取 AC-4 码流，也不提交任何音频。

## 当前能力

- Windows、macOS 原生窗口，基于 `egui`/`eframe` 的 Winit + WGPU 后端。
- 选择或拖入 `.m4a`、`.mp4`、`.ac4` 文件，仅记录路径和展示文件信息。
- 展示 presentation、空间后端、对象场景、诊断和传输控制的占位状态。
- 预留 Windows Spatial Audio 与 macOS AU Spatial Mixer 两个后端选项。

## 明确不做

- 不依赖 `MacinDecode-AC4-Core`，尚未建立解码集成。
- 不解析容器、presentation、对象元数据或 PCM。
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
MIT/Apache-2.0 的 `egui`/`eframe` 与 MIT 的 `rfd`，不以 GPLv3 作为发布基础。

架构边界见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)。

