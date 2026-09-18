## 简体中文

MacinDecode AC-4 Player **v0.1.2 预览版**新增耳机补偿、逐对象电平表和自定义听者皮肤，
改进头部追踪、播放切换与安装体验。以下是相对 v0.1.1 的主要变化。欢迎通过
[Issues](https://github.com/SakuzyPeng/MacinDecode-AC4-Player/issues) 反馈使用中遇到的问题。

### 下载与安装

在本页下方的 **Assets** 中选择适合你的安装包：

| 你的电脑 | 下载哪个文件 |
| --- | --- |
| Windows 11（推荐保持更新），Intel / AMD 64 位电脑 | 以 `x86_64-pc-windows-msvc.msi` 结尾的文件 |
| macOS 14 或更新版本，Apple 芯片 Mac（M1 及更新机型） | 以 `aarch64-apple-darwin.pkg` 结尾的文件 |

双击安装包，按提示完成安装。应用只安装到当前用户，不需要管理员权限。Mac 上安装后的应用位于
个人文件夹中的 **Applications（应用程序）**。

已有版本可直接运行新安装包升级，播放列表、设置和已导入的 SOFA、耳机补偿配置及皮肤会保留。

本预览版安装包尚未正式签名，系统可能提示无法验证开发者。请确认下载来源是本仓库的发布页面。
普通使用只需下载 `.msi` 或 `.pkg`；其他附件用于核对下载文件和记录构建信息，无需安装。

### 本次更新

- **耳机补偿（HpTF）**：SAF binaural 模式支持导入和管理 AutoEq 参数均衡器配置，查看频响曲线、
  悬停对比其他配置，调整 Bass / Tilt，并将调整保存成新配置。可从 [AutoEq 网站](https://autoeq.app/)
  下载耳机对应的参数均衡器文本文件。
- **逐对象响度与表带**：新增 dBFS 铭牌、增益环与实测电平内芯；可打开 Meter bank 查看各对象的电平、
  峰值保持和削顶提示，并在 dBFS 与 LUFS-M 之间切换。轨迹尺寸也反映采样时的响度。
- **静音对象与参照系**：持续静音的对象会淡出并计数，地面的增益环保留。黑号表示场景固定、白号表示
  头部固定，虚化时也保持明暗区别；软件双耳和 Windows 对象直通支持内容指定的逐对象头追策略。
- **自定义听者皮肤**：支持导入 Minecraft Steve / Alex PNG 皮肤，包含衣帽外层、独立左右肢体和头部朝向同步。
- **更顺畅的播放准备**：加快大型 SOFA 的准备；输出格式兼容时，跳转、循环和切歌复用已经准备的 HRTF。
  同时改善自定义 SOFA 的输出峰值保护、动态头追响应和 Windows 场景更新。
- **音频设置更易浏览**：按播放模式提供独立页签并记住页签选择，SOFA / HpTF 列表在原地滚动，显示配置的实际启用状态。
- **macOS 播放体验**：Atmos 标识辅助使用连续的长时间线，减少短循环切换对 AirPods 播放的干扰；修正控制中心图标兼容性。
- **Windows 安装体验**：支持同版本安装包覆盖、修复和卸载，增加安装进度与完成界面，升级保留用户数据。
- **中英文用户手册**：安装和上手说明与详细手册分开，新增播放链路、参照系、投影、响度和头追插图。

### 第一次播放

1. 将音频文件拖进窗口，或点击 **Add files** 添加文件。
2. 双击列表中的曲目开始播放。
3. 点击 **Audio settings** 选择播放模式。不确定如何设置时，可先选择 **SAF binaural** 并使用耳机。

更多操作见 [中文上手说明](https://github.com/SakuzyPeng/MacinDecode-AC4-Player/blob/v0.1.2/README.md)
和 [用户手册](https://github.com/SakuzyPeng/MacinDecode-AC4-Player/blob/v0.1.2/docs/MANUAL.md)。
[完整变更](https://github.com/SakuzyPeng/MacinDecode-AC4-Player/compare/v0.1.1...v0.1.2)

### 使用前了解

- 只支持含 AC-4 音轨的文件，不播放普通 MP3、AAC 或 FLAC。部分 AC-4 文件也可能暂不支持。
- Windows 使用系统空间音频前，需要在系统设置中启用空间声音；使用 Dolby Atmos 还需要安装 Dolby Access。
- macOS 控制中心的「杜比全景声」标签只在 **7.1.4** 系统空间音频输出时生效。
- 部分文件无法拖动进度条跳转；空间效果和头部追踪也取决于系统设置及设备支持。
- 这是预览版，仍需更多设备上的实际使用反馈。遇到问题时，请提供系统版本、输出设备和应用中的错误提示。

## English

MacinDecode AC-4 Player **v0.1.2 preview** adds headphone compensation, per-object meters and custom
listener skins, with improvements to head tracking, playback transitions and installation. These
are the main changes since v0.1.1. Please report problems through
[Issues](https://github.com/SakuzyPeng/MacinDecode-AC4-Player/issues).

### Download and install

Choose an installer from **Assets** at the bottom of this page:

| Your computer | File to download |
| --- | --- |
| Windows 11 (keep it up to date), 64-bit Intel / AMD PC | The file ending in `x86_64-pc-windows-msvc.msi` |
| macOS 14 or later, Apple silicon Mac (M1 or newer) | The file ending in `aarch64-apple-darwin.pkg` |

Double-click the installer and follow its steps. It installs for your user account and does not
require administrator rights. On Mac, find the installed app in **Applications** inside your home folder.

Run the new installer to upgrade an existing version. Playlists, settings and imported SOFA files,
headphone profiles and skins are preserved.

These preview installers are not formally signed, so your system may say it cannot verify the
developer. Make sure you downloaded them from this repository's release page. You only need the
`.msi` or `.pkg` file; the other attachments help verify the download and record how it was built.

### What's new

- **Headphone compensation (HpTF):** SAF binaural can import and manage AutoEq parametric EQ profiles,
  plot their response, preview another profile on hover, adjust Bass / Tilt and save the result as a
  new profile. Download a parametric EQ text profile for your headphones from the [AutoEq website](https://autoeq.app/).
- **Per-object loudness and meter bank:** new dBFS nameplates and split footprints show requested gain
  alongside measured level. The optional Meter bank adds peak hold and clip indicators, with dBFS and
  LUFS-M readings. Trail sizes also reflect loudness when each position was sampled.
- **Silent objects and reference frames:** persistently silent objects fade and are counted, while
  their gain rings remain. Black numbers identify scene-relative objects and white numbers identify
  head-locked objects, retaining their contrast during fading. Software binaural and Windows object
  passthrough follow content-declared per-object head-tracking policies.
- **Custom listener skins:** import Minecraft Steve / Alex PNG skins, including clothing layers,
  separate left and right limbs and the tracked head orientation.
- **Faster playback preparation:** large SOFA files prepare faster, and compatible output formats
  reuse the prepared HRTF across seeks, loops and track changes. Custom SOFA peak protection, moving
  head-tracking response and Windows scene updates have also been improved.
- **Easier audio settings:** mode-specific pages remember your selection; SOFA / HpTF lists scroll
  in place and show whether a profile is actually in use.
- **macOS playback:** the Atmos label helper uses a continuous long timeline to reduce interruptions
  around short loops with AirPods, and Control Center icon compatibility has been corrected.
- **Windows installation:** same-version replacement, repair and uninstall support, with installation
  progress and completion pages. Upgrades preserve user data.
- **Chinese and English user manuals:** installation and quickstart are separate from the detailed
  manual, with new diagrams for playback paths, reference frames, projection, loudness and head tracking.

### Your first playback

1. Drag an audio file into the window, or click **Add files**.
2. Double-click a track in the list to play it.
3. Open **Audio settings** to choose a playback mode. If you are unsure, start with **SAF binaural** and headphones.

See the [English quickstart](https://github.com/SakuzyPeng/MacinDecode-AC4-Player/blob/v0.1.2/README.en.md)
and [user manual](https://github.com/SakuzyPeng/MacinDecode-AC4-Player/blob/v0.1.2/docs/MANUAL.en.md)
for more controls and settings.
[Full changelog](https://github.com/SakuzyPeng/MacinDecode-AC4-Player/compare/v0.1.1...v0.1.2)

### Before you start

- Only files containing AC-4 audio are supported. Ordinary MP3, AAC, and FLAC files will not play,
  and some AC-4 files may not be supported yet.
- On Windows, enable spatial sound in system settings before using system spatial audio. Dolby Atmos also requires Dolby Access.
- The macOS Control Center **Dolby Atmos** label only works with **7.1.4** system spatial audio output.
- Some files do not support seeking. Spatial effects and head tracking also depend on your system settings and equipment.
- This is a preview, and feedback from more devices is welcome. When reporting a problem, include
  your operating system version, output device, and any error message shown by the app.
