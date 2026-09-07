## 简体中文

这是 MacinDecode AC-4 Player 的首个预览版。你可以用它打开 AC-4 空间音频文件、听取空间效果，
并在三维场景中查看声音的位置。欢迎试用，并通过 [Issues](https://github.com/SakuzyPeng/MacinDecode-AC4-Player/issues)
反馈遇到的问题。

### 下载与安装

在本页下方的 **Assets** 中选择适合你的安装包：

| 你的电脑 | 下载哪个文件 |
| --- | --- |
| Windows 11（推荐保持更新），Intel / AMD 64 位电脑 | 以 `x86_64-pc-windows-msvc.msi` 结尾的文件 |
| macOS 14 或更新版本，Apple 芯片 Mac（M1 及更新机型） | 以 `aarch64-apple-darwin.pkg` 结尾的文件 |

双击安装包，按提示完成安装。应用只安装到当前用户，不需要管理员权限。Mac 上安装后的应用位于
个人文件夹中的 **Applications（应用程序）**。

本预览版安装包尚未正式签名，系统可能提示无法验证开发者。请确认下载来源是本仓库的发布页面。
普通使用只需下载 `.msi` 或 `.pkg`；其他附件用于核对下载文件和记录构建信息，无需安装。

### 可以用它做什么

- 播放含 AC-4 音轨的 `.m4a`、`.mp4` 和 `.ac4` 文件，并查看音频信息。
- 使用系统空间音频，或选择 **SAF binaural**，用普通立体声耳机听取空间效果。
- 在三维场景里观察声音的位置，拖动旋转视角。
- 创建和管理多个播放列表；重新打开应用时保留曲目和播放位置，并保持暂停。
- 在支持的播放模式下使用 AirPods 头部追踪或手动调整听者朝向。

### 第一次播放

1. 将音频文件拖进窗口，或点击 **Add files** 添加文件。
2. 双击列表中的曲目开始播放。
3. 点击 **Audio settings** 选择播放模式。不确定如何设置时，可先选择 **SAF binaural** 并使用耳机。

更多操作见 [中文使用说明](https://github.com/SakuzyPeng/MacinDecode-AC4-Player/blob/v0.1.1/README.md)。

### 使用前了解

- 只支持含 AC-4 音轨的文件，不播放普通 MP3、AAC 或 FLAC。部分 AC-4 文件也可能暂不支持。
- Windows 使用系统空间音频前，需要在系统设置中启用空间声音；使用 Dolby Atmos 还需要安装 Dolby Access。
- macOS 控制中心的「杜比全景声」标签只在 **7.1.4** 系统空间音频输出时生效。
- 部分文件无法拖动进度条跳转；空间效果和头部追踪也取决于系统设置及设备支持。
- 这是预览版，仍需更多设备上的实际使用反馈。遇到问题时，请提供系统版本、输出设备和应用中的错误提示。

## English

This is the first preview of MacinDecode AC-4 Player. Open AC-4 spatial audio files, listen to their
spatial effects, and see where sounds are placed in a live 3D view. Please report problems through
[Issues](https://github.com/SakuzyPeng/MacinDecode-AC4-Player/issues).

### Download and install

Choose an installer from **Assets** at the bottom of this page:

| Your computer | File to download |
| --- | --- |
| Windows 11 (keep it up to date), 64-bit Intel / AMD PC | The file ending in `x86_64-pc-windows-msvc.msi` |
| macOS 14 or later, Apple silicon Mac (M1 or newer) | The file ending in `aarch64-apple-darwin.pkg` |

Double-click the installer and follow its steps. It installs for your user account and does not
require administrator rights. On Mac, find the installed app in **Applications** inside your home folder.

These preview installers are not formally signed, so your system may say it cannot verify the
developer. Make sure you downloaded them from this repository's release page. You only need the
`.msi` or `.pkg` file; the other attachments help verify the download and record how it was built.

### What you can do

- Play `.m4a`, `.mp4`, and `.ac4` files containing AC-4 audio, and inspect their audio information.
- Listen through system spatial audio, or choose **SAF binaural** for ordinary stereo headphones.
- Watch sound positions in the 3D view and drag to change your viewpoint.
- Organize several playlists; the app remembers your track and playback position and reopens paused.
- Use AirPods head tracking or adjust the listener's orientation manually in supported playback modes.

### Your first playback

1. Drag an audio file into the window, or click **Add files**.
2. Double-click a track in the list to play it.
3. Open **Audio settings** to choose a playback mode. If you are unsure, start with **SAF binaural** and headphones.

See the [English user guide](https://github.com/SakuzyPeng/MacinDecode-AC4-Player/blob/v0.1.1/README.en.md)
for more controls and settings.

### Before you start

- Only files containing AC-4 audio are supported. Ordinary MP3, AAC, and FLAC files will not play,
  and some AC-4 files may not be supported yet.
- On Windows, enable spatial sound in system settings before using system spatial audio. Dolby Atmos also requires Dolby Access.
- The macOS Control Center **Dolby Atmos** label only works with **7.1.4** system spatial audio output.
- Some files do not support seeking. Spatial effects and head tracking also depend on your system settings and equipment.
- This is a preview, and feedback from more devices is welcome. When reporting a problem, include
  your operating system version, output device, and any error message shown by the app.
