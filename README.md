# MacinDecode AC-4 Player

一个用于打开、检查和播放 AC-4 空间音频文件的原生桌面应用。解码在 Windows、macOS 和 Linux 上
都可用；Windows 与 macOS 支持播放，Linux 提供实时场景预览。

## 使用

1. 启动应用，点击添加文件或直接把文件拖入窗口。
2. 从播放列表中选择要播放的内容。
3. 在 Audio settings 中选择播放模式；使用上一曲、下一曲、播放、暂停、进度跳转、音量和静音控制。
4. 如需排查文件或播放问题，可打开详情与诊断窗口。

支持 `.m4a`、`.mp4` 和 `.ac4` 文件；文件本身需要包含 AC-4 音频。

## 主要功能

- 一次添加多个文件，并在播放列表中选择或移除内容。
- 查看容器、节目、对象数量、低频声道和其他 AC-4 信息。
- 在兼容的 Windows 音频设备上播放 Full A-JOC 空间音频，并记住所选设备。
- macOS 默认通过 MacinRender 渲染多声道床，再交给系统空间音频；Windows 默认保留原始对象直通。
- 系统床固定 Apple 几何，开放 7.1.4／9.1.6／22.2，默认 7.1.4；22.2 默认等功率复制 LFE。
- macOS 7.1.4／9.1.6 默认开启可关闭的控制中心 Atmos 标识辅助；22.2 跳过辅助链，详见 [播放集成](docs/MACINRENDER.md)。
- 支持 SAF 软件双耳、内置 KEMAR／用户 SOFA，以及独立的手动朝向和 macOS AirPods 采集。
- 支持上一曲、下一曲切换，以及顺序播放、单曲循环、列表循环和随机播放；默认顺序播放且不循环。
- MP4/M4A 支持安全精确跳转；包含完整随机访问点的裸 `.ac4` 也可跳转。
- 播放结束后可复用已打开的文件从头重播；设备断开或 Scene 拓扑变化时自动恢复。
- 解码失败后设置仍可操作，并可点击播放从头重试，无需重启应用。
- 显示缓冲、解码和输出状态，便于判断文件是否可播放。

## Windows 播放要求

- 对象直通能力取决于当前 Windows 空间声音格式可提供的动态音频对象数量。
- AC-4 L3 最多包含 16 个对象，可在 Windows 10 的 Dolby Atmos 耳机或内置扬声器路径上回放。
- AC-4 L4 需要 20 个对象；上述 Dolby Atmos 路径需要更新后的 Windows 11，较早版本只提供 16 个对象。
  Dolby Atmos 家庭影院（HDMI）路径在较早的 Windows 版本上也可提供 20 个对象。
- 使用 Dolby Atmos 时需要安装 Dolby Access，并在 Windows 中启用对应的空间声音格式；
  Windows Spatial Audio API 本身不强制使用 Dolby Atmos。

对象数量限制见 Microsoft 的
[Spatial Sound runtime resource limits](https://github.com/MicrosoftDocs/win32/blob/docs/desktop-src/CoreAudio/spatial-sound.md#microsoft-spatial-sound-runtime-resource-implications)。

在 Windows 体验手动头部追踪时，建议先确认回放链路本身延迟正常。虚拟声卡、虚拟混音软件及无线耳机
可能引入额外的缓冲或传输延迟，让声音方向的变化落后于操作。可先选择声卡直接输出到有线耳机作为对照，
再逐一接入虚拟声卡或无线耳机，便于区分回放链路延迟与头部追踪本身的响应。

## 平台与限制

- Windows：支持文件检查、解码和空间音频播放。
- macOS：支持系统空间音频和 SAF 双耳；AirPods 采集需要包含运动权限声明的 `.app`。
- Linux：支持文件检查、解码和按实际时间轴推进的 3D 场景预览。
  用 `--no-default-features` 构建可以关掉解码，得到一个只做检查的外壳（也是唯一不需要规范表的配置）；
  这个外壳在**所有平台**上都能构建，Windows 上也一样——那里它同样不提供播放，因为没有可播的东西。
- 当前聚焦 Full A-JOC 内容，其他 AC-4 编码形式可能无法播放。
- 进度条会在后台 seek 索引完成后启用。拖动只预览，松开时执行一次跳转，并保持原来的播放/暂停状态。
- MP4 seek 同时要求容器同步样本和 AC-4 Full random access；裸流要求 Core 报告 Full random access。
  目标之前没有安全点时会拒绝跳转，当前播放不受影响。
- 裸 `.ac4` 中途改变采样率会安全停止并报错，不跨采样率推测时间线。
- 压缩音频按帧读取，不把长媒体完整读入内存。检查、索引和解码复用打开的文件与 MP4 元数据，
  各自使用 256 KiB 读缓冲；定位索引最多保存 8192 个安全起点，精确 seek 从安全点向前解码。
- seek、重播和恢复沿用打开的文件句柄；文件改名或删除后仍可继续访问原文件。
  Windows 打开期间拒绝就地写入；读取时检测到文件大小或修改时间改变会停止，移除并重新添加文件可重新载入。
- MP4 `moov` 元数据上限为 64 MiB，单个 packet 上限约 16 MiB。超限会明确报错，不按文件总长度分配内存。
- 不自动应用响度调整、动态范围控制、对白增强或额外降混。

系统空间音频效果取决于文件内容、操作系统设置和输出设备能力。软件双耳支持普通立体声输出设备。

## 后续计划

- 增加直接面向物理多声道扬声器设备的输出。
- 扩展实时 Scene 渲染器到 EAR、HOA 与 Apple AUSpatialMixer。

## 构建与开发

默认构建还会从锁定源码编译 MacinRender，需 CMake/Ninja 与 C++20 工具链。原生依赖、平台设置、
源代码覆盖及应用打包见 [MacinRender 集成](docs/MACINRENDER.md)。
`--no-default-features --features decode` 可关闭 MacinRender，保留原有 Windows 对象直通和其他平台预览。

```bash
cargo run
cargo fmt --all -- --check
cargo test
cargo clippy --all-targets -- -D warnings
```

完整解码功能需要从官方 ETSI 规范在本地生成三份锁定表——**所有平台都一样**，这是构建输入而不是
平台限制。本仓库不会提交或分发这些文件。在与 `Cargo.lock` 锁定版本一致的 `MacinDecode-AC4-Core`
检出中运行：

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

```bash
export MACINDECODE_AC4_SPEC_DIR=<MacinDecode-AC4-Core>/spec
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
cargo test decoder::worker::tests::decodes_local_media_into_a_bounded_scene_buffer -- --ignored
cargo test backend::windows::tests::submits_decoded_scene_to_windows_spatial_audio -- --ignored
cargo test -p macindecode-windows-spatial-audio ended_renderer_releases_objects_without_entering_failed_state -- --ignored
cargo test -p macindecode-windows-spatial-audio opens_enumerated_endpoints_by_stable_id -- --ignored
```

真实媒体应只放在仓库根目录被忽略的 `.local-test-media/`，不得提交到 Git。

开发者文档：[架构](docs/ARCHITECTURE.md) · [Windows 解码](docs/WINDOWS_DECODE.md) ·
[Windows Spatial Audio](docs/WINDOWS_SPATIAL_AUDIO.md)

## 许可证

本项目采用 [MIT License](LICENSE)。
