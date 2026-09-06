# 控制中心 Atmos 标识实验

独立 macOS PoC。首个目标是直接验证：同一进程持续播放被 tap 清零的 JOC 音轨时，
由另一套 ASBR 输出的独立 AC-4 渲染 PCM 是否会被控制中心显示为「杜比全景声」。

## 实测结果：组合成功

2026-09-06，macOS 27.0，当前蓝牙耳机输出：用户在控制中心确认「Atmos 标识实验」显示
**杜比全景声**。对应运行记录确认同一 PID 内两条链都在运行：

- 独立 AC-4 PCM：48 kHz、12 声道 f32，布局标签 `0x00c0000c`（Atmos 7.1.4），
  ASBR 状态为 rendering、速率 1，未报告错误。
- 辅助 JOC：AVPlayer 状态为 playing，tap 输出为 48 kHz / 12 声道，解码帧数持续增长，
  数据全部清零，tap 未报告错误。

随后扩展同一段 AC-4 的输出布局：

| PCM 布局 | 声道数 | ASBR 输入布局标签 | 控制中心观察 |
| --- | ---: | --- | --- |
| 7.1.4 | 12 | `Atmos_7_1_4` / `0x00c0000c` | 用户确认：杜比全景声 |
| 9.1.6 | 16 | `Atmos_9_1_6` / `0x00c10010` | 用户确认：杜比全景声 |
| 22.2 | 24 | `CICP_13` / `0x00cc0018` | 用户确认：多声道、正常出声；重建 JOC 后仍为多声道 |

9.1.6 使用重启后的实验程序，22.2 在同一进程保持 JOC 活跃时切换 PCM。日志分别确认
ASBR 实际输入为 16 / 24 声道，均为 48 kHz，渲染速率 1，无报错。22.2 使用等功率
LFE 分配：单路 LFE 向两个 LFE 声道各乘 `1/sqrt(2)`。

22.2 的首次观察与前两项启动顺序不同：前两项先启动 PCM、后启动 JOC；22.2 首次是在
JOC 已运行时新建 ASBR。为排除顺序影响，保持 24 声道 PCM 连续播放，单独停止、释放并
重建 JOC AVPlayer。日志确认此时 PCM 时间线继续推进，JOC 时间线重新从零开始。
用户确认重建后标识仍为「多声道」。因此单独重启辅助 JOC 没有让本次 22.2 输出显示 Atmos。
本结果只说明已测试组合的布局差异，不能据此认定系统具有普遍的 16 声道 Atmos 标识上限。

这确认了已观察到的组合在本次环境能显示 Atmos。尚未进行停止辅助 AVPlayer、跨进程对照或
其他设备的复核，因而不将本结果扩大为对系统内部分类机制或普遍稳定性的证明。
可听 PCM 与 JOC 素材完全独立。上述记录来自独立 PoC；正式播放器的后续集成见
[播放集成说明](../../docs/MACINRENDER.md)。

## 当前组合

```text
AC-4 → MacinDecode Core → ADM → MacinRender SAF VBAP / Apple 几何
     → 7.1.4 / 9.1.6 / 22.2 f32 PCM CAF → AVSampleBufferAudioRenderer → 可听输出

另一首 E-AC-3/JOC → AVPlayer → MTAudioProcessingTap → 全部清零 → 静音输出
```

两条链在同一进程中运行，各自使用自己的时间线，互不传递 PCM、格式描述或对象元数据。
PCM 输出校验 CAF 的声道数与布局标签，并使用对应布局，格式描述的 cookie/extensions 均为空。
JOC 使用与 SpatialCompare 相同的 post-effects tap 清零方式，AVPlayer 音量保持 1。
tap 的解码帧数、声道数及错误数通过原子计数器记录。

本实验使用预先渲染的真实 AC-4 PCM。它验证内容标识与 PCM 来源的关系；不代表已经把
辅助 AVPlayer 接进正式播放器，也不用于证明离线 ADM 中转与正式 Scene 实时渲染逐样本相同。

## 构建与操作

需要 Apple Silicon、macOS 15 或更新版本及 Swift 编译器。

```bash
bash experiments/atmos-control-center/build.sh /path/to/joc.m4a /path/to/ac4-714.caf
```

产物位于 `target/atmos-control-center/AtmosBadgeProbe.app`，采用 ad hoc 签名。

1. 将 `ac4-714.caf`、`ac4-916.caf`、`ac4-222.caf` 放在配置 PCM 所在目录。
2. 点击「测试 7.1.4」「测试 9.1.6」或「测试 22.2」，自动启动相应 PCM 和静音 JOC。
   也可通过文件选择器选择单个 PCM，并分别启动两个引擎。
3. 确认两个帧数都持续增长；在控制中心查看「Atmos 标识实验」的音频分类。
4. 记录实际显示文字及观察时间。若需要定位原因，再分别停止 JOC 或 PCM。

PCM 文件要求匹配 7.1.4 / 9.1.6 / 22.2 的声道数与标签，最多加载前 30 秒并循环。文件 PCM 与有声 JOC 参考均降低约
18 dB；合成音仅作为备用输入。「全部停止」的快捷键为空格。

## 证据与边界

- `target/atmos-control-center/events.jsonl`：同一 PID 下两个引擎的状态，每两秒记录。
  扩展布局版本改为追加写入，保留不同 PID 的历史记录。
- `target/atmos-control-center/combined-run.json`：素材摘要和组合运行时的快照。
- `target/atmos-control-center/combined-success-events.jsonl`：用户确认 Atmos 显示时保存的日志副本。
- `target/atmos-control-center/render.log`：实际 AC-4 PCM 的离线渲染记录。
- `target/atmos-control-center/combined-916.json`：9.1.6 的用户观察与运行快照。
- `target/atmos-control-center/combined-222-initial.json`：22.2 首次切换后的用户观察与运行快照。
- `target/atmos-control-center/combined-222-restart-joc.json`：保持 24 声道播放并重建 JOC 后的复核。
- `target/atmos-control-center/layout-results.json`：三个布局及 22.2 复核的结果汇总。
- `target/atmos-control-center/layout-assets.json`：16 / 24 声道 PCM 的文件摘要、采样率和标签。
- `target/atmos-control-center/render-916.log`、`render-222.log`：对应布局的 Release 渲染记录。
- 控制中心文字需要独立观察，程序不会根据引擎状态自动判定其显示结果。

媒体、应用和运行日志位于被 Git 忽略的 `target/` 下。此目录只保存实验源码和构建脚本。
