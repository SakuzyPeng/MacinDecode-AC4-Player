# MacinDecode AC-4 Player

一个纯 Rust、原生桌面的 AC-4 空间音频播放器。当前 Windows 路径已串联
`MacinDecode-AC4-Core` Full A-JOC 解码、有限 Scene FIFO 与 Windows Spatial Audio，
并保持解码、播放协调和平台输出之间的窄边界。

## 当前能力

- Windows、macOS 原生窗口，基于 `egui`/`eframe` 的 Winit + WGPU 后端。
- 多选或拖入 `.m4a`、`.mp4`、`.ac4` 文件，组成可选择、可移除的播放列表。
- 后台检查当前条目的容器与 AC-4 元数据，展示紧凑摘要和分区详情。
- Windows 后台按 MP4 sample table 或裸流 sync frame 把有界 AU 交给
  `Ac4DecoderSession(audio-decode)`，取得 normalized planar `f32` 对象/LFE PCM 与 OAMD。
- 解码 PCM 进入最多 2 秒的有界 Scene FIFO；达到 300 ms 后报告可用，不把整段 PCM 累积到内存。
- 展示真实对象数、LFE、元数据完整性、解码 AU/Scene frame 和缓冲状态。
- Windows 默认输出端点通过 `ISpatialAudioClient` 创建事件驱动的对象流；Scene 对象映射为动态对象，
  LFE 映射为一个静态 Low Frequency 对象。
- Play/Pause、Stop 回到开头、主音量与静音已连接真实空间流；诊断窗口展示端点容量、对象提交、
  render update、位置更新和欠载计数。
- 主程序继续 `forbid(unsafe_code)`；Windows COM、原始对象缓冲区和 `PROPVARIANT` 生命周期封装在
  独立的 `windows-spatial-audio` crate 中。

## 当前限制

- 暂不实现 macOS 音频解码；macOS 仍保留 GUI 与 inspection。
- Windows 只使用系统默认 render endpoint，尚无设备选择；尚无 seek 和上一曲控制。
- OAMD ramp 会在每个 Windows render quantum 起点取样并插值，quantum 内不做逐采样位置变化。
- 播放中若 Scene configuration generation、对象/LFE 拓扑或所选 presentation 改变，当前 Windows
  对象流会明确报错；需要重新打开来源后按新配置激活，暂不支持流内动态重配置。
- 不应用响度、DRC、Dialogue Enhancement 或额外 downmix；Scene PCM 保持 Core 的 normalized 输出。
- 不调用 Windows 系统媒体解码器；压缩 AC-4 始终交给锁定版本的 `MacinDecode-AC4-Core`。
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
cargo test backend::windows::tests::submits_decoded_scene_to_windows_spatial_audio -- --ignored
cargo test -p macindecode-windows-spatial-audio ended_renderer_releases_objects_without_entering_failed_state -- --ignored
```

真实媒体应只放在仓库根目录被忽略的 `.local-test-media/`，不得提交到 Git。

本仓库当前未选择项目发布许可证。第三方依赖的许可证需在引入和发布前持续审计；GUI 栈使用
MIT/Apache-2.0 的 `egui`/`eframe`，`rfd` 与 MacinDecode inspection 使用 MIT，
不以 GPLv3 作为发布基础。

架构边界见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)，Windows 解码契约见
[docs/WINDOWS_DECODE.md](docs/WINDOWS_DECODE.md)，原生输出契约见
[docs/WINDOWS_SPATIAL_AUDIO.md](docs/WINDOWS_SPATIAL_AUDIO.md)。
