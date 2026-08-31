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

每次打开或关闭来源都会递增 request ID，并先清空 FIFO。worker 在 AU 之间以及 FIFO 满载等待时
检查控制命令；旧 request 的 block 会被队列拒绝。这样即使用户快速切换播放列表，上一文件的 PCM
也不会进入新文件的空间流。

## 当前限制

- 仅 Windows 编译 `audio-decode`；其他平台保持 inspection。
- presentation 使用 `AutoUnique`；多 presentation 文件会得到 Core 的结构化选择错误，尚无 UI 选择器。
- 仅连接 Full A-JOC；Core 已明确拒绝的 channel-based、direct-object 或未实现工具不会静默回退。
- Core 当前 MP4 API 接收完整文件切片，所以压缩源会读入内存；PCM 始终受 2 秒 FIFO 限制。
- 尚无 FIFO consumer、设备流或 seek，因而 transport 控件保持禁用。
