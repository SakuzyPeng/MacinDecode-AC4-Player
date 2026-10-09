# 安装包与 CI

Windows 发布当前用户 MSI，程序载荷只有一个自包含 EXE。macOS 发布 Apple Silicon PKG，安装到 `~/Applications`；`.app` 内只有主程序、Info.plist、图标和 ad-hoc 签名资源，最低系统为 **14.0**。

Windows 程序安装到 `%LOCALAPPDATA%\Programs\MacinDecode Spatial Player`。Rust 与全部自建 C/C++ 库使用静态 CRT；MacinRender（含 Rust 数值内核）和 SQLite 编入主程序。macOS 渲染和头追代码同样静态链接，Accelerate、CoreMotion 等系统框架继续动态链接。用户数据库、播放列表、设置及自定义 SOFA 的存储格式不变。

Windows 每次构建生成新的 ProductCode，保留固定 UpgradeCode 和组件身份（改名时的一次例外见下节）；即使版本号相同，新 MSI 也能在事务内替换之前的预览构建，不要求先手动卸载。相同版本之间不区分构建先后，较低的 X.Y.Z 仍禁止覆盖较高版本；正式发布仍应递增版本号。再次打开同一份 MSI 会提供修复／卸载入口。安装界面包含准备、执行进度和完成页面，执行进度订阅 Windows Installer 的实际事件，并显示删除文件、快捷方式、注册信息等阶段；单文件载荷的进度可能跳跃，不代表剩余时间。

## 改名后的身份

