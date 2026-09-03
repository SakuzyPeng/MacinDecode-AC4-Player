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
- `backend::state` 是 OAMD 状态解析（`element_state_at` 及其 ramp 插值）与未来路径采样器。它**不带
  平台门控**：内容只是跨平台解码器类型上的算术，放在 Windows-only 的 `backend::source` 里意味着它的
  测试哪都跑不了——而这恰好是最容易算错、又最不容易被发现的一段；
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
  `try_lock` 每帧全部落空，镜像会静默停止更新。帧目前约 22 KB（轨迹 40 点、未来路径 48 点各
  20 个槽位占了绝大部分），每个 UI 帧复制一次。往这个结构体上再加定长数组前先看这个数字。
- **锁序单向：镜像锁与 Scene FIFO 锁永不同时持有。** 当前状态的写入点在 `try_pop` 早已返回之后；
  未来路径确实在 FIFO 锁内采样，但那一段跑完、锁释放之后才 `try_lock` 镜像。两把锁没有交叠，
  也就没有和 decode worker 互锁的可能。

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

**未来路径**：FIFO 里躺着已解码、尚未播放的位置，把它们画出来，`BUFFER` 这个数字就变成空间里的
几何量——缓冲越浅，物体前方的虚线越短。取数走 `SharedSceneQueue::with_queued_blocks` 这个**非破坏性
peek**：key 不匹配直接返回，闭包在队列锁内运行且只读，**不动 `buffered_frames`、不 notify condvar**，
所以 decode worker 永远不会把一次 peek 误当成腾出了空间。闭包里不许分配、不许阻塞——它跑在渲染
回调上。

采样从 `SceneRenderSource` 正在播的那个 block 的**剩余部分**开始（不需要锁），再进队列，否则线头会
差最多一个 block。位置一律经 `backend::state::element_state_at` 解析——和音频用的是同一个函数，
这是正确性要求而不是省事：平行推导会在每一条 ramp 上和听到的东西漂开，而且漂了也看不出来。槽位用
`SceneSignature::object_element_ids()`（已排序）二分查出，与镜像按 element_id 升序排的槽位一致。

时钟只有一个：采样间隔和重算频率都用 `TRAIL_INTERVAL_MILLISECONDS`（40 ms），`FUTURE_SAMPLES` = 48
即 1.92 s，刚好压在 `MAX_BUFFER_SECONDS` 之下。于是过去和未来是同一把尺子的两个方向，而重算最多
只落后一个采样间隔——正是这条路径自身的分辨率，看不出错位。每个 quantum 都重走一遍 FIFO 纯属浪费。

某个对象在某一时刻没有位置，就在那里**结束它自己**的路径，其余对象继续；已结束的槽位不会被后面的
block 重新打开，否则会画出一段对象根本不会走的连线。前向空隙里没有任何元素状态，此时采样网格
**重新对齐到下一个有数据的 block**，而不是让所有路径都在第一个空隙处终结——代价是一段偏短的虚线。

`write_future` 只往 key 相同的帧上写：路径只有和它延续的那组位置放在一起才有意义，而那组位置是
`write` 建立的。

**瞬跳（`ramp_frames == 0`）**：不标出来的话，未来路径会在跳变两端画一条穿过房间的直线，断言物体
经过了那条线上的每一点——而它一个中间点都没到过；轨迹那边「点间距 = 速度」也会退化成「极快地滑
过去」，和真的高速运动分不开。所以跳变段不画连线，两端各一个空心线框方块，起点加一个指向落点的
箭头。这反而让规则更严格：**虚线段长正比于一个采样间隔内连续走过的距离**，而瞬跳连续走过的距离
是零，于是画出零长度。

判定分两层，不能混：

- **语义**（`backend::state`）：`ramp_frames == 0` 是 OAMD 明说不插值，是关于码流的事实，不是从
  位移猜出来的。
- **标注**（`scene3d::scene`，阈值 `JUMP_MIN_DISTANCE`）：是不是**值得画标记**是感知判断。真实流
  很可能对每次微调都用 `ramp_frames == 0`，无条件标注会把整条路径变成一串空心方块，比原问题更糟；
  而移动了两个百分之一房间宽度的「瞬跳」没人会跟丢。低于阈值的仍按普通线段画。

