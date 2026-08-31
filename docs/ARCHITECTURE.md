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

GUI、播放协调器和解码适配器均留在 Rust 进程内，未来直接依赖
`macindecode-ac4-mp4` 与 `macindecode-ac4-scene`，不跨语言暴露 Rust 类型或 Rust ABI。

平台后端只能消费播放器内部定义的窄语义：对象稳定 ID、单声道 normalized `f32` PCM、单路 LFE、
整数采样时间、active、位置、增益和 ramp。后端不得接收 `Ac4SceneFrame`，也不得反向影响解码器
的数据模型。

## 当前边界

本仓库目前实现 GUI、只读 inspection 和 Windows Core 解码入口：

- `model` 校验用户选择的媒体路径并保存壳层状态；
- `inspection` 在单独线程调用 `macindecode-ac4-inspect`，缓存 owned report，不阻塞 GUI；
- `decoder` 在 Windows 单独线程组合 `macindecode-ac4-mp4`、
  `macindecode-ac4-bitstream` 与 `macindecode-ac4-scene(audio-decode)`；
- `decoder::windows` 把 Core 的借用 Scene view 复制为播放器自有的对象/LFE PCM、稳定元素 ID、
  起点状态与 ramp 更新，Core 类型不会越过该适配边界；
- Scene FIFO 最多保存 2 秒 PCM，300 ms 即可进入 ready。切换来源会递增 request generation，
  清空旧 FIFO，并使旧 worker 输出无法重新进入新来源；
- `backend` 描述计划中的平台后端，但不实例化设备；
- `app` 绘制播放列表、bitstream 详情和真实解码诊断，但不消费 Scene FIFO；
- transport 控件全部禁用，避免产生已经能够播放的错觉。

Core 的 MP4 入口当前要求完整文件切片，因此 decode worker 会先把压缩源读入内存；有界约束针对
解码后的对象 PCM。后续若 Core 增加 seekable source API，可替换容器读取层而不改变 Scene FIFO
或平台后端契约。

## 后续里程碑

1. 引入 `MacinDecode-AC4-Core` 的 inspect crate，后台生成只读 bitstream report（已完成）。
2. 接入 Scene/Decode crate，增加 Windows decode worker 与有界 FIFO（已完成首版）。
3. 让 Windows Spatial Audio 后端消费 FIFO，并保持另一后端的能力协商契约。
4. 增加 play/pause/seek、设备切换、欠载诊断与真实文件回归。