应用从 MacinDecode AC-4 Player 改名为 MacinDecode Spatial Player，可见名称、可执行文件
`macindecode-spatial-player`、应用 ID / macOS bundle ID `com.macinrender.macindecode-spatial-player`、
安装目录、开始菜单快捷方式和 `HKCU\Software\MacinDecode\SpatialPlayer\Installer` 一起换名。
数据目录跟着应用 ID 换位置，首次启动由播放器复制旧目录，规则见 [STORAGE.md](STORAGE.md#从旧应用-id-迁移)。

- **UpgradeCode 不变**：`package.py` 的 `guid("upgrade")` 仍由旧 ID `com.macinrender.macindecode-ac4-player`
  派生，`test_packaging.py` 钉住其值。新 MSI 因此按 MajorUpgrade 先卸掉旧产品（旧目录、快捷方式、注册表值），
  再装到新目录，不会并排出现两个产品。
- **组件 GUID 改变**：组件的键路径和文件路径都变了，按 Windows Installer 组件规则必须换新 GUID，因此
  `guid("executable")` 改由新 ID 派生。
- **macOS 没有升级关系**：PKG 的标识也换成新 ID，安装到 `~/Applications/MacinDecode Spatial Player.app`；
  旧 `.app` 不会被删除，权限（蓝牙、运动）按新 bundle ID 重新询问。PKG 不带脚本，这一步留给用户。

## 构建

Windows 使用 Python 3.12、MSVC、.NET SDK；macOS 使用 Python 3.11+ 和支持 C++20 stop_token 的 Apple 工具链。CI 固定 Xcode 26.3、CMake 3.31.6、Ninja 1.11.1.4、cargo-about 0.9.2 和 WiX 5.0.2。

WiX UI 扩展同样锁定为 5.0.2，缓存于 `.ci-tools/`。仅引用标准 MSI 对话框和错误／进度文本，不引入可执行的 CustomAction。

```sh
python scripts/package.py --target x86_64-pc-windows-msvc
python3 scripts/package.py --target aarch64-apple-darwin
```

脚本为完整默认 feature 准备构建输入：从锁定的 Core 提交取得并生成 ETSI 规范表，从锁定的 MacinRender 提交构建原生库和 Rust 数值内核。使用仓库固定的 Rust 1.98；生产构建不再需要 OpenBLAS、LAPACKE、Boost 或仅为旧 SDK 下载的 LLVM。规范表与源码位于被忽略的 `.ci-inputs/`，不作为发布资产。

已有合法输入可通过 `MACINDECODE_AC4_SPEC_DIR`、`MACINRENDER_SOURCE_DIR` 指定。CI 使用锁定输入；本地覆盖只适合开发，构建清单会记录实际原生提交。

许可报告包括 Rust 依赖、播放器 MIT 许可、Noto CJK 字体及 MacinRender 原生第三方许可，并内嵌于 About 页面。图标沿用 `assets/icons/`，不另建一套品牌资源。

打包工具与稳定许可输入放在 `.ci-tools/`，避免 Cargo 缓存清理破坏 .NET 工具结构。Core 的 Rust 数值内核使用其共享 `build/rust`；原生许可 bundle 同时覆盖其 Rust 依赖。打包后解包核对文件和哈希，再搬移完整程序载荷运行 SQLite/设置检查、VBAP／内置双耳渲染的实际 Scene 提交、播放进度及图形窗口。运行时只允许系统组件及显卡驱动。分别采集原生渲染与窗口初始化的加载证据，拒绝应用旁边、构建目录或 Homebrew 中的动态库。检查通过才输出 `dist/` 的安装包、校验和和构建清单。失败日志保存在 `target/packaging-failures/`。

## CI 与发布

Cargo 缓存仅在任务成功完成后保存，确保包含 Release 构建结果；`v2-shared-native` 前缀用于共享原生构建目录的新布局。首次运行需重新填充，后续主线、PR 和发布构建可以复用 GitHub 允许其访问的缓存，成功的 PR 运行也会保存本 PR 的缓存。

测试、Clippy 和 Release 复用 `.ci-inputs/macinrender/build/player-native` 下的同一份原生 Release 产物，按源码路径、目标平台和工具链配置区分目录并加文件锁。目录放在 Cargo target 外，避免 rust-cache 的清理误删 CMake 产物。Cargo 并行数按运行器 CPU 数量设置，上限为 4；嵌套 CMake／Rust 编译上限为 2，避免两层并发耗尽内存。Windows 精确命中 Rust 缓存时，发布阶段使用 2 路并行：依赖已编译，此时主要剩下播放器的 ThinLTO，实测 4 路反而更慢。冷构建仍使用最多 4 路并行编译依赖。并行度在缓存恢复后设置，不参与构建缓存身份。

AC-4 规范表随固定 Core 提交缓存，复用前核对提交号和三个表文件的 SHA-256；缺失、变化或生成失败都会失效。缓存命中时保留文件时间，避免相同表数据触发解码器重编译。Core 的构建脚本仍校验锁定的表摘要。UI 字体保存在 `.ci-tools/fonts`，各 Cargo profile 复制校验通过的字体，并发下载由文件锁协调。

许可证工具 `cargo-about` 按运行器平台、目标架构和脚本中的锁定版本独立缓存，版本校验通过后立即保存，不等待播放器构建结束。缓存只包含工具二进制，许可证清单每次重新生成。工具准备单列为 CI 步骤，避免其首次编译时间混入安装包构建。

PR、main 推送和手动执行使用同一 Windows x64 / macOS ARM64 矩阵，运行 workspace 测试、Clippy、打包回归、完整构建、窗口与安装生命周期检查。硬件和真实媒体测试仍按原文档单独运行，不用空输出测试替代实际听验。

Core 源码及其 Rust 构建目录随当前原生提交缓存；不再维护独立的 OpenBLAS SDK 缓存。发布清单以 `native_numerics` 记录当前数值后端，静态库和系统依赖的运行时核验继续执行。

Windows 打包回归还使用独立产品 GUID、注册表键和临时安装目录复现同版本重打包的 1638 错误，并验证旧标识迁移、同版本内容替换、同包重开、修复、跨版本升级、禁止降级和卸载。实际应用的 CI 生命周期检查另外覆盖同版本替换后的用户数据保留。静默安装测试不能替代交互界面的人工验收。

应用在 macOS 使用 ad-hoc 签名，MSI/PKG 本轮未正式签名。PKG 只启用当前用户安装域，无安装脚本；MSI 只写当前用户安装记录。安装器不修改业务数据。

`vX.Y.Z` 标签必须匹配包版本和干净提交。两平台通过后创建预发布 Release 草稿，重复运行可以更新草稿，不能覆盖已公开版本。不同版本的安装器保持稳定身份、支持修复和升级并阻止降级。生命周期升级夹具复用同一程序并提高安装器版本，只验证安装器和数据保留行为。

严格的干净系统、最低系统版本、普通账户和真实音频/头追设备验收仍需实机进行；GitHub runner 预装开发工具，清理 PATH 的测试仅用于发现部署依赖泄漏。

## 静态链接边界

播放器直接使用固定版本的 C ABI，FFI 仍封装在独立 crate，主程序禁止 unsafe。CMake File API 提供有序的传递链接输入；构建脚本拒绝 DLL 导入库和第三方动态库，最终安装包再检查 PE 普通／延迟导入或 Mach-O 架构、依赖及签名。头追探针仅创建和销毁控制对象，不调用采样，也不请求运动权限。

`python scripts/package-player.py` 保留独立程序载荷的打包入口，自动嵌入许可并执行相同的依赖和运行检查，构建清单放在载荷外侧。正式安装包仍使用 `scripts/package.py`，生成 MSI／PKG、SHA-256 和含静态构建信息的清单。

默认功能从公开 HTTPS Git 地址获取锁定提交的 posebridge-core，无需新增凭据。传感器库静态编入播放器；macOS Info.plist 包含 NSBluetoothAlwaysUsageDescription，启动/安装检查不扫描设备。新依赖纳入 About 许可报告；不添加 PoseBridge CLI 或 DLL。