两个时态各有一处必须小心：

- **未来路径**：跳变的归属跨 block 留存在 `FuturePath` 里。block 约 42 ms 而采样间隔 40 ms，存在
  一个采样点都没落进去的 block，不留存就会整个漏掉。判断某个 block 是否「已经过去」用的是**上一个
  采样点**而不是下一个——落在两个采样点之间的 block 不是过去，它携带的跳变属于后一个采样点。
- **轨迹**：quantum 约 10 ms 而面包屑 40 ms 一颗，所以 `copy_object_pcm` 查出的 flag 经
  `ObjectView.jumped` 交给镜像**锁存**，直到下一颗面包屑取走。锁存逻辑放在 `scene_view` 而不是
  Windows-only 的 `source`，因此本机可测；key 变化和槽位换元素时与轨迹一并清空。

**`PlaybackKey` 语义**：每次写入都盖上 render source 从 `SceneQueueReader` 取到的 key，UI 用
`DecoderController::playback_key()` 比对，不匹配就当作没有数据（舞台画一间空房间）。seek、换源和
设备恢复因此自动清场，不需要单独的清空路径——被保留的旧帧仍带着旧 key，同样会被拒。这与 Scene
FIFO 自己的 key 门是同一套语义，视图不可能显示 stream 已经越过的那段播放的位置。key 变化时轨迹和
未来路径都被显式清空：跨过一次 seek 继续连线，会画出一条对象从未走过的路径。

**没人看的时候停掉 FIFO 走查**：窗口最小化或隐藏时 eframe 完全不跑 egui pass，于是每帧的网格重建和
GPU 提交自动全免——但音频线程上的 `publish_future_path` 不会自动停，它每 40 ms 走一遍整个 FIFO
（约 47 个 block × 20 个对象 × 每 block 的 metadata 更新），是这个模块向 WASAPI 线程索取的最大一笔
开销，而且此时全是白费。所以镜像上挂一个 `observed: AtomicBool`：UI 在 `logic` 里按「上一次之后
`ui` 有没有跑过」置位，render callback 读它决定走不走。

- 两侧都用 `Ordering::Relaxed`——这是提示，早一个 quantum 或晚一个 quantum 都不损失什么。
- **默认 true**：没人说过话的镜像按「有人看」处理，反过来会让已经在看的人得到一张空图。
- 停下来时**发布一次空路径**再停，否则窗口切回来会先闪一条描述几分钟前缓冲状态的线。恢复不等节流：
  时间线早已越过下一个到期帧，所以路径在窗口回来后的第一个 quantum 就重新出现。
- **每 quantum 的 `write`（轨迹）照写不误。** 它只有 640 字节 memcpy，省不出什么；而停掉它会让轨迹
  在后台留一个洞，恢复时要么把洞两端连起来撒谎、要么整条丢掉。未来路径没有这个问题：它完全是当前
  FIFO 的函数，不跨 tick 累积，停了再算即可。

判定用「`ui` 跑过没有」而不是读 `ViewportInfo`：真正决定跑不跑 pass 的是 eframe 自己的 `show_ui`
（`is_visible || 后代可见`），照着 `ViewportInfo` 再推一遍等于把它的规则抄第二份，它一改就漂。

同理，`output_repaint_delay` 在隐藏时把 `Playing` 从 16 ms 放到 250 ms——运动只在有帧的时候才需要
帧率。**`Ended` + 有播放意图那条不看可见性**：曲目交接正是后台最需要保持及时的一条。

非 Windows 平台上没有任何写方，`read` 恒为空，舞台走同一条路径显示空房间。

## 后续里程碑

1. 引入 `MacinDecode-AC4-Core` 的 inspect crate，后台生成只读 bitstream report（已完成）。
2. 接入 Scene/Decode crate，增加 Windows decode worker 与有界 FIFO（已完成首版）。
3. 让 Windows Spatial Audio 后端消费 FIFO，并保持另一后端的能力协商契约（已完成首版）。
4. 增加播放列表切换、连续播放、Play/Pause、欠载诊断与真实文件回归（已完成首版）。
5. 增加 seek、Windows 设备切换与无重开恢复（已完成）。
6. 接入 macOS AU Spatial Mixer。
