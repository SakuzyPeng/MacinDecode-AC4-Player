# MacinDecode AC-4 Player

一个纯 Rust、原生桌面的 AC-4 空间音频播放器。当前里程碑先在 Windows 接入
`MacinDecode-AC4-Core` 的 Full A-JOC 解码路径，并保持解码、播放协调和平台输出之间的窄边界。

## 当前能力

- Windows、macOS 原生窗口，基于 `egui`/`eframe` 的 Winit + WGPU 后端。
- 多选或拖入 `.m4a`、`.mp4`、`.ac4` 文件，组成可选择、可移除的播放列表。
- 后台检查当前条目的容器与 AC-4 元数据，展示紧凑摘要和分区详情。
- Windows 后台按 MP4 sample table 或裸流 sync frame 把有界 AU 交给
  `Ac4DecoderSession(audio-decode)`，取得 normalized planar `f32` 对象/LFE PCM 与 OAMD。
- 解码 PCM 进入最多 2 秒的有界 Scene FIFO；达到 300 ms 后报告可用，不把整段 PCM 累积到内存。
- 展示真实对象数、LFE、元数据完整性、解码 AU/Scene frame 和缓冲状态。
- 预留 Windows Spatial Audio 与 macOS AU Spatial Mixer 两个后端选项。

## 明确不做

- 暂不把 Scene FIFO 提交给音频设备，因此 play/pause/seek 仍禁用，不能把“解码就绪”表述为播放。
- 暂不实现 macOS 音频解码；macOS 仍保留 GUI 与 inspection。
- 不应用响度、DRC、Dialogue Enhancement 或额外 downmix；Scene PCM 保持 Core 的 normalized 输出。
- 不创建设备、音频流或播放线程。
- 不包含 WebView、HTML、CSS、JavaScript 或 WebAssembly 构建入口。

## 开发

```bash
cargo run
cargo fmt --all -- --check
cargo test
cargo clippy --all-targets -- -D warnings
```

Windows 的完整解码 feature 需要用户从官方 ETSI 规范在本地准备三份锁定表。播放器不会提交或
分发这些文件。先在与 `Cargo.lock` 相同提交的 `MacinDecode-AC4-Core` 检出中运行：

```text
python -m pip install -r scripts/requirements-spec.txt
python scripts/fetch_specs.py
python scripts/generate_spec_tables.py
```

再为 Windows 构建设置两个本机环境变量；路径示例故意留空，避免把开发机路径写入仓库：

```bat
set "PATH=<Rust-1.98-bin>;%PATH%"
set "MACINDECODE_AC4_SPEC_DIR=<MacinDecode-AC4-Core>\spec"
cargo test
cargo clippy --all-targets -- -D warnings
cargo run
```

`MACINDECODE_AC4_SPEC_DIR` 中必须存在：

- `generated/ts103190_pdf_tables.rs`
- `ts_103190_tables.c`
- `ts_103190_tables_part2.c`

本地端到端回归可额外设置 `MACINDECODE_AC4_TEST_MEDIA`，再运行被忽略的媒体测试：

```bat
cargo test decoder::windows::tests::decodes_local_media_into_a_bounded_scene_buffer -- --ignored
```

本仓库当前未选择项目发布许可证。第三方依赖的许可证需在引入和发布前持续审计；GUI 栈使用
MIT/Apache-2.0 的 `egui`/`eframe`，`rfd` 与 MacinDecode inspection 使用 MIT，
不以 GPLv3 作为发布基础。

架构边界见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)，Windows 解码契约见
[docs/WINDOWS_DECODE.md](docs/WINDOWS_DECODE.md)。
