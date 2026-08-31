# Windows Spatial Audio 输出契约

## 数据路径

```text
owned Scene FIFO
      │ stable element ID + mono planar f32 + OAMD state/ramp + optional LFE
      v
SceneRenderSource ── render quantum adapter ── macindecode-windows-spatial-audio
                                                   │
                                                   v
                                      selected ISpatialAudioClient endpoint
                                      dynamic objects + static LFE object
```

播放器不调用 Windows 系统解码器。`MacinDecode-AC4-Core` 产生的 Scene block 才能进入输出层；
native crate 看不到 AC-4 bitstream、Core Session 或 Core 的借用类型。

## 流激活与所有权

- 独立设备目录 worker 在 MTA 枚举活动 `eRender` endpoint，读取稳定 endpoint ID、友好名称、系统默认
  标记，并通过 `GetMaxDynamicObjectCount` 探测 Spatial Audio 容量；目录约每 2 秒刷新。
- UI 可选择“系统默认”或明确 endpoint ID。所选 ID 由 eframe 持久化；断开时临时回退到兼容的系统
  默认设备，恢复后自动回切。选择“系统默认”时会跟随默认 endpoint 的变化。
- 对象格式固定为 1 声道、32-bit IEEE float、Scene 原始采样率。
- 激活前检查 `GetMaxDynamicObjectCount`。动态对象最小值和最大值都设为 Scene 对象数；有 LFE 时
  额外申请静态 `AudioObjectType_LowFrequency`。
- `SpatialAudioObjectRenderStreamActivationParams` 复制到 `CoTaskMemAlloc` 所有的 `VT_BLOB`。
  `windows` crate 会在 `PROPVARIANT` 析构时调用 `PropVariantClear`，因此不得让 BLOB 指向 Rust allocator
  或栈内存。
- Scene 对象、stream、client、endpoint、event 与 activation backing 的析构顺序受 native crate 控制；
  Reset 后先释放对象和 stream，再关闭 event 与 COM apartment。

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
- LFE 使用独立静态对象，不参与动态对象槽位计数；它和动态对象一样遵循 OAMD active、语义完整性、
  linear gain 与 ramp，最终再乘主音量。没有有效状态或 inactive 时以零增益提交。
- 首个 block 会锁定完整 Scene 签名：采样率、configuration generation、presentation、动态对象
  element ID 集合和 LFE element ID。播放中发生变化时 adapter 报告可恢复边界，协调器在该绝对位置
  自动重建流，避免把 Core 新分配的 element ID 绑定到旧 Windows 对象。

## 控制与结束

- native stream 配置完成后保持运行并在非播放状态提交静音。Play 才消费 Scene FIFO；Pause 停止消费
  但保留流；主音量和静音通过每对象 volume 生效。
- 同一 request 暂时回到 Buffering 时保留已经激活的 native stream；FIFO 恢复供数后继续播放，不因
  一次欠载销毁 renderer。
- 同一设备且完整 Scene 签名兼容时，seek 在 render quantum 边界用 `ReplaceSource` 替换 reader，
  重设绝对 `playhead_frames`，复用现有 Spatial Audio stream。播放/暂停状态和主音量保持不变。
- 设备、采样率、对象/LFE 或 Scene 签名不兼容时，协调器内部重建原生流并从保留的绝对位置恢复；
  用户无需重新选择或重新打开文件。
- FIFO 的 request ID + playback epoch 防止旧来源或旧 seek 进入新流。切换播放列表文件才会销毁当前
  文件状态；旧 reader 会把失效 key 视为 EOS。
- producer 标记 EOS 且 FIFO 排空后，对所有已激活对象调用 `SetEndOfStream`；完成最后一次
  `EndUpdatingAudioObjects` 后立即释放这些对象接口，同时保留已结束的 stream，以免后续 update 复用
  已失效对象并进入 Failed。播放器按顺序、单曲循环、列表循环或随机模式决定重播当前来源、切换
  来源或结束；同一来源重播使用压缩缓存 seek 到 0，签名兼容时直接替换 source。
- 若首选 endpoint 消失会临时使用兼容默认设备；没有任何活动 endpoint 能容纳当前 Scene 时，播放器
  保存文件、绝对位置、音量和播放意图，暂停等待设备恢复。

## 本地回归

真实媒体不进入版本库。把文件放在根目录已忽略的 `.local-test-media/`，设置
`MACINDECODE_AC4_TEST_MEDIA` 后运行：

```bat
cargo test backend::windows::tests::submits_decoded_scene_to_windows_spatial_audio -- --ignored
cargo test -p macindecode-windows-spatial-audio ended_renderer_releases_objects_without_entering_failed_state -- --ignored
cargo test -p macindecode-windows-spatial-audio opens_enumerated_endpoints_by_stable_id -- --ignored
```

这两项测试要求默认 endpoint 支持 Spatial Audio：媒体回归检查对象槽位、至少 20 次 render update、
PCM/object buffer、位置提交、Pause 和零欠载；EOS 回归检查对象释放后 renderer 稳定停留在 Ended。
普通 `cargo test` 不依赖本地媒体或音频设备。
