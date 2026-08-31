# MacinDecode AC-4 Player

一个用于打开、检查和播放 AC-4 空间音频文件的原生桌面应用。Windows 版本支持实际播放，
macOS 版本目前用于查看文件信息。

## 使用

1. 启动应用，点击添加文件或直接把文件拖入窗口。
2. 从播放列表中选择要播放的内容。
3. 在 Windows 上选择输出设备，并使用播放、暂停、进度跳转、停止、音量和静音控制。
4. 如需排查文件或播放问题，可打开详情与诊断窗口。

支持 `.m4a`、`.mp4` 和 `.ac4` 文件；文件本身需要包含 AC-4 音频。

## 主要功能

- 一次添加多个文件，并在播放列表中选择或移除内容。
- 查看容器、节目、对象数量、低频声道和其他 AC-4 信息。
- 在兼容的 Windows 音频设备上播放 Full A-JOC 空间音频，并记住所选设备。
- MP4/M4A 支持安全精确跳转；包含完整随机访问点的裸 `.ac4` 也可跳转。
- Stop 回到开头并暂停，不重新读取文件；设备断开或 Scene 拓扑变化时自动恢复。
- 显示缓冲、解码和输出状态，便于判断文件是否可播放。

## Windows 播放要求

- 回放能力取决于当前 Windows 空间声音格式可提供的动态音频对象数量。
- AC-4 L3 最多包含 16 个对象，可在 Windows 10 的 Dolby Atmos 耳机或内置扬声器路径上回放。
- AC-4 L4 需要 20 个对象；上述 Dolby Atmos 路径需要更新后的 Windows 11，较早版本只提供 16 个对象。
  Dolby Atmos 家庭影院（HDMI）路径在较早的 Windows 版本上也可提供 20 个对象。
- 使用 Dolby Atmos 时需要安装 Dolby Access，并在 Windows 中启用对应的空间声音格式；
  Windows Spatial Audio API 本身不强制使用 Dolby Atmos。

对象数量限制见 Microsoft 的
[Spatial Sound runtime resource limits](https://github.com/MicrosoftDocs/win32/blob/docs/desktop-src/CoreAudio/spatial-sound.md#microsoft-spatial-sound-runtime-resource-implications)。

## 平台与限制

- Windows：支持文件检查、解码和空间音频播放。
- macOS：目前仅支持界面和文件检查，不支持音频解码与播放。
- 当前聚焦 Full A-JOC 内容，其他 AC-4 编码形式可能无法播放。
- 进度条会在后台 seek 索引完成后启用。拖动只预览，松开时执行一次跳转，并保持原来的播放/暂停状态。
- MP4 seek 同时要求容器同步样本和 AC-4 Full random access；裸流要求 Core 报告 Full random access。
  目标之前没有安全点时会拒绝跳转，当前播放不受影响。
- 裸 `.ac4` 中途改变采样率会安全停止并报错，不跨采样率推测时间线。
- 活动文件的压缩数据保存在内存中；seek、Stop、设备恢复和拓扑恢复都不重新读盘。
  文件在磁盘上被外部修改后，需要重新选择才会生效。
- 尚不支持上一曲按钮；可直接在播放列表中选择另一文件。
- 不自动应用响度调整、动态范围控制、对白增强或额外降混。

实际空间音频效果取决于文件内容、Windows 版本和输出设备能力。

## 后续计划

- 增加面向扬声器系统的空间音频渲染。
- 在 Apple 平台接入 AU Spatial Mixer。
- 适配头部追踪交互；Windows 版本也会提供对应功能，但使用鼠标或其他指针输入模拟头部朝向。

## 构建与开发

```bash
cargo run
cargo fmt --all -- --check
cargo test
cargo clippy --all-targets -- -D warnings
```

Windows 的完整解码功能需要从官方 ETSI 规范在本地生成三份锁定表。本仓库不会提交或分发这些文件。
在与 `Cargo.lock` 锁定版本一致的 `MacinDecode-AC4-Core` 检出中运行：

```text
python -m pip install -r scripts/requirements-spec.txt
python scripts/fetch_specs.py
python scripts/generate_spec_tables.py
```

随后设置构建环境并运行项目：

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

本地端到端回归可额外设置 `MACINDECODE_AC4_TEST_MEDIA`，再运行：

```bat
cargo test decoder::windows::tests::decodes_local_media_into_a_bounded_scene_buffer -- --ignored
cargo test backend::windows::tests::submits_decoded_scene_to_windows_spatial_audio -- --ignored
cargo test -p macindecode-windows-spatial-audio ended_renderer_releases_objects_without_entering_failed_state -- --ignored
cargo test -p macindecode-windows-spatial-audio opens_enumerated_endpoints_by_stable_id -- --ignored
```

真实媒体应只放在仓库根目录被忽略的 `.local-test-media/`，不得提交到 Git。

开发者文档：[架构](docs/ARCHITECTURE.md) · [Windows 解码](docs/WINDOWS_DECODE.md) ·
[Windows Spatial Audio](docs/WINDOWS_SPATIAL_AUDIO.md)

## 许可证

本项目采用 [MIT License](LICENSE)。
