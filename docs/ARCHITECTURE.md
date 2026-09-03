# 架构草案

## 目标结构

```text
Native GUI (egui/eframe)
        ├── inspection worker ── MacinDecode inspect API
        │
        │ commands / immutable snapshots
Playback coordinator
        │ bounded render quanta
AC-4 decode adapter ───────── Spatial output backend
        │                     ├── Windows Spatial Audio
MacinDecode scene API         └── macOS AU Spatial Mixer
```

GUI、播放协调器和解码适配器均留在 Rust 进程内，直接依赖
`macindecode-ac4-mp4` 与 `macindecode-ac4-scene`，不跨语言暴露 Rust 类型或 Rust ABI。

平台后端只能消费播放器内部定义的窄语义：对象稳定 ID、单声道 normalized `f32` PCM、单路 LFE、
整数采样时间、active、位置、增益和 ramp。后端不得接收 `Ac4SceneFrame`，也不得反向影响解码器
的数据模型。

## 当前边界

本仓库目前实现 GUI、只读 inspection、Windows Core 解码和 Windows Spatial Audio 输出：

- `model` 校验用户选择的媒体路径并保存壳层状态；
- `inspection` 在单独线程调用 `macindecode-ac4-inspect`，缓存 owned report，不阻塞 GUI；
- `decoder` 在 Windows 单独线程组合 `macindecode-ac4-mp4`、
  `macindecode-ac4-bitstream` 与 `macindecode-ac4-scene(audio-decode)`；
- `decoder::windows` 把 Core 的借用 Scene view 复制为播放器自有的对象/LFE PCM、稳定元素 ID、
  起点状态与 ramp 更新，Core 类型不会越过该适配边界；
- 活动压缩文件只读一次并保存在当前 request；初始顺序解码立即开始，独立 worker 并行建立 AU 时间线
  与 Full random-access 索引；
- Scene FIFO 最多保存 2 秒 PCM，300 ms 即可进入 ready。切换来源递增 request ID，seek、重播和
  自动恢复递增 playback epoch；组合 key 使旧 worker 输出无法重新进入当前来源；
- `backend::source` 在任意 Windows render quantum 上消费 Scene FIFO，裁剪负时间和重叠、为空隙补零，
  并在 quantum 起点计算 OAMD ramp 状态；
- `backend::windows` 把播放器语义适配为独立 native crate 的安全接口，不把 COM 类型泄漏到播放器；
- `crates/windows-spatial-audio` 是唯一允许 `unsafe` 的边界，负责 endpoint 枚举/选择、COM 对象流、
  动态对象、静态 LFE、事件等待、source replacement 和原始 `f32` buffer；主程序仍设置
  `unsafe_code = "forbid"`；
- `backend::state` 是 OAMD 状态解析（`element_state_at` 及其 ramp 插值）与瞬跳语义判定。它**不带平台
  门控**：内容只是跨平台解码器类型上的算术，放在 Windows-only 的 `backend::source` 里意味着它的测试
  哪都跑不了——而这恰好是最容易算错、又最不容易被发现的一段；
- `scene_view` 是渲染回调到 UI 的单向镜像，`app` 的 3D 对象场景从这里取数（见下节）；
- `app` 连接上一曲/下一曲、顺序/单曲循环/列表循环/随机播放、Play/Pause、精确 seek、持久化设备
  选择、断开回退/恢复、拓扑自动重建、音量/静音和真实输出诊断。

Core 的 MP4 入口当前要求完整文件切片，因此 decode worker 会先把压缩源读入内存；有界约束针对
解码后的对象 PCM。后续若 Core 增加 seekable source API，可替换容器读取层而不改变 Scene FIFO
或平台后端契约。

## 场景视图镜像

3D 对象场景要显示对象此刻在哪里，而唯一知道这件事的是运行在 WASAPI 事件线程上的 render
callback。`scene_view::SceneViewMirror` 是这条单向通道，`Arc<Mutex<SceneViewFrame>>`。

**所有权**：`backend::SpatialOutputController` 持有唯一的 `Arc`，经 `backend::windows` 的
`spawn` / `replace_source` 注入每一个 `backend::source::SceneRenderSource`，并通过
`scene_view()` 暴露给 `app`。镜像挂在控制器而不是每条 stream 上——设备恢复和兼容 seek 都会新建
render source，共用同一个镜像才不会让视图空一拍。

