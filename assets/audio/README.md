# 内置 Atmos 标识辅助素材

`atmos-assist.m4a` 是本项目自行生成的测试信号，采用项目 MIT 许可，不含第三方音乐。
用途是让 macOS 同进程的 AVPlayer 识别到 JOC 内容；应用在每个播放项的
MTAudioProcessingTap 中将全部输出清零，文件里的信号不会混入 AC-4 回放。

源信号为 30 秒、48 kHz、−30 dBFS 的 997 Hz 正弦，带首尾淡变，以及一条缓慢移动的
ADM 对象轨迹。源生成器：`scripts/generate-atmos-assist.py`。

一次性生成流程：

```bash
python3 scripts/generate-atmos-assist.py source.wav
mradm render --input source.wav --output source-714.wav --renderer saf \
  --speaker-geometry apple --output-layout 7.1.4 --output-bit-depth i24 --no-peak-limit
dee_ddpjoc_encoder --input-format cbi_wav --input source-714.wav \
  --output assist.ec3 --data-rate 384
ffmpeg -i assist.ec3 -map 0:a:0 -c copy -f mp4 -movflags +faststart atmos-assist.m4a
```

使用 Release 版 MacinRender 和本地 DME 5.7.2 编码。JOC 按完整编码帧封装，成品时长
30.016 秒、384 kbps、约 1.44 MB；文件摘要与工具记录见 `atmos-assist.json`。
普通构建只包含已验证的成品文件，不需要编码器、MacinRender CLI 或 FFmpeg。

macOS 构建将成品编入可执行文件，原生组件按 SHA-256 写入当前用户缓存目录。
缓存通过内容校验和原子写入维护；正式包和未打包的 Cargo 程序都不依赖开发机素材路径。
