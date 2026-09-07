# 安装包与 CI

本轮以完整播放器为发布对象，保留必需的原生运行库。Windows 为当前用户 MSI，macOS 为 Apple Silicon PKG，安装到 `~/Applications`。macOS 最低版本沿用最新原生后端要求的 **14.0**。

Windows 程序安装到 `%LOCALAPPDATA%\Programs\MacinDecode AC-4 Player`；Rust CRT 与原生 C++ CRT 尽量静态链接，包内保留 MacinRender、OpenBLAS 及依赖检查确认必需的 DLL。macOS `.app/Contents/Frameworks` 包含 MacinRender 和头追库。严格单 EXE／全部自有库静态化留待后续，不通过关闭解码或渲染功能来缩小包。

## 构建

Windows 使用 Python 3.12、MSVC、.NET SDK；macOS 使用 Python 3.11+ 和支持 C++20 stop_token 的 Apple 工具链。CI 固定 Xcode 26.3、CMake 3.31.6、Ninja 1.11.1.4、cargo-about 0.9.2 和 WiX 5.0.2。

```sh
python scripts/package.py --target x86_64-pc-windows-msvc
python3 scripts/package.py --target aarch64-apple-darwin
```

脚本为完整默认 feature 准备构建输入：从锁定的 Core 提交取得并生成 ETSI 规范表，从锁定的 MacinRender 提交构建原生库；Boost 1.89.0 与 Windows OpenBLAS 0.3.34 下载均校验 SHA-256。规范表和下载的 SDK 位于被忽略的 `.ci-inputs/`，不作为发布资产。

已有合法输入可通过 `MACINDECODE_AC4_SPEC_DIR`、`MACINRENDER_SOURCE_DIR` 和 `BOOST_ROOT` 指定。CI 使用全新锁定输入；本地覆盖只适合开发，构建清单会记录实际原生提交。

许可报告包括 Rust 依赖、播放器 MIT 许可、Noto CJK 字体及 MacinRender 原生第三方许可，并内嵌于 About 页面。图标沿用 `assets/icons/`，不另建一套品牌资源。

打包后解包核对文件和哈希，再搬移完整程序载荷运行 SQLite/设置检查、原生空输出会话及实际图形窗口。运行时允许系统库和包内已检查的库，拒绝从构建目录或 Homebrew 加载未打包依赖。检查通过才输出 `dist/` 的安装包、校验和和构建清单。失败日志保存在 `target/packaging-failures/`。

## CI 与发布

PR、main 推送和手动执行使用同一 Windows x64 / macOS ARM64 矩阵，运行 workspace 测试、Clippy、打包回归、完整构建、窗口与安装生命周期检查。硬件和真实媒体测试仍按原文档单独运行，不用空输出测试替代实际听验。

应用与原生库在 macOS 使用 ad-hoc 签名，MSI/PKG 本轮未正式签名。PKG 只启用当前用户安装域，无安装脚本；MSI 只写当前用户安装记录。安装器不修改业务数据。

`vX.Y.Z` 标签必须匹配包版本和干净提交。两平台通过后创建预发布 Release 草稿，重复运行可以更新草稿，不能覆盖已公开版本。不同版本的安装器保持稳定身份、支持修复和升级并阻止降级。生命周期升级夹具复用同一程序并提高安装器版本，只验证安装器和数据保留行为。

严格的干净系统、最低系统版本、普通账户和真实音频/头追设备验收仍需实机进行；GitHub runner 预装开发工具，清理 PATH 的测试仅用于发现部署依赖泄漏。