**写入点**：`SceneRenderSource::render_quantum` 的尾部，直接抄这一帧**已经算好**的
`RenderQuantum` 对象表，不重新解析一遍 OAMD。理由是平行推导会在 ramp 上和音频漂开；而且
`windows_render_state` 产出的坐标（Core/ADM `[x, y, z]` 映射为 `[x, z, -y]`）本来就是 `scene3d`
绘制所用的听众空间，不需要任何转换。

**实时约束**（每一条都是必需的，不是风格问题）：

- 写侧只用 `try_lock`，抢不到就丢弃这次更新。丢一帧画面看不出来，阻塞 WASAPI 回调是真的会响。
- 两侧都不分配。对象数组定长（`MAX_VIEW_OBJECTS`，20），`SceneViewFrame` 是 `Copy`，因此一次
  写入就是一次 memcpy；超出预算的场景被截断并上报，绝不扩容。
- 读侧在锁内**只做一次结构体复制**就放锁，再拿副本去建网格。持锁跨越整个网格组装会让写侧的
  `try_lock` 每帧全部落空，镜像会静默停止更新。帧目前约 11 KiB（20 个槽位各 40 个轨迹点占了绝大
  部分），每个 UI 帧复制一次。往这个结构体上再加定长数组前先看这个数字。
- 镜像写入发生在 Scene FIFO 的 `try_pop` 已返回之后，不同时持有两把锁，也就不会和 decode worker
  互锁。

**抽取率**：当前状态每个 quantum 写一次，轨迹面包屑按固定的**呈现时间**间隔取样
（`TRAIL_INTERVAL_MILLISECONDS`），时钟就是 render source 的 `timeline_frame`——这里唯一单调的
呈现时间源，而且它恰好在 seek 时跳变，也正是轨迹必须丢弃的时刻，两者天然一致。间隔固定是重点：
**点与点的间距就是速度**，按 quantum 取样会把它变成帧率的读数。

**一个没有解析出任何对象状态的 quantum 会被丢弃而不是存成空场景**——前向时间线空隙和 FIFO
抽干都会产生这种 quantum 且相当常见，存成空场景会让每个对象大约每个缓冲闪断一次；保留上一帧
位置既更稳，也更诚实：确实没有更新的位置。

轨迹按槽位存放，但**槽位换了元素就清空**——轨迹属于元素，不属于数组下标，场景的元素集合一变，
同一个下标可能已经换了对象。轨迹不带增益着色：镜像记录的是位置，没有逐点增益历史，用此刻的
增益给过去的位置上色是在断言一件没发生过的事。

场景使用独立于 Core `element_id` 的可见编号：存在 LFE 时，它的柜体六面固定显示 `0`；动态对象不论
是否存在 LFE，都按稳定场景槽位显示 `1..=20`。场景工具条的 `IDs` 按钮可整体开关，默认开启。编号
采用留有接缝的七段码，并作为略高于表面的 3D 几何绘制：旋转和透视自然跟随所在面，背面的编号由
实体深度遮挡，前后对象重叠时也由同一个深度缓冲处理，不需要 UI 另造一套遮挡判断。

**增益落在脚印上**：增益是对象除位置之外携带的第二个量，此前只有两档——`OBJECT_SILENT_GAIN` 之上
给 `ACCENT`、之下向 `STAGE` 褪一档——于是 −35 dB 和 0 dB 画出来完全一样。现在改由每个对象**已经有的
那个落地脚印**承载：边长在 `FOOTPRINT_MIN_SCALE`~`FOOTPRINT_MAX_SCALE` 之间**按分贝**取（线性读法
会把下面整整 30 dB 全挤在地板附近分辨不出），超过单位增益夹住。

- **为什么不是套在方块外面的一圈线框。** 试过，撤了：线框盒**同心套在**实心盒外面是调试碰撞体的
  通用视觉签名，读起来就是「打开了碰撞盒显示」，而且这跟画多淡无关——问题在同心＋盒形这个组合本身，
  不在权重上。脚印则完全不新增记号：它在 decal 层、与地板共面，因此永远不可能穿过方块面或盖住编号；
  「越响，脚下的光池越大」是自然读法，而地板格网本来就是量它的尺子。
- **地板复用 `OBJECT_SILENT_GAIN`，不另立常量。** mockup 表里的 −36 dB 增益地板和它本来就是同一个数。
  共用之后「脚印缩到最小」和「方块褪成静默色」由构造发生在同一个增益上——一个阈值两个通道，日后不
  可能各自漂走；有一条测试专门钉这件事。
