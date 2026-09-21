# 传感器增强验证记录（2026-09-21）

实现固定到 PoseBridge 0.5.0 提交 `b8b6a415ecdff8a0e1f2fdd0979798d80f84ad21`。
Player 验证代码提交为 `98bf6ecb237adc196fb9c1e3050ef7537ae04e43`。
RenderCore 提交与音频接口未改动；增强默认关闭。用户此前确认的 20 Hz 普通体验仍是默认基准。

## 软件验证

- PoseBridge：macOS 40 项、Windows 29 项 Rust 测试通过（Unix 伪串口测试不在 Windows 执行）。
  两平台 Release 构建、Clippy、四种 CLI→OSC 组合、C ABI 400／C11 策略和迁移消费测试通过。
  新增批次／游标／溢出测试、E4 拆包与粘包、安装转换、同一次 read 内不同帧不拼接字段，
  以及 E4 非 20 Hz 在写入前拒绝。实验工具的成功、失败与 Ctrl-C 恢复流程使用假设备验证通过。
- Player 合成对照：独立真值的 20 Hz、90°/s 恒速转动，包含交付抖动、10 ms 相同平滑，
  普通 RMS **5.0470°**，增强 **3.2523°**，下降 **35.6%**。批量交付的组合轴轨迹也通过 ≥20% 改善门槛。
- 静止额外抖动 <0.1°、25 ms／5° 上限、急停与反向不自由积累、历史溢出／时钟代次重置／
  重复时间／缺字段／冲击降级、映射年龄过期冻结、50 ms 切换、实测回正和安装检查均有回归覆盖。
  ±180° 和俯仰 ±90° 的组合姿态通过物理 ZXY 往返验证。
- Player 回归：macOS 主程序 374 项、原生封装 5 项通过；Windows 主程序 371 项、原生封装 4 项、
  Windows 空间音频封装 3 项通过。真实设备／媒体等 ignored 用例另行验收。控制线程的 20 Hz
  模拟器验证了开关不重连、保持与停止冻结。
- 两平台 fmt、默认功能全 targets Clippy、默认 Release 构建、无默认功能 `cargo check` 均通过；
  macOS 无默认功能全 targets Clippy 也通过。Windows 最终 EXE 的系统 DLL 导入检查通过。
- macOS 现有测试 `.app` 已更新：签名和运行库检查通过，窗口实际渲染 40 帧，VBAP 与双耳渲染
  各呈现 4800 帧；没有以此代替传感器实机或实听验收。

这里的时间映射只估计相对最低交付包络的额外滞留。固定链路／融合延迟与完整音频输出延迟未知，
没有用音频排队帧数替代端到端测量。

## 实机与实听

| 平台／路径 | E4、20 Hz、预热后 60 秒、静止与三轴转动 |
|---|---|
| macOS USB | 未完成 |
| macOS BLE | 未完成 |
| Windows USB | 未完成 |
| Windows BLE | 未完成 |

`FULL_INERTIAL_20HZ_VALIDATED=false`。没有任何完整帧正式预设被开放，准备增强数据仍选已验证 A4。
Motion 格式的加速度可显示并参与质量判断，但它缺少设备时间戳，不能据此启用预测。

执行实机验收使用 [PoseBridge 的配置恢复与回读验收工具](https://github.com/SakuzyPeng/PoseBridge/blob/b8b6a415ecdff8a0e1f2fdd0979798d80f84ad21/docs/motion.md)。
每轮先读取原配置，结束／失败后恢复并核验；不校准、归零或 Flash 保存。尚未进行本次增强开／关的实听，
应固定同一 A4 格式、20 Hz、平滑和音频输出链路，仅切换 Tracking 中的 Sensor-side enhancement。
此前“20 Hz 足够灵敏”的主观反馈不作为新算法收益的验收结果。
