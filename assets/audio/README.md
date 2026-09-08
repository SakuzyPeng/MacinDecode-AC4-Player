# 内置 Atmos 标识辅助素材

`atmos-assist.m4a` 是本项目自行生成的测试信号，采用项目 MIT 许可，不含第三方音乐。
用途是让 macOS 同进程的 AVPlayer 识别到 JOC 内容；应用在每个播放项的
MTAudioProcessingTap 中将全部输出清零，文件里的信号不会混入 AC-4 回放。

源信号为 30 秒、48 kHz、−30 dBFS 的 997 Hz 正弦，带首尾淡变，以及一条缓慢移动的
ADM 对象轨迹。源生成器：`scripts/generate-atmos-assist.py`。原始压缩成品保留为
`atmos-assist-source.m4a`，便于独立重现播放素材；只有 `atmos-assist.m4a` 编入播放器。

一次性生成流程：

```bash
python3 scripts/generate-atmos-assist.py source.wav
mradm render --input source.wav --output source-714.wav --renderer saf \
  --speaker-geometry apple --output-layout 7.1.4 --output-bit-depth i24 --no-peak-limit
dee_ddpjoc_encoder --input-format cbi_wav --input source-714.wav \
  --output assist.ec3 --data-rate 384
ffmpeg -i assist.ec3 -map 0:a:0 -c copy -f mp4 -movflags +faststart atmos-assist-source.m4a
python3 scripts/extend-atmos-assist.py atmos-assist-source.m4a atmos-assist.m4a
```

使用 Release 版 MacinRender 和本地 DME 5.7.2 编码。JOC 按完整编码帧封装，原始成品时长
30.016 秒、384 kbps、约 1.44 MB。延长脚本只接受摘要匹配的原始素材；重新编码生成不同
素材后须先复核脚本的固定包长、时间表和种子摘要，不能直接替换。

播放素材将同一份 `mdat` 压缩包通过 MP4 chunk offsets 引用 2880 次，所有样本的时间戳
连续递增，时长为 24 小时 46.08 秒。文件只增加 11,500 字节，没有重复存储或重新编码音频。
这样 AVPlayer 不必在每个约 30 秒交界重建播放项、解码器及静音 tap，减少标签辅助对
AirPods 系统空间音频回放的干扰。24 小时后的播放项仍由现有双项队列补充。
文件摘要与工具记录见 `atmos-assist.json`；`python3 scripts/test_atmos_asset.py` 检查重现结果，
并在安装 FFmpeg 时验证交界和尾部的实际解封装。普通构建只包含已验证的成品文件，
不需要编码器、延长脚本、MacinRender CLI 或 FFmpeg。

macOS 构建将成品编入可执行文件，原生组件按 SHA-256 写入当前用户缓存目录。
缓存通过内容校验和原子写入维护；正式包和未打包的 Cargo 程序都不依赖开发机素材路径。