- **最小值仍然明显可见**：脚印同时是掠射视角下把空中对象钉在格网上的深度线索，静默对象不能把它弄丢。
- **方块本身保持定尺寸**，不参与增益编码：它是脚印被读数的参照系，而且面上的编号按面尺寸等比缩放，
  方块一变小编号就跟着变小。
- **面包屑的地面投影不按增益缩放**，理由和轨迹不带增益着色的那条相同：镜像记录的是位置，没有逐点
  增益历史，用此刻的增益去缩放过去的落点是在断言一件没发生过的事。
- **正顶视时方块会盖住自己的脚印**：只有脚印宽过方块本身（scale > 1，约 −8 dB 以上）的对象才露得
  出来。这是可以接受的——去 TOP 是为了精读方位角，不是读增益，而两档颜色（可听/静默）在任何角度
  都还在，粗读不丢。掠射视角下脚印同样被压扁，那时垂线接手深度线索。要让更多档在顶视露出来，只需
  抬高 `FOOTPRINT_MIN_SCALE`，代价是地面更满。
- LFE 没有垂线、没有脚印、也没有轨迹——三套系统的缺席就是「无位置」的语义信号。

**瞬跳（`ramp_frames == 0`）**：若仍把跳变前后的轨迹点连起来，「点间距 = 速度」会退化成「极快地
滑过去」，和真的高速运动分不开。所以已发生的跳变段不画连线，两端各一个空心线框方块，起点加一个
指向落点的箭头；轨迹只用线段表达对象实际连续移动过的部分。

判定分两层，不能混：

- **语义**（`backend::state`）：`ramp_frames == 0` 是 OAMD 明说不插值，是关于码流的事实，不是从
  位移猜出来的。
- **标注**（`scene3d::scene`，阈值 `JUMP_MIN_DISTANCE`）：是不是**值得画标记**是感知判断。真实流
  很可能对每次微调都用 `ramp_frames == 0`，无条件标注会把整条路径变成一串空心方块，比原问题更糟；
  而移动了两个百分之一房间宽度的「瞬跳」没人会跟丢。低于阈值的仍按普通线段画。

quantum 约 10 ms 而面包屑 40 ms 一颗，所以 `copy_object_pcm` 查出的 flag 经 `ObjectView.jumped`
交给镜像**锁存**，直到下一颗面包屑取走。锁存逻辑放在 `scene_view` 而不是 Windows-only 的
`source`，因此本机可测；key 变化和槽位换元素时与轨迹一并清空。

**`PlaybackKey` 语义**：每次写入都盖上 render source 从 `SceneQueueReader` 取到的 key，UI 用
`DecoderController::playback_key()` 比对，不匹配就当作没有数据（舞台画一间空房间）。seek、换源和
设备恢复因此自动清场，不需要单独的清空路径——被保留的旧帧仍带着旧 key，同样会被拒。这与 Scene
FIFO 自己的 key 门是同一套语义，视图不可能显示 stream 已经越过的那段播放的位置。key 变化时轨迹和
被显式清空：跨过一次 seek 继续连线，会画出一条对象从未走过的路径。

`output_repaint_delay` 在窗口隐藏时把 `Playing` 的轮询从 16 ms 放到 250 ms——运动只在有画面时才需要
帧率。`logic` 只能知道上一轮有没有运行 `ui`，所以窗口恢复后的第一次 `logic` 仍可能排下 250 ms；正在
运行的 `ui` 会再请求一次 16 ms，让较早的 deadline 覆盖隐藏节奏。**`Ended` + 有播放意图**不看可见性：
曲目交接正是后台最需要保持及时的一条。

非 Windows 平台上没有任何写方，`read` 恒为空，舞台走同一条路径显示空房间。

## 后续里程碑

1. 引入 `MacinDecode-AC4-Core` 的 inspect crate，后台生成只读 bitstream report（已完成）。
2. 接入 Scene/Decode crate，增加 Windows decode worker 与有界 FIFO（已完成首版）。
3. 让 Windows Spatial Audio 后端消费 FIFO，并保持另一后端的能力协商契约（已完成首版）。
4. 增加播放列表切换、连续播放、Play/Pause、欠载诊断与真实文件回归（已完成首版）。
5. 增加 seek、Windows 设备切换与无重开恢复（已完成）。
6. 接入 macOS AU Spatial Mixer。
