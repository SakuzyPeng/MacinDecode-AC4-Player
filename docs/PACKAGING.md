# 安装包与 CI

Windows 发布当前用户 MSI，程序载荷只有一个自包含 EXE。macOS 发布 Apple Silicon PKG，安装到 `~/Applications`；`.app` 内只有主程序、Info.plist、图标和 ad-hoc 签名资源，最低系统为 **14.0**。

Windows 程序安装到 `%LOCALAPPDATA%\Programs\MacinDecode AC-4 Player`。Rust 与全部自建 C/C++ 库使用静态 CRT；MacinRender、OpenBLAS 和 SQLite 编入主程序。macOS 渲染和头追代码同样静态链接，Accelerate、CoreMotion 等系统框架继续动态链接。用户数据库、播放列表、设置及自定义 SOFA 的存储格式和位置不变。

## 构建

Windows 使用 Python 3.12、MSVC、.NET SDK；macOS 使用 Python 3.11+ 和支持 C++20 stop_token 的 Apple 工具链。CI 固定 Xcode 26.3、CMake 3.31.6、Ninja 1.11.1.4、cargo-about 0.9.2 和 WiX 5.0.2。

```sh
python scripts/package.py --target x86_64-pc-windows-msvc
python3 scripts/package.py --target aarch64-apple-darwin
```

脚本为完整默认 feature 准备构建输入：从锁定的 Core 提交取得并生成 ETSI 规范表，从锁定的 MacinRender 提交构建原生库；Boost 1.89.0、Windows OpenBLAS 0.3.34 源码及 LLVM 21.1.8 下载均校验 SHA-256。OpenBLAS 使用 clang-cl/MSVC ABI、静态 CRT 和上游自带 C LAPACK，保留 LP64、GENERIC 基线、动态 CPU 分派和最多 64 个原生工作线程；不依赖 Fortran 或 OpenMP 运行库。构建前需要 7-Zip 解包便携 LLVM 工具链。规范表和下载的 SDK 位于被忽略的 `.ci-inputs/`，不作为发布资产。

已有合法输入可通过 `MACINDECODE_AC4_SPEC_DIR`、`MACINRENDER_SOURCE_DIR` 和 `BOOST_ROOT` 指定。CI 使用全新锁定输入；本地覆盖只适合开发，构建清单会记录实际原生提交。

许可报告包括 Rust 依赖、播放器 MIT 许可、Noto CJK 字体及 MacinRender 原生第三方许可，并内嵌于 About 页面。图标沿用 `assets/icons/`，不另建一套品牌资源。

打包工具与稳定许可输入放在 `.ci-tools/`，避免 Cargo 缓存清理破坏 .NET 工具结构。OpenBLAS SDK 按源码、LLVM、MSVC、SDK 和完整配置缓存，接入前运行实数／复数 BLAS、解方程、SVD 和特征分解残差检查。打包后解包核对文件和哈希，再搬移完整程序载荷运行 SQLite/设置检查、VBAP／内置双耳渲染的实际 Scene 提交、播放进度及图形窗口。运行时只允许系统组件及显卡驱动。分别采集原生渲染与窗口初始化的加载证据，拒绝应用旁边、构建目录或 Homebrew 中的动态库。检查通过才输出 `dist/` 的安装包、校验和和构建清单。失败日志保存在 `target/packaging-failures/`。

## CI 与发布

PR、main 推送和手动执行使用同一 Windows x64 / macOS ARM64 矩阵，运行 workspace 测试、Clippy、打包回归、完整构建、窗口与安装生命周期检查。硬件和真实媒体测试仍按原文档单独运行，不用空输出测试替代实际听验。

应用在 macOS 使用 ad-hoc 签名，MSI/PKG 本轮未正式签名。PKG 只启用当前用户安装域，无安装脚本；MSI 只写当前用户安装记录。安装器不修改业务数据。

`vX.Y.Z` 标签必须匹配包版本和干净提交。两平台通过后创建预发布 Release 草稿，重复运行可以更新草稿，不能覆盖已公开版本。不同版本的安装器保持稳定身份、支持修复和升级并阻止降级。生命周期升级夹具复用同一程序并提高安装器版本，只验证安装器和数据保留行为。

严格的干净系统、最低系统版本、普通账户和真实音频/头追设备验收仍需实机进行；GitHub runner 预装开发工具，清理 PATH 的测试仅用于发现部署依赖泄漏。

## 静态链接边界

播放器直接使用固定版本的 C ABI，FFI 仍封装在独立 crate，主程序禁止 unsafe。CMake File API 提供有序的传递链接输入；构建脚本拒绝 DLL 导入库和第三方动态库，最终安装包再检查 PE 普通／延迟导入或 Mach-O 架构、依赖及签名。头追探针仅创建和销毁控制对象，不调用采样，也不请求运动权限。

`python scripts/package-player.py` 保留独立程序载荷的打包入口，自动嵌入许可并执行相同的依赖和运行检查，构建清单放在载荷外侧。正式安装包仍使用 `scripts/package.py`，生成 MSI／PKG、SHA-256 和含静态构建信息的清单。
