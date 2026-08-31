# Windows Spatial Audio 输出契约

## 数据路径

```text
owned Scene FIFO
      │ stable element ID + mono planar f32 + OAMD state/ramp + optional LFE
      v
SceneRenderSource ── render quantum adapter ── macindecode-windows-spatial-audio
                                                   │
                                                   v
                                      default ISpatialAudioClient endpoint
                                      dynamic objects + static LFE object
```

播放器不调用 Windows 系统解码器。`MacinDecode-AC4-Core` 产生的 Scene block 才能进入输出层；
native crate 看不到 AC-4 bitstream、Core Session 或 Core 的借用类型。

## 流激活与所有权

- render worker 在 MTA 初始化 COM，打开默认 `eRender` / `eConsole` endpoint；当前没有设备选择 UI。
- 对象格式固定为 1 声道、32-bit IEEE float、Scene 原始采样率。
- 激活前检查 `GetMaxDynamicObjectCount`。动态对象最小值和最大值都设为 Scene 对象数；有 LFE 时
  额外申请静态 `AudioObjectType_LowFrequency`。
- `SpatialAudioObjectRenderStreamActivationParams` 复制到 `CoTaskMemAlloc` 所有的 `VT_BLOB`。
  `windows` crate 会在 `PROPVARIANT` 析构时调用 `PropVariantClear`，因此不得让 BLOB 指向 Rust allocator
  或栈内存。
- Scene 对象、stream、client、endpoint、event 与 activation backing 的析构顺序受 native crate 控制；
  Stop/Reset 后先释放对象和 stream，再关闭 event 与 COM apartment。

主程序设置 `unsafe_code = "forbid"`。COM vtable 调用、原始指针和 Windows 对象 buffer 只存在于
`crates/windows-spatial-audio`。

## Quantum 适配

- Windows 每次 `BeginUpdatingAudioObjects` 给出的 frame count 可以跨越多个 Scene block；adapter 会连续
  读取并填满本次 quantum。
- MP4 时间线早于 0 的部分被裁掉；重叠 block 从当前播放位置裁剪；正向空隙补静音。
- Core/ADM Cartesian `[x, y, z]` 转换为 Windows listener coordinates `[x, z, -y]`，每轴限制在
  `[-1, 1]`。
- 每个 element ID 只激活一个动态对象并保持到流结束。每个 update 都重新提交位置与音量；inactive、
  坐标不完整或语义不完整的对象以零增益提交。
- OAMD ramp 在每个 Windows quantum 起点插值。Windows 一个对象在单个 quantum 只接受一组位置/音量，
  因而 quantum 内的 metadata update 会量化到后续 quantum 边界。
- LFE 使用独立静态对象，不参与动态对象槽位计数。

## 控制与结束

- native stream 配置完成后保持运行并在非播放状态提交静音。Play 才消费 Scene FIFO；Pause 停止消费
  但保留流；主音量和静音通过每对象 volume 生效。
- FIFO 的 request ID 防止旧来源进入新流。来源切换会销毁旧 renderer；旧 reader 将其 request 视为 EOS。
- producer 标记 EOS 且 FIFO 排空后，对所有已激活对象调用 `SetEndOfStream`。Stop 会销毁流、清空 FIFO，
  并让 Core 从文件开头重新预缓冲。
- 当前没有 seek、上一曲或非默认设备选择。

## 本地回归

真实媒体不进入版本库。把文件放在根目录已忽略的 `.local-test-media/`，设置
`MACINDECODE_AC4_TEST_MEDIA` 后运行：

```bat
cargo test backend::windows::tests::submits_decoded_scene_to_windows_spatial_audio -- --ignored
```

该测试要求默认 endpoint 支持 Spatial Audio，并检查对象槽位、至少 20 次 render update、PCM/object
buffer、位置提交、Pause 和零欠载。普通 `cargo test` 不依赖本地媒体或音频设备。
