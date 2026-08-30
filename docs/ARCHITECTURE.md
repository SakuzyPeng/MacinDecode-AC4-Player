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

本仓库目前实现 GUI shell 与只读 inspection：

- `model` 校验用户选择的媒体路径并保存壳层状态；
- `inspection` 在单独线程调用 `macindecode-ac4-inspect`，缓存 owned report，不阻塞 GUI；
- `backend` 描述计划中的平台后端，但不实例化设备；
- `app` 绘制播放列表、bitstream 摘要与详情，不启动解码或播放线程；
- transport 控件全部禁用，避免产生已经能够播放的错觉。

## 后续里程碑

1. 引入 `MacinDecode-AC4-Core` 的 inspect crate，后台生成只读 bitstream report（已完成）。
2. 接入 Scene/Decode crate，增加流式 decode worker 与有界 FIFO。
3. 实现首个平台后端并保持另一后端的能力协商契约。
4. 增加 play/pause/seek、设备切换、欠载诊断与真实文件回归。
