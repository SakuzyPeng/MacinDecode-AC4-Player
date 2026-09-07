# MacinDecode AC-4 Player

**简体中文** · [English](README.en.md)

一个用来**打开、查看和播放 Dolby AC-4 空间音频文件**的桌面应用。它自带 AC-4 解码器，
不调用系统或第三方媒体解码器；解码出的音频对象既可以送到系统空间音频或耳机双耳渲染，
也会实时画在窗口中央的三维场景里。

> 它不是通用音乐播放器：只处理**含 AC-4 音轨**的 `.m4a`、`.mp4` 和 `.ac4`，不播放 MP3、AAC、FLAC。

## 目录

- [能用它做什么](#能用它做什么)
- [获取应用](#获取应用)
- [上手五步](#上手五步)
- [播放模式怎么选](#播放模式怎么选)
- [平台支持](#平台支持)
- [Windows 空间音频的前提](#windows-空间音频的前提)
- [常见问题](#常见问题)
- [已知限制](#已知限制)
- [从源码构建](#从源码构建)
- [开发者文档](#开发者文档)
- [许可证](#许可证)

## 能用它做什么

- **播放 AC-4 沉浸声**：Windows 走空间音频对象直通，macOS 走系统空间音频，两个平台都可以改用软件双耳（普通耳机即可）。
- **看清文件里有什么**：容器、节目、对象数量、低频声道、码率等信息，不播放也能看。
- **管理多个播放列表**：新建、改名、排序、拖拽、跨列表复制或移动；关掉再打开会回到上次那首歌和断点，并保持暂停。
- **实时三维场景**：每个音频对象在空间中的位置随播放推进移动，可以任意角度观察。
- **出问题时能查**：诊断窗口显示缓冲、解码和输出状态，帮你判断是文件的问题还是设备的问题。

支持的文件：`.m4a`、`.mp4`、`.ac4`，内部必须是 AC-4 音轨。目前主要面向 **Full A-JOC** 沉浸声内容，
其他 AC-4 形式可能无法播放——但仍然可以查看它们的信息。

## 获取应用

目前**还没有公开发布的安装包**。仓库已经带有完整的打包与安装检查流程：给提交打 `vX.Y.Z` 标签后，
CI 会构建 Windows x64 的当前用户 MSI 和 macOS Apple Silicon 的 PKG，并创建预发布草稿；这些安装包
暂未正式签名。在正式发布之前，请按 [从源码构建](#从源码构建) 自己编译运行。

安装位置（发布后）：Windows 装到 `%LOCALAPPDATA%\Programs\MacinDecode AC-4 Player`，
macOS 装到 `~/Applications`，都不需要管理员权限。

## 上手五步

1. 启动应用。
2. 点击侧栏的 **Add files**，或者直接把文件拖进窗口。
3. 侧栏顶部选择播放列表（`+` 新建，`⋯` 管理）。**单击**条目查看信息，**双击**（或按 Enter、右键 → Play）开始播放。
4. 右上角 **Audio settings** 选择播放模式，旁边的下拉框选择输出设备。
5. 底部是播放控制：上一曲 / 播放 / 下一曲、进度条、音量、静音，以及顺序、单曲循环、列表循环和随机。

**三维场景**（窗口中央）：拖动旋转视角，`Shift` + 拖动平移，滚轮缩放；右上角 `ISO` / `TOP` / `BACK` /
`SIDE` / `RESET` 可以直接跳到固定视角，另有按钮切换透视/正交投影和元素编号显示。场景一次最多画 20 个
对象，超出时左下角会说明还有多少没画出来。

**想看更多细节**：文件卡片上的 **Details…** 打开比特流详情窗口，场景标题右侧的 **`...`** 打开诊断窗口。

## 播放模式怎么选

在 **Audio settings** 里切换。选错了也不要紧——切换是热生效的，不会中断当前这首歌的解码。

| 模式 | 可用平台 | 说明 |
| --- | --- | --- |
| **Automatic**（默认） | 全部 | Windows 用对象直通，macOS 用系统空间音频 |
| **Windows object passthrough** | Windows | 把 AC-4 的动态对象原样交给 Windows 空间声音，需要先在系统里启用空间声音格式 |
| **System spatial audio** | macOS / Windows | 先渲染成 7.1.4 / 9.1.6 / 22.2 多声道床（Apple 几何），再交给系统的空间音频。默认 7.1.4 |
| **SAF binaural** | macOS / Windows | 软件双耳渲染，任何普通立体声耳机都能用；内置 KEMAR，也可以选自己的 SOFA 文件 |

- **22.2** 默认把单路 LFE 以等功率复制到两路 LFE，也可以选择直通。
- **macOS 的 7.1.4 / 9.1.6** 默认开启“控制中心 Atmos 标识辅助”，可以关闭；它只影响系统对内容的标识，
  不改变 AC-4 的渲染方式，详见 [播放集成](docs/MACINRENDER.md)。
- **听者朝向**：在 SAF binaural 和 Windows object passthrough 下可调——macOS 可用 AirPods 头部追踪
  （需要带运动权限声明的正式 `.app`），其他情况使用手动朝向（在设置窗口里拖动方块或直接输入角度）。
  系统空间音频模式下由系统负责头部追踪。
- **自定义 HRTF**：选择 SOFA 文件后会复制到数据目录的 `sofa/` 中统一管理，下次可直接从列表选择。

## 平台支持

| | Windows 10+ (x64) | macOS 14+（Apple Silicon） | Linux |
| --- | --- | --- | --- |
| 查看文件信息 | ✅ | ✅ | ✅ |
| 解码 | ✅ | ✅ | ✅ |
| 播放 | 对象直通 / 系统空间音频 / 软件双耳 | 系统空间音频 / 软件双耳 | ❌ |
| 三维场景 | ✅ | ✅ | ✅（按真实时间轴推进的静音预览） |
| 头部追踪 | 手动 | AirPods（正式 `.app`）或手动 | — |

解码在三个平台上都可用；Windows 与 macOS 之外没有播放输出，Linux 上得到的是可以看、可以检查、
可以看场景推进的静音预览。安装包只覆盖 Windows x64 与 Apple Silicon；Intel Mac 可以从源码构建，但未经验证。
应用使用 GPU 绘制场景（Windows 走 DX12），需要可用的显卡驱动。

## Windows 空间音频的前提

对象直通能播多少对象，取决于当前 Windows 空间声音格式能提供的**动态对象数量**：

- **AC-4 L3** 最多 16 个对象：Windows 10 的 Dolby Atmos 耳机或内置扬声器路径即可回放。
- **AC-4 L4** 需要 20 个对象：上述路径需要更新后的 Windows 11，较早版本只提供 16 个；
  Dolby Atmos 家庭影院（HDMI）路径在较早版本上也能提供 20 个。
- 使用 Dolby Atmos 需要安装 **Dolby Access** 并在 Windows 设置里启用对应的空间声音格式。
  Windows Spatial Audio API 本身并不强制使用 Dolby Atmos。

对象数量限制见 Microsoft 的
[Spatial Sound runtime resource limits](https://github.com/MicrosoftDocs/win32/blob/docs/desktop-src/CoreAudio/spatial-sound.md#microsoft-spatial-sound-runtime-resource-implications)。
输出设备下拉框里灰掉的设备就是槽位不够的设备，把鼠标停在上面会说明还差多少。

## 常见问题

**为什么没有声音？**
先看状态栏和诊断窗口：如果显示解码失败，说明这个文件的 AC-4 形式暂不支持；如果显示输出不可用，
多半是播放模式与设备不匹配。Windows 对象直通要求设备提供足够的动态对象槽位（见上一节）；
想先确认文件本身能播，可以切到 **SAF binaural**，它只需要一副普通耳机。

**进度条为什么是灰的？**
打开文件后，后台会并行建立跳转索引，索引完成之前不能拖动——这不影响播放，第一次播放不用等它。
状态栏会显示正在建立索引。少数文件本身没有安全的跳转点，那么进度条会一直不可用。

**拖动进度条为什么没反应？**
拖动过程只是预览，松手才真正跳转，并保持你原来的播放/暂停状态。如果目标位置之前没有安全跳转点，
应用会拒绝这次跳转，当前播放不受影响。

**转头之后声音方向跟不上？**
先排除回放链路的延迟。虚拟声卡、虚拟混音软件和无线耳机都会引入额外缓冲。建议先用声卡直连有线耳机
作为对照，再逐一接入其他设备，就能区分是链路延迟还是头部追踪本身的响应。

**播放列表和设置存在哪里？**

- macOS：`~/Library/Application Support/com.macinrender.macindecode-ac4-player/`
- Windows：`%APPDATA%\com.macinrender.macindecode-ac4-player\data\`
- Linux：`${XDG_DATA_HOME:-~/.local/share}/com.macinrender.macindecode-ac4-player/`

里面是播放列表数据库（`library.sqlite3`）、设置（`settings.json`）、窗口状态（`app.ron`）和你的 SOFA 文件（`sofa/`）。
删掉整个目录就能恢复初始状态。加 `--data-dir <路径>` 启动可以使用独立的数据目录。

**文件改名或移动之后怎么办？**
右键条目 → **Locate file…** 重新指向新位置，该文件在所有播放列表里的引用都会一起更新。
无法读取的条目不会被自动删除，可以随时重试。

**播放中改动了文件会怎样？**
播放期间检测到文件大小或修改时间变化会安全停止；把条目移除再重新添加即可继续。
播放过程中改名或删除文件不影响当前这首——应用使用已经打开的文件句柄。

## 已知限制

- **不做任何自动响度处理**：不应用响度调整、动态范围控制（DRC）、对白增强或额外降混。
- 当前聚焦 **Full A-JOC** 内容，其他 AC-4 编码形式可能无法播放。
- **跳转有前提**：MP4/M4A 需要容器同步样本加上解码器报告的 Full random access；裸 `.ac4` 需要同步帧范围
  加上 Full random access。
- 裸 `.ac4` 在中途改变采样率会安全停止并报错，不跨采样率推测时间线。
- MP4 `moov` 元数据上限 64 MiB，单个 packet 上限约 16 MiB，超出会明确报错。
- 三维场景一次最多绘制 20 个对象。
- 空间效果最终取决于文件内容、系统设置和输出设备能力；软件双耳则支持任何普通立体声设备。

## 从源码构建

### 需要准备

- **Rust 1.98**——`rust-toolchain.toml` 已锁定版本，rustup 会自动安装。
- **Python 3.11+**——用于准备构建输入（Windows 上的 MSI 检查需要 3.12）。
- **CMake、Ninja 和 C++20 工具链**——仅 macOS/Windows 需要，用于编译 MacinRender 原生渲染库；Linux 不需要。
- **网络**——构建脚本会下载校验过的 Noto Sans CJK 字体用于显示中日韩文件名；可用
  `MACINDECODE_UI_FONT_PATH` 指定本地字体文件跳过下载。

### 三种构建规模

| 构建 | 命令 | 需要规范表 | 需要 C++ 工具链 |
| --- | --- | --- | --- |
| 只做检查 | `cargo run --no-default-features` | ❌ | ❌ |
| 解码 + 场景预览 + Windows 对象直通 | `cargo run --no-default-features --features decode` | ✅ | ❌ |
| 完整功能（默认） | `cargo run` | ✅ | ✅（macOS/Windows） |

“规范表”指从官方 ETSI TS 103 190 规范在本地生成的三份表格。**所有平台都需要**——这是构建输入，
不是平台限制；本仓库不会提交或分发这些文件。

### 准备构建输入（做一次即可）

```sh
python scripts/prepare_inputs.py
```

它按 `Cargo.toml` 和 `crates/macinrender/native/CMakeLists.txt` 里锁定的提交检出
[MacinDecode-AC4-Core](https://github.com/SakuzyPeng/MacinDecode-AC4-Core) 与 MacinRender，
生成规范表，并在 Windows 上准备 OpenBLAS 和 Boost，全部放进被忽略的 `.ci-inputs/`。
然后在你的 shell 里指向它们：

```bash
export MACINDECODE_AC4_SPEC_DIR="$PWD/.ci-inputs/ac4-core/spec"
export MACINRENDER_SOURCE_DIR="$PWD/.ci-inputs/macinrender"
export BOOST_ROOT="$PWD/.ci-inputs/boost_1_89_0"
```

```bat
set "MACINDECODE_AC4_SPEC_DIR=%CD%\.ci-inputs\ac4-core\spec"
set "MACINRENDER_SOURCE_DIR=%CD%\.ci-inputs\macinrender"
set "BOOST_ROOT=%CD%\.ci-inputs\boost_1_89_0"
```

`MACINDECODE_AC4_SPEC_DIR` 指向的目录里必须有 `generated/ts103190_pdf_tables.rs`、
`ts_103190_tables.c` 和 `ts_103190_tables_part2.c`。Windows 的完整构建还需要 OpenBLAS 相关的
CMake 变量，`scripts/prepare_inputs.py` 会一并准备；如果不想自己拼这些变量，直接用下面的打包脚本，
它会在同一个进程里准备输入再构建。

### 日常命令

```bash
cargo run
cargo test
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

普通 `cargo test` 不需要音频设备，也不需要真实媒体。涉及硬件和真实媒体的回归测试是 ignored 的，
需要设置 `MACINDECODE_AC4_TEST_MEDIA` 后单独运行：

```sh
cargo test decoder::worker::tests::decodes_local_media_into_a_bounded_scene_buffer -- --ignored
cargo test decoder::worker::tests::seeks_real_media_across_epochs_on_the_open_file -- --ignored
cargo test backend::windows::tests::submits_decoded_scene_to_windows_spatial_audio -- --ignored
cargo test -p macindecode-windows-spatial-audio ended_renderer_releases_objects_without_entering_failed_state -- --ignored
```

真实媒体只放在仓库根目录被忽略的 `.local-test-media/`，不要提交到 Git。

### 打包

```sh
python scripts/package.py --target x86_64-pc-windows-msvc
python3 scripts/package.py --target aarch64-apple-darwin
```

脚本自己准备锁定的构建输入，构建 release，检查实际载荷、系统依赖、原生渲染和图形窗口，通过后才在
`dist/` 输出安装包、校验和与构建清单。PR、`main` 推送和手动触发都会在两个平台跑同一套流程，
详见 [安装包与 CI](docs/PACKAGING.md)。

## 开发者文档

[架构](docs/ARCHITECTURE.md) ·
[播放集成（MacinRender）](docs/MACINRENDER.md) ·
[Windows 解码](docs/WINDOWS_DECODE.md) ·
[Windows Spatial Audio](docs/WINDOWS_SPATIAL_AUDIO.md) ·
[播放列表与持久化](docs/PLAYLISTS.md) ·
[数据目录与 SOFA](docs/STORAGE.md) ·
[安装包与 CI](docs/PACKAGING.md)

## 许可证

本项目采用 [MIT License](LICENSE)。应用的 About 页面内嵌了全部第三方依赖的许可声明。
