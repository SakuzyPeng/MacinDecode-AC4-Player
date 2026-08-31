# Windows 解码契约

## 数据路径

Windows 首版不调用系统媒体解码器。压缩数据只经过锁定提交的 `MacinDecode-AC4-Core`：

```text
MP4/M4A ── macindecode-ac4-mp4 ── bounded raw_ac4_frame ─┐
raw AC-4 ─ macindecode-ac4-bitstream SyncFrameIter ──────┤
                                                          v
                                    Ac4DecoderSession (Full A-JOC)
                                                          │
                         normalized mono planar f32 + OAMD state/ramps
                                                          v
                                    2-second owned Scene FIFO
```

MP4 edit list、priming、composition time 与 presentation shift 在把 AU 交给 Scene Session 前用
Core 的整数时间线 API 换算。裸流按 TOC 的 codec frame length 推进整数采样时间。

活动文件只读取一次，压缩字节由当前 request 持有。初始顺序解码在完成容器/首帧探测后立即开始；
独立的 `ac4-seek-index` worker 同时扫描 AU、绝对 presentation 时间和安全随机访问点，因此首次预缓冲
不等待完整索引。索引完成前 UI 会显示 indexing 状态并禁用 seek，播放本身不受影响。

## FIFO 所有权

`Ac4DecoderSession::decode_access_unit` 返回的视图只在 Session 下一次可变调用前有效。播放器在
worker 内立即复制以下最小语义，然后才允许 Session 继续：

- 配置代次、presentation 下标/ID、采样率、整数起点与长度；
- 稳定 Scene element ID；
- 每个对象一路 normalized mono `f32` PCM；
- 最多一路原生 LFE PCM；
- 帧起点的 active、Cartesian position、linear gain 与 semantic-complete 状态；
- 帧内 metadata update 的 offset、ramp、changed mask 与完整目标状态。

FIFO 按每声道时间帧计量，容量为当前采样率的 2 秒。它保存的是 Scene block，而不是把对象交错
成传统 channel bed，因此后端仍可按准确的 element ID 和 ramp 时间提交动态空间对象。

## 并发与来源切换

每次打开或关闭来源都会递增 request ID；同一文件内的 seek、Stop、设备恢复和拓扑恢复只递增
playback epoch。两者组成 `PlaybackKey`，FIFO、decode event 和 Scene reader 都拒绝旧 key。worker 在
AU 之间以及 FIFO 满载等待时检查控制命令，因此快速连续 seek 或切换播放列表时，旧 PCM 不会进入
当前空间流。

## Seek 契约

- MP4/M4A 索引同时要求 sample table 标记同步样本，且 Core topology 报告
  `RandomAccess::Full`；两者缺一不可。
- 裸 `.ac4` 使用 sync-frame 字节范围和 Core 的 `RandomAccess::Full`。索引同时验证整条流采样率
  不变并计算总时长。
- seek 从目标之前最近的安全 AU 建立 discontinuity Session，解码到目标所在 Scene block；render
  source 以绝对 playhead 在 block 内裁到目标采样。PCM、OAMD 初始状态和 ramp 都按该采样位置对齐。
- 目标之前没有安全点时拒绝 seek，队列和当前 renderer 保持不变。seek 到精确文件结尾会直接进入 EOS。
- Stop 等价于 `seek(0)` 后 Pause，不关闭文件、不重新读盘。

## 当前限制

- 仅 Windows 编译 `audio-decode`；其他平台保持 inspection。
- presentation 使用 `AutoUnique`；多 presentation 文件会得到 Core 的结构化选择错误，尚无 UI 选择器。
- 仅连接 Full A-JOC；Core 已明确拒绝的 channel-based、direct-object 或未实现工具不会静默回退。
- Core 当前 MP4 API 接收完整文件切片，所以压缩源会读入内存；PCM 始终受 2 秒 FIFO 限制。
- 活动文件在磁盘上的外部修改直到重新选择前不可见。
- 裸流中途改变采样率会安全报错，不跨采样率拼接时间线。

原生对象提交与坐标契约见 [WINDOWS_SPATIAL_AUDIO.md](WINDOWS_SPATIAL_AUDIO.md)。
