# MacinRender 播放集成

Player 将 AC-4 Core 的 Scene 转换为 renderer-native Scene，经 MacinRender C ABI v1.36
进行空间渲染及设备输出。Core 类型和平台指针均不进入 GUI；FFI 封装集中在独立 crate。

## 播放策略

- 自动模式：Windows 使用原始动态对象直通，macOS 使用系统空间音频。
- 系统空间音频：SAF VBAP、Apple 几何；开放 7.1.4、9.1.6、22.2，默认 7.1.4。
  22.2 默认将单路 LFE 以每路 `1/sqrt(2)` 复制到双 LFE，也可选择 direct。
- 软件双耳：SAF HRTF，默认内置 KEMAR，可选择用户 SOFA。
- 系统模式跟随系统默认输出。Windows 的固定床超出静态槽位的位置采用所选 Apple 几何；
  静态槽位的最终角度由 Windows 的空间化器决定。
- 软件朝向在 Mac 优先使用 AirPods，缺失或权限不可用时使用手动朝向；Windows 使用手动朝向。
  系统空间音频模式保持上游中性姿态，macOS 的系统头追由系统负责。

## 时间线与线程

Scene FIFO 始终只有一个消费者。软件渲染路径由独立 producer 线程提交 PCM，保留有界的纯元数据
历史；图像位置按设备呈现进度解析。ASBR 的 PTS 时钟与渲染器已经 pull 的帧数独立，队列里的
未播放音频不推进 GUI。实时 callback 路径使用上一周期消费进度，精度明确为 callback 级。

Core 的元数据 bitmask 逐字段转换（例如 Core POSITION=bit 3，而 renderer POSITION=bit 2）。
重叠裁剪保留进行中的 ramp；缺口提交显式静音 Scene；seek 的 epoch 屏障同时清理设备及渲染队列。
软件路径允许 generation 在帧边界改变拓扑；同 generation 内改变元素集合会报错。

重播与跳回播放时间零点时，通过保留的文件句柄读取首个编码 AU 开始解码，保留 MP4 priming 所需的预卷。
不能直接跳到呈现时间为零的较晚同步样本，否则可能缺失 A-SPX 的跨帧历史。解码失败后，播放按钮
允许显式从头重试；重试复用打开的文件与索引、创建新的 Core 会话，并用新 epoch 丢弃旧事件与 Scene。
中途定位时，先在解码线程内验证与正常起播相同的预缓冲量，再发布 PCM、元数据与就绪事件；若随机访问点
缺少延迟控制所需的历史，可在发布之前退回更早的起点重试。剩余媒体不足预缓冲量时，在确认 EOS 后发布尾部。
解码失败会取消未完成的输出准备与交接、保留切换前的设置并重新启用设置控件，不会一直等待已销毁输出的就绪通知。

SOFA 热切换在独立加载线程进行，使用拥有独立错误结果的 C ABI，避免阻塞送音或播放进度。
需要更换输出格式时，先在后台准备暂停状态的新输出，旧输出继续播放；准备成功后才从当时的呈现位置
进行安全 seek 和交接。准备失败或原播放 epoch 已改变时丢弃候选输出。SOFA 文件选择使用异步对话框。
实时双耳渲染器复用有界的只读 HRTF 缓存（最多四组、64 MiB），SOFA 路径、文件时间和大小变化会使缓存失效；
FFT 工作区、卷积尾部及对象状态仍由每个渲染器独立持有。
实时路径在 FFT 与插值表准备完成后释放原始时域 HRIR 和测量方向数组，缓存只保留回放需要的
频域 HRTF 与压缩插值表；离线 SAF spreader 仍保留其需要的时域数据。以 48 kHz、777 方向、
双耳各 1024 taps 的数据集计算，每份实时缓存少保留约 6.1 MiB 数据，音频算法和采样精度不变。
头追改变预渲染上限时，生产端与消费端使用同一个有效起播门槛；已有缓冲达到新上限也会立即发布就绪，
避免循环重启时停在零帧并等待头追更新超时。输出尚在缓冲时，状态栏会明确显示缓冲状态。
原生采集在自己的 NSOperationQueue 接收传感器回调，Rust 只轮询快照；停止时先排空回调。

独立头部控制线程使用 canonical X-right/Y-front/Z-up 四元数，yaw 绕 Z、pitch 绕 X、roll 绕 Y，
构成为 `Rz(yaw) Rx(pitch) Ry(roll)`。音频使用 world-to-head 逆旋转；场景保留世界坐标，角色
头部使用正向旋转。相机的 orbit/pan 不修改听者。窗口隐藏时传感器处理继续运行。

## 原生源码构建

默认 feature 包含 `decode` 和 `macinrender`。Cargo 调用 CMake/Ninja 构建固定提交的源代码；
原生库使用 Release，开启 SOFA，关闭 CLI、测试和 IAMF。macOS 使用 Accelerate；Windows
需要 MSVC、Boost 头文件及 OpenBLAS/LAPACKE，沿用 MacinRender 的 Windows 工具链。

可设置以下开发覆盖，避免修改锁定版本：

```text
MACINRENDER_SOURCE_DIR=<本地 MacinRender 源码目录>
MACINRENDER_FETCHCONTENT_DIR=<Cargo target 内的依赖缓存目录>
```

Windows 的 `CMAKE_TOOLCHAIN_FILE`、`OPENBLAS_LIBRARY`、`LAPACKE_LIBRARY`、
`OPENBLAS_HEADER_PATH`、`LAPACKE_HEADER_PATH` 会传入 CMake。配置缓存与产物位于 Cargo 构建目录。
没有 `macinrender` 时不需要 C++ 依赖：`cargo run --no-default-features --features decode` 保留
Windows 对象直通以及 macOS/Linux 场景预览。`--no-default-features` 为纯检查构建。

```bash
cargo build --release
python3 scripts/package-player.py
```

打包脚本复制实际 Cargo 构建的库、许可证和构建信息。Mac `.app` 包含运动权限声明及本地签名；
Windows 包包含 MacinRender、OpenBLAS 及所需的 MSVC 运行库，并逐个检查可执行文件和 DLL 的依赖。
macOS 包使用圆角像素图标，Windows 可执行文件嵌入直角方块图标；窗口图标直接编译进程序。
图标源文件和再生成方式见 [应用图标](../assets/icons/README.md)。Windows 构建还需要 SDK 的资源编译器，
MSVC 构建会自动查找 `rc.exe`。
裸 `cargo run` 缺少应用权限声明时，AirPods 采集明确显示不可用并使用手动朝向。软件双耳模式
首次检测到支持设备时可能出现系统运动权限提示。实际三轴方向还应通过 AirPods 真机听验确认。
