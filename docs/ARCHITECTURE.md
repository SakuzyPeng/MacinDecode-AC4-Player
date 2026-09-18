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
MacinDecode scene API         └── MacinRender Scene + device output
                                    ├── SAF VBAP → macOS/Windows system spatial audio
                                    └── SAF binaural → stereo device
```

GUI、播放协调器和解码适配器均留在 Rust 进程内，直接依赖
`macindecode-ac4-mp4` 与 `macindecode-ac4-scene`，不跨语言暴露 Rust 类型或 Rust ABI。

平台后端只能消费播放器内部定义的窄语义：对象稳定 ID、单声道 normalized `f32` PCM、单路 LFE、
整数采样时间、active、位置、增益和 ramp。后端不得接收 `Ac4SceneFrame`，也不得反向影响解码器
的数据模型。

## 当前边界

本仓库实现 GUI、只读 inspection、跨平台 Core 解码、Windows 对象直通，以及可选的 MacinRender
系统空间音频／软件双耳输出。新输出链和固定几何、布局、LFE 默认值见 [MacinRender 集成](MACINRENDER.md)。

`decode` 控制 Core 解码；`windows_spatial_output` 表示 Windows + decode；`macinrender_output`
表示 macOS/Windows + decode + macinrender；`spatial_output` 为两种真实输出的并集。均由
`build.rs` 统一生成并注册 `rustc-check-cfg`。原有 Windows 专用代码使用独立的 Windows 门控，
共享 Scene 算术与无声预览只依赖 decode。

`backend::controller` 管理模式、设备偏好、热更新和头部控制，复用原生对象控制器，或创建
`backend::macinrender` producer。后者是 Scene FIFO 的唯一消费者；元数据历史不保留 PCM，
由输出设备的呈现时钟驱动画面。独立 HRTF 加载线程不阻塞 producer 或状态查询。

- `model` 校验媒体路径；`playlist` 定义稳定的媒体、列表和条目 ID、浏览状态与播放游标；
- `library` 的独立工作线程持有 SQLite 连接，以事务提交列表操作并发布浏览/播放列表的共享快照；
  `preferences` 管理版本化 JSON、原子替换、备份与应用数据目录锁，详见 [播放列表与持久化](PLAYLISTS.md)；
- `inspection` 在单独线程调用 `macindecode-ac4-inspect`，缓存 owned report，不阻塞 GUI；
- `decoder` 在单独线程组合 `macindecode-ac4-mp4`、
  `macindecode-ac4-bitstream` 与 `macindecode-ac4-scene(audio-decode)`；该线程**显式**申请 16 MiB 栈。
  实测一条 20 对象的 L4 A-JOC 流完整解完：release 在 512 KiB 溢出、1 MiB 通过，debug 在 1 MiB 溢出、
  2 MiB 通过——也就是说 `std` 给 spawn 线程的 2 MiB 默认值**扛得住这条流**，只是 debug 下余量不到两倍，
  而 A-JOC 的栈用量随码流里的工具配置变化。Core 自己的测试为同一段重建开 16 MiB，线程栈又只是保留
  地址空间而非提交内存，所以照着它要是这笔交易里便宜的一边。这条以前靠 Windows 链接器的
  `/STACK:8000000` 隐式满足，换个平台就没了，所以要求必须写在代码里；
- `decoder::worker` 把 Core 的借用 Scene view 复制为播放器自有的对象/LFE PCM、稳定元素 ID、
  起点状态与 ramp 更新，Core 类型不会越过该适配边界；同一个 worker 还持有第二条臂
  `decoder::demo`——实时合成的内置演示曲目，喂同一条 FIFO、同一个 key，见[内置演示曲目](#内置演示曲目)；
- `media::MediaSource` 为当前选择延迟打开一次文件，并通过 `OnceLock` 共享文件句柄和 `moov`；
  inspection、index 和 decode 各自使用独立游标与 256 KiB 缓冲，只有实际定位读盘共享短锁。
  文件 I/O 全部在工作线程上执行。换源创建新句柄，seek 与重播沿用原句柄；纯检查构建在报告完成后释放句柄。
  初始顺序解码不等待扫描结束，独立 worker 并行建立 Full random-access 索引；
- Scene FIFO 最多保存 2 秒 PCM，300 ms 即可进入 ready。切换来源递增 request ID，seek、重播和
  自动恢复递增 playback epoch；组合 key 使旧 worker 输出无法重新进入当前来源；
- `backend::source` 在任意 Windows render quantum 上消费 Scene FIFO，裁剪负时间和重叠、为空隙补零，
  并在 quantum 起点计算 OAMD ramp 状态；
- `backend::windows` 把播放器语义适配为独立 native crate 的安全接口，不把 COM 类型泄漏到播放器；
- `crates/windows-spatial-audio` 是 Windows COM 的 `unsafe` 边界，负责 endpoint 枚举/选择、COM 对象流、
  动态对象、静态 LFE、事件等待、source replacement 和原始 `f32` buffer；主程序仍设置
  `unsafe_code = "forbid"`。新增 `crates/macinrender` 独立封装 C ABI、句柄生命周期与原生头部采集；
- `backend::state` 是 OAMD 状态解析（`element_state_at` 及其 ramp 插值）与瞬跳语义判定。它**不带平台
  门控**：内容只是跨平台解码器类型上的算术，放在 Windows-only 的 `backend::source` 里意味着它的测试
  哪都跑不了——而这恰好是最容易算错、又最不容易被发现的一段；
- `scene_view` 是渲染回调到 UI 的单向镜像，`app` 的 3D 对象场景从这里取数（见下节）；
- `app` 连接上一曲/下一曲、顺序/单曲循环/列表循环/随机播放、Play/Pause、精确 seek、持久化设备
  选择与持久化相机视角、断开回退/恢复、拓扑自动重建、音量/静音和真实输出诊断。

输出设备选择和相机视角在 `settings.json` 中保存，并在**读回时**做校验，而不是假定
文件是自己写的。旧 eframe RON 业务键仅作为首次迁移来源。相机不直接 serde
`Camera` 本身，而是过一层 `CameraState` 快照：预设缓动属于会话内状态，不该落盘；读回时每个字段都
要重新过一遍拖拽时同样的限幅。这一层是承重的——非有限的仰角会把 NaN 顺着视图矩阵带进场景里的每
一个顶点，而 `f32::clamp` 对 NaN 是直接放行的。

Core 的 `Ac4Mp4Metadata` 只借用 `moov`，复用完整文件入口的轨道、DSI、sample-description 和时间线校验；
`std` reader 跳过 `mdat`，按描述符读取单个 AU。Annex G 同样使用有界 sync-frame 缓冲，并保留 CRC 检查。
MP4 元数据最多 64 MiB，packet 最多约 16 MiB；正常文件只占用实际元数据与 packet 大小，文件偏移始终为 `u64`。

索引只保存同时满足容器 sync 与 Core Full random access 的起点（裸流只检查后者），最多 8192 点。
达到上限后逐级抽稀，并保留首点、最小呈现时间点和末点；允许从更早安全点预卷，精确目标与失败回退语义不变。
MP4 seek 重走紧凑 sample table 到目标 AU，不保存第二份逐 AU 大表；raw seek 直接定位到 sync header。

文件句柄保持原文件实体，因此改名、路径替换或删除不要求重新打开路径。Windows 允许共享读与删除，拒绝共享写；
每次实际读盘前后检查文件长度和修改时间，检测到就地改写则报错，避免继续拼接变化的数据。整文件不可变字节快照已移除。
检查工作在换源、重试或移除时取消，返回结果带请求 ID，过期结果不能完成新请求；逐帧问题明细上限 1024 条并报告省略数量。

## 场景图形资源

`scene3d::gpu` 为场景自身的物理像素区域分配 4× MSAA 颜色、深度和单采样 resolve 纹理；
不再让 eframe 为整个窗口分配多采样与深度附件。纹理在尺寸不变时复用，尺寸变化时替换，
尺寸计算与 egui callback 的取整、屏幕裁剪一致。场景保持原来的抗锯齿和深度遮挡，GUI 使用
普通单采样 pass，以预乘 alpha 合成场景纹理，场景工具条和菜单仍由 egui 正常覆盖。
MSAA 与深度附件每次清空、pass 结束后丢弃，标为 `TRANSIENT_ATTACHMENT`，允许 Apple GPU 等
支持的平台使用片上临时存储；不受益的平台由 wgpu 按普通附件处理。只有 resolve 纹理跨 pass 保留。

## 场景视图镜像

以下描述 Windows 对象直通路径；MacinRender 路径按设备呈现时间解析同一份元数据语义。

3D 对象场景要显示对象此刻在哪里，而对象直通路径的权威来源是运行在 WASAPI 事件线程上的 render
callback。`scene_view::SceneViewMirror` 是这条单向通道，`Arc<Mutex<SceneViewFrame>>`。

**所有权**：`backend::SpatialOutputController` 持有唯一的 `Arc`，经 `backend::windows` 的
`spawn` / `replace_source` 注入每一个 `backend::source::SceneRenderSource`，并通过
`scene_view()` 暴露给 `app`。镜像挂在控制器而不是每条 stream 上——设备恢复和兼容 seek 都会新建
render source，共用同一个镜像才不会让视图空一拍。

**写入点**：`SceneRenderSource::render_quantum` 的尾部，直接抄这一帧**已经算好**的
`RenderQuantum` 对象表，不重新解析一遍 OAMD。理由是平行推导会在 ramp 上和音频漂开；而且
`backend::state::listener_render_state` 产出的坐标（Core/ADM `[x, y, z]` 映射为 `[x, z, -y]`）本来
就是 `scene3d` 使用的轴系。镜像另外保留内容参照系，绘制时将跟头对象及其轨迹按当前头姿正向旋转到世界坐标。

**实时约束**（每一条都是必需的，不是风格问题）：

- 写侧只用 `try_lock`，抢不到就丢弃这次更新。丢一帧画面看不出来，阻塞 WASAPI 回调是真的会响。
- 两侧都不分配。对象数组定长（`MAX_VIEW_OBJECTS`，20），`SceneViewFrame` 是 `Copy`，因此一次
  写入就是一次 memcpy；超出预算的场景被截断并上报，绝不扩容。
- 读侧在锁内**只做一次结构体复制**就放锁，再拿副本去建网格。持锁跨越整个网格组装会让写侧的
  `try_lock` 每帧全部落空，镜像会静默停止更新。帧目前约 24 KiB（20 个槽位各 40 个轨迹点和
  40 个响度 bin 占了绝大部分），每个 UI 帧复制一次。往这个结构体上再加定长数组前先看这个数字。
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
增益给过去的位置上色是在断言一件没发生过的事。响度 bin 环按同样的理由、在同样的两处清空。

### 对象响度：镜像存能量，UI 侧施加弹道

镜像每个槽位另外保存一圈 **40 × 10 ms = 400 ms** 的 K 加权能量 bin（`LoudnessBin`：平方和、帧数、
样本峰值）。400 ms 正是 ITU-R BS.1770 的 momentary 窗口，所以把整圈求和得到的就是标准定义的那个量
本身，而不是它的近似。

镜像存的是**能量**而不是算好的表头读数，这条是设计的承重墙：

- 三个消费者的发布节奏差几个数量级（Windows 每 quantum、MacinRender 每 2 ms 轮询、预览每 UI 帧且
  封顶 0.25 s）。任何"每次发布推进一步"的衰减器在三条路上表现都不同。
- `write` 在没有对象的更新上保留上一帧，所以存在镜像里的衰减器会**冻住而不是落下**。

因此 bin 的关闭条件是"装满了一个 bin 的音频帧"而不是"时钟到点"，三条路因此落在同一张栅格上；
弹道（10 ms 起、300 ms 落）则由 `app::ObjectMeters` 按墙钟在绘制侧推进。

测量本身在 `backend::state::KWeighting`，和 OAMD 解析同处一个模块、同一个理由：三条路必须逐条一致。
跨消费者的统一规则是：**时间轴空隙按静音计**（计帧、零能量，零样本要真的过滤波器），**欠载完全不计**
（传输故障不是内容），**master volume 不进表**（表读内容电平，不读音量旋钮）。

MacinRender 路径的能量存在 `MetadataFrame` 里、**从 block 偏移 0 起索引**（该结构存的是未裁剪的
`start`/`duration`，发布侧的偏移也按它算），并在设备呈现位置按 bin 取回；一个水位线保证同一个 bin
不会被两次发布重复计入。

**持续静音的对象会淡出**：`app::ObjectMeters` 另外记每个槽位连续低于静音底的秒数，
保持约 2 秒后再用约 0.6 秒淡到零，导出一个 `presence`（0..1）。方块、掉线和 trail 用它向
`STAGE` 再退一层，停在 `SILENT_PRESENCE_FLOOR`。**声音一回来立刻恢复**——消失可以慢，出现不能慢。
编号按同样的比例向各自所在面着色后的底色收敛，与方块共用 `mesh::shade_face`。这样黑号始终比面暗、
白号始终比面亮；若编号直接向背景色淡出，方块侧面后续的明暗处理会让黑号反而比面更亮。

铭牌的不透明度由 `app::plate_alpha` 给出，是**两句话相乘**：电平一掉到静音底就退到
`NAMEPLATE_SILENT_ALPHA`（说"这块牌没有东西可报了"，和读数变成 `−∞` 同一瞬间，不引入新的边界），
`presence` 再决定它退多远——淡出开着就一路到零并跳过整块绘制，关着就停在那档暗色。
所以"关掉淡出"恢复的是**对象**，不是让一块永远 `−∞` 的牌重新占满亮度。

音频设置旁的 **Visual settings** 窗口统一提供编号、响度显示和持续静音淡出三个独立开关。
`AppPreferences::fade_silent_objects` 默认开启，并沿用设置工作线程保存、恢复；旧设置缺少该字段时
使用默认值。淡出和 Silent 计数仅受这个开关控制，不再依赖 `object_loudness`——
`ObjectMeters::drawn_presence` 只收淡出开关，签名里根本没有响度开关。
关闭淡出时传给场景的 `presence` 固定为 1，但测量和静音计时继续推进，重新开启时立即反映当前状态
（`app::turning_the_fade_off_restores_presence_without_stopping_the_silence_clock`）。
三个开关的持久化往返由 `app::library_integration::the_object_visual_switches_survive_a_restart` 钉住。

这个时钟**不在镜子里**：它依赖弹道和墙钟，是显示侧的状态，塞进音频侧构建的 `TrackingSummary`
等于把 UI 计时器推回音频线程。场景上方那条统计条因此多出的"Silent"徽章也在 `app` 侧数，
并且用 `MUTED` 填充而不是 `ACCENT`——前两块按参照系划分、相加等于对象数，这一块是电平状态，
一个静音对象同时还是其中之一，三块同色会被误读成可以相加。

**表带**（`app::draw_meter_bank`）是同一份能量的第二个消费者：场景右侧一条可折叠的栏，每个对象一行，
画电平、把元数据要求的 gain 作为同一标尺上的一条竖线、一个 peak 标记，以及由
`SceneViewFrame::sample_peak` 点亮的 clip。**只有 clip 这一项刻意不过 K 加权**——转换器不是按
听觉模型削顶的。

表格与超出显示数量的提示共同放在受剩余高度约束的纵向滚动区域中，栏头保持固定。
侧栏关闭 egui 的自动分隔线，只使用受侧栏裁剪的 Frame 边框，避免内容高度增长时把竖线画进底部播放控制区。

单位在两者之间切换：场景那份 30 ms 快表 dBFS，和整个 400 ms 环上的 BS.1770 动量响度 LUFS-M。
切换改的是**条和数字一起测的东西**，不是给同一个值换个标签；动量那一档不带弹道，因为加上攻击和
释放之后它就不再是标准定义的那个量，只是长得像。两档共用**同一条静音底** `OBJECT_SILENT_GAIN`，
和铭牌、足迹一致——表带没有自己更长的量程，因为
`app::the_nameplate_strip_and_the_footprint_share_one_decibel_floor` 存在的全部理由就是不让同一个
对象的两处读数互相矛盾。

`ui()` **每帧只读一次镜子**，并在任何面板绘制之前推进 `ObjectMeters`。表带和舞台共用这一帧：读两次
可能拿到不同的发布，推进两次会把同一段墙钟时间向弹道收两遍费。表带的开关和单位都进
`AppPreferences`，开关默认**关**——它是唯一一个要从场景手里拿走宽度的对象可视化。

**地面的 gain 环不淡**。理由不是保守：gain 0 dB 但轨道空的那个对象本身就是持续静音的，
把环一起淡掉，正好把环芯特意要暴露的东西藏掉。
`scene3d::scene` 的 `a_faded_out_object_keeps_its_gain_ring_on_the_floor` 钉住这一条。

**per-object 的响度不是标准量**：BS.1770 是给声道式节目定义的，对象音频的公认做法是渲到参考布局再测。
这里算的东西作为算术是确定的、也是任何对象表都会算的那个，但没有标准把它定义为"对象的响度"——
UI 因此按 dBFS 标注，并由 `backend::state` 的对拍测试（dev-dependency `ebur128`）保证它和参考实现
在 0.01 LU 内一致。

场景使用独立于 Core `element_id` 的可见编号：存在 LFE 时，它的柜体六面固定显示 `0`；动态对象不论
是否存在 LFE，都按稳定场景槽位显示 `1..=20`。**Visual settings** 中的 **Element numbers (IDs)** 可整体开关，默认开启。编号
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
- **正交顶视会盖住窄于方块的脚印，透视顶视不会。** 正交下垂直向下看，方块和它正下方的脚印投影到
  同一个位置，于是 scale < 1 的脚印被精确盖住——这是投影的性质，不是这套编码的局限。透视下两者深度
  不同，产生视差位移（离视轴越远偏得越多），脚印重新露出来，静默对象也不例外；工具条上的
  ORTHO/PERSP 一键即可切换。所以安静那一端可以继续留小，地面保持干净，不必为了顶视抬高
  `FOOTPRINT_MIN_SCALE`。掠射视角下脚印仍会被压扁，那时垂线接手深度线索。
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

### 无回放平台上的场景预览

镜像的写入点是 WASAPI 渲染回调，所以没有回放就没有推动它的时钟——即使解码在跑，舞台也只会是一间
空房间，FIFO 填到两秒上限后解码本身也停住。`backend::preview::ScenePreview` 是补上的那个时钟。

它按独立的 `Instant` 单调时钟走同一条 Scene FIFO；隐藏窗口只执行 `logic` 时，egui 的 UI 时间输入
不会更新，因此不能使用 `stable_dt`。暂停会解除计时，恢复和重新配置时从当前时刻重新开始，避免把
暂停或 seek 等待时间算进预览。它用 render source 用的**同一个**
`backend::state::element_state_at` 解析 OAMD、同一个 `listener_render_state` 换算坐标，然后写同一个
镜像。共用这两个函数是重点：画面不是一份平行推导，而是"如果这台机器能放音，此刻会提交给渲染器的
那一组对象"。它不提交 PCM——没有地方送。

两条消费路径还共用 `backend::state::validate_block` 和 `block_offset_at`：过期块直接跳过，跨越
零点或 seek 目标的首块从正确偏移开始。预览也检查完整 Scene signature 和采样率；不兼容时停在边界，
保留失败状态和帧位置，拓扑错误由协调器按原有 epoch/seek 流程重建，避免继续显示已经被替换的对象。
由于预览推进发生在协调器的错误处理之后，新出现的预览错误会请求一次后续逻辑运行；即使解码已到
EOS、不再轮询 decoder，也能进入恢复流程。已经锁定的同一错误不会反复请求重绘。
预览向镜像传递全部对象，由镜像执行 20 对象的有界复制并记录超额数量，所以遗漏提示不会被提前截断。

三条约束：

- **构造条件是 `feature = "decode"` 且没有渲染器**（`backend.rs` 的 `ensure_configured` 里，原先
  `drop(reader)` 的那个分支）。一条 FIFO 永远只有一个消费者，预览和 render source 不可能同时在 pop。
- **每次写入盖的仍是 reader 的 `PlaybackKey`**，所以 seek/换源的清场逻辑一字不改地适用。
- **时间累进带亚帧余数**，并把单次 tick 截到 0.25 秒。48 kHz 下 16 ms 恰好是 768 帧，但帧时间不保证
  整除，每 tick 丢掉余数会让预览可测量地走慢；截断则是防止窗口被拖住几秒后一次性冲掉整个缓冲。

`OutputSnapshot::is_preview()` 让 UI 说实话：设备标签是 `Scene preview · no audio output`，状态行也
换成对应措辞。相位仍然走 Ready/Playing/Paused/Ended/Failed，因为传输控件、时间轴和重绘节奏本来就该
一视同仁。清空预览同时重置输出快照并更新 revision，删除文件后不会残留可播放状态。

Windows 上不构造预览：那里 render source 拥有 FIFO，多一个消费者就是在和音频抢数据。

## 内置演示曲目

仓库不携带任何音频，Core 也只解不编，所以"内置一段能播的东西"只能是实时合成的。`decoder::demo`
是 Scene FIFO 的**第二个生产者**：它从解码器下游接入，因此两条渲染路径、无声预览、场景视图和表带
看到的演示曲目与看到一次解码完全一样——没有一个消费者为它改过一行。

**它不是 AC-4，也不假装是。** 合成从 Core 之后插入，所以它验证不了解码器的任何一行。
`DecodeContainer::Generated` 的标签就是 `built-in demo`，状态行照实显示。它与文件共用同一个
`PlaybackKey`、同一条 FIFO、同一套相位和 EOS；`PlaybackSource` 在控制器一层区分两者，
`media::MediaSource` 保持纯文件语义——演示曲目没有路径，给它编一个就等于让库、文件目录和重定位
对话框各自把它当成一个失踪的文件。

Demo 有独立的播放模式，默认单曲循环，也可只播一次，不借用或修改浏览中的播放列表模式。
EOS 后沿用正常的 seek-to-zero 重播流程，推进 playback epoch；精确定位到末尾是合法 EOS，
仍保留 Scene signature 供输出配置和重播。暂停、重播、重试按活动源判断，不要求库光标存在。
文件会话暂存在 checkpoint 中，Demo 的时钟不能覆盖它；浏览和库编辑仍更新这个暂停的会话。
停止 Demo 恢复原光标和断点并保持暂停，显式播放文件则清除 Demo 状态。

### 为什么是卡农

曲目是帕赫贝尔的 D 大调卡农（Canon per 3 Violini e Basso，1694，公有领域），选它是为了**可纠错**
而不是好听。原创曲里渲染器的错误——丢了一个声部、提交晚了、位置镜像了——和"这段本来就长这样"
无法区分；而在人人熟悉的卡农里，第二声部奏的是第一声部八拍前刚奏过的东西，听者手里一直攥着正确
答案。曲式本身承担了两件事：

- **它是卡农**，所以旋律线只存一份，三把小提琴是同一条线的三个进入偏移。声部之间不可能走样，
  因为没有可走样的对象；把三者放上同一条轨道、角度差正比于时间差，卡农就同时变成看得见的。
- **它建立在固定低音上**，两小节重复 28 次。低音全程扛着和声，所以淡出、静音或停用其上任意子集
  仍然正确——这正是"演示可以换着展示不同东西而音乐不出错"的前提。那个两小节周期同时也是幕的单位。

乐谱由 `scripts/generate-demo-score.py` 从 Mutopia 的 LilyPond 源机械转写并提交，构建与测试都不
联网。生成期校验每一小节的四拍、以及 28 次固定低音是否逐音重复；Rust 侧再独立重算对象预算并要求
与生成器一致。一个覆盖每个音符每个字段的 FNV-1a 摘要负责拦住音级集合和音域都拦不住的单个错音——
转写一首人人熟悉的曲子，代价就是错一个音会非常刺眼，包括手抄时记错的那个。

### 模态合成，以及为什么不是延迟线

音色是若干衰减正弦之和，`s(t) = A(t) · Σₖ aₖ·e^(−t/τₖ)·sin(2π·f·rₖ·t)`，`t` 自音符起振算起。
任意 `t` 可直接求值，于是 `DemoProgram::block_at` 是其起始帧的纯函数——**seek 只是换一个参数**，
没有状态要回卷、没有尾巴要重算、没有索引要建立。Karplus-Strong 至少同样好听，但它是递归的，
seek 之后必须把每个仍在响的音从起点重算一遍才能得到同一串样本。敲击与拨弦的瞬态也远比持续音
好定位：pad 在哪儿都糊，槌击在某一处——这正是空间演示需要的。

块内每个分音用一次旋转加一次乘法推进，种子从该块自身的起点精确求出，所以上面那条性质分毫未动；
换来的是大约一个数量级，debug 构建合成整首 190 秒只要 6.3 秒，三十倍实时。

一个对象承载一条旋律线时，每个音在下一个音到来时被阻尼——拨弦 5.8 秒的尾巴配上十六分音符会让
同一个对象上二十多个音同时在响，没有哪件真实乐器这样。池槽持有单个音时则完整响完。这个差别
就是"一个乐器一个对象"与"一个正在响的音一个对象"的分野。

每个音符先单独施加起振和阻尼包络，再叠加到对象 PCM；新音的窗口不能修改已有尾音。
元数据同样从绝对时间求值：块首先计算正在进行的增益渐变，并以 gain-only 更新携带剩余时长，
所以跨块或 seek 到渐变中间都不会跳到目标增益。轨迹更新只改位置，遇到新音或释放时结束旧轨迹。

### 十六个槽，以及幕

十六个对象槽从第一块存在到最后一块，元素 ID 从不改变，所以 `SceneSignature` 全程恒定，任何幕的
边界都不会触发流重建——这是"时分复用"相对"中途扩容"的根本区别，后者会改签名并强制整条流重建。
六个槽承载音乐（三把小提琴、固定低音、两路通奏），其余十个是各幕支配的池；一音一对象的那一幕
另外收回三个小提琴槽，因为它们的声部已经把音符交给池了。

幕的边界落在固定低音回到首音的那一拍，听上去是乐句换气而不是技术性切换。分幕的理由是对象预算
稀缺而演示项并不同时发生：与其为整首曲子可能用到的一切各占一个对象，不如在时间上复用固定的一组
元素 ID。需要**同时**对比的演示（参考系 AB、满增益的静音对象）省不掉，必须并行占槽。

池槽交接前必须让上一个音衰到 −20 dB（`ln(10)·τ`，截断尾巴不再可闻的点）。生成器按这个保护期测量
每一幕的并发峰值并拒绝会超额的幕表，Rust 测试独立重算并要求一致。池满时抢占最老的槽并计数，一条
测试断言计数恒为零——于是十六这个上限由构造保证，而不是依赖算术一直成立。

十六个槽从第 0 块起全部声明，所以 Windows 端点必须报出 `max_dynamic_objects ≥ 16` 才配得起来。
这不是额外的门槛，而是对象直通本来的下限：Windows 10 的 Dolby Atmos 耳机与内置扬声器路径就提供
十六个，要二十个的是 AC-4 L4（`README.md` 的"Windows 空间音频的前提"引了 Microsoft 的资源限制
文档）。凡是能播 L3 的端点都配得起这段演示，所以没有另一套更小的幕表——那要维护第二份并发证明，
而它要服务的端点并不存在。

端点真的报不出十六时，失败是看得见的而不是神秘的：设备下拉框标出每个端点的槽位数、把不够的灰掉
并在悬停时说明差多少，激活失败的消息同时给出端点提供的数量与场景要求的数量。

### 测试上的副作用

演示曲目不需要任何媒体，所以 worker 的命令路径、FIFO 背压、key 校验、seek 与 EOS 第一次有了普通
`cargo test`——在此之前它们全部躺在 `MACINDECODE_AC4_TEST_MEDIA` 之后的 ignored 测试里。
`backend::preview` 也第一次有了用**真实生产者**产出的 Scene 驱动的测试，而不是为断言量身定做的
fixture。

## 后续里程碑

1. 引入 `MacinDecode-AC4-Core` 的 inspect crate，后台生成只读 bitstream report（已完成）。
2. 接入 Scene/Decode crate，增加 Windows decode worker 与有界 FIFO（已完成首版）。
3. 让 Windows Spatial Audio 后端消费 FIFO，并保持另一后端的能力协商契约（已完成首版）。
4. 增加播放列表切换、连续播放、Play/Pause、欠载诊断与真实文件回归（已完成首版）。
5. 增加 seek、Windows 设备切换与无重开恢复（已完成）。
6. 接入 MacinRender SAF 渲染、系统空间输出、软件双耳与头部控制（已实现）。
7. 扩展到实时 Apple AUSpatialMixer、EAR／HOA 后端，以及物理多声道设备输出。

### 跨帧元数据续接

`decoder::metadata` 只操作 Player 自有状态。每次连续解码维护逐元素当前值、目标和独立的位置/增益期限，
在帧首生成剩余渐变事件，再应用该帧的 offset 0 控制；seek 重试、新 epoch、generation 或时间缺口清除历史。
续接在目标帧裁剪和 FIFO 发布之前完成，预卷中的控制也能正确影响目标位置。三个 Scene 消费者共用同一
解析器，不持有 Core 类型或第二份 PCM；跟踪策略及诊断仍在实际呈现时刻可见。

### 听者皮肤

`scene3d::skin` 解码标准 64×64 和旧版 64×32 PNG。现代格式检查双臂底面与侧面的
透明留白以推断 Steve / Alex，手动体型覆盖按皮肤保存；旧格式固定使用 Steve 并镜像左肢。
基础层按 Minecraft 语义视为不透明，外层使用原 alpha。加载时把相邻同色像素合并成面片，
每帧仅变换坐标、着色并提交，头部与衣帽复用 `Figure` 的颈部枢轴和三轴旋转。
不透明面片复用场景深度缓冲；半透明面片按当前视线从远到近绘制、混合且不写深度，
衣帽内外两侧均可见。皮肤尺寸限制使面片数量和解码内存保持有界。

`skin_library` 在工作线程校验图片并原子复制到 `skins/`，以内容 SHA-256 去重。
`AppPreferences::skins` 保存显示名称、选择和体型覆盖，UI 在成功加载后才切换；
损坏或缺失的导入会显示错误并保留当前角色。`app::visual_settings` 集中提供皮肤和音频对象显示选项。
