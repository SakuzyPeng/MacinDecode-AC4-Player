# 数据目录与托管文件夹

播放器沿用最新版的多播放列表数据库、JSON 设置和 eframe 窗口存档，详见 [PLAYLISTS.md](PLAYLISTS.md)。本次不引入旧方案的第二套单播放列表数据库，也不搬移已存在的用户资料。

- macOS：`~/Library/Application Support/com.macinrender.macindecode-ac4-player/`
- Windows：`%APPDATA%/com.macinrender.macindecode-ac4-player/data/`
- 开发隔离：`MACINDECODE_PLAYER_DATA_DIR` 或命令行 `--data-dir PATH`。

业务目录含 `library.sqlite3`、`settings.json`、`app.ron`，以及两个托管文件夹：`sofa/` 放 HRIR，`hptf/` 放 AutoEq 耳机补偿曲线。同一数据目录仍由现有操作系统文件锁保护。设置与播放列表继续通过 `LibraryController` 的单一工作线程提交，原有损坏保护、备份、浏览/播放分离及断点恢复均保留。

两个文件夹共用 `file_catalog` 的同一套机制，只由 `Kind` 区分接受的扩展名和用户看到的名词。后台递归扫描，不跟随符号链接；导入先写同目录临时文件，完成并同步后原子提交；同名同内容复用，不同内容追加指纹与序号，取消时删除本次临时文件。相对路径、SHA-256 和文件状态作为版本化派生索引写入现有 SQLite `metadata`，不更改媒体库 schema 或覆盖用户设置。

派生名称全部来自 `Kind::slug`：工作线程是 `<slug>-catalog`，临时文件前缀是 `.<slug>-import-`，索引键是 `<slug>-index-v1`。SOFA 的 slug 就是 `sofa`，因此 `sofa-index-v1` 与旧版本写入的键一致，升级不会丢索引。一个文件夹只收自己的扩展名：`.sofa` 不会被当成补偿曲线，`.txt` 也不会被当成 HRIR，扫描时同样只清点自己那一类。

Audio settings 的两个文件选择器分别默认打开各自的文件夹，外部选择的文件复制进去之后再交给对应的异步切换机制：SOFA 走 HRTF 热切换，补偿曲线走 `adm_scene_output_set_hptf`。已有的外部路径继续有效。扫描本身不证明格式合法——`hptf/` 里任何 `.txt` 都会被列出——真实解析由 MacinRender 在调用线程同步完成，失败沿用现有输出设置恢复行为。缺失条目保留在索引中，不自动替换用户选择。

文件列表将可读取条目显示为 `available`，将当前原生双耳输出实际启用的文件显示为 `in use`。
`SpatialOutputController::active_sofa` 仅在原生输出已成功初始化时返回路径；热切换失败仍返回先前生效的路径。
仅保存或选中 SOFA、输出初始化尚未完成、系统空间音频模式和内置 KEMAR 均不会把文件标为正在使用。
`active_hptf` 是同一套语义，再多一道：补偿是渐进换入的，只有渲染器回显的 `applied_revision` 等于播放器送出的 revision 才算生效，所以选中之后、音频回调取用之前会短暂显示为未运行。系统空间音频和 Windows 对象直通不生成本侧的两声道，永远不会有 `in use`。
使用状态从输出实时派生，刷新文件夹不会清除它；持久化索引仍只记录扫描状态，不把过去的加载成功当作本次启用证明。

`--check-install --data-dir PATH` 验证真实多列表库和设置的关闭/重开，并初始化 MacinRender 空输出会话，不打开音频设备。`--smoke-test --data-dir PATH` 显示真实场景窗口后自动退出；两者都要求显式隔离目录。eframe 存档明确指向该目录内的 `app.ron` 文件。

## 皮肤

`Visual settings → Listener skin` 的导入会在后台验证 PNG 格式、64×64 / 64×32 尺寸和 1 MiB 文件上限，
成功后原子复制到 `skins/<SHA-256>.png`。相同内容复用，不同内容互不覆盖；重命名或移动外部原图
不影响已导入皮肤。`settings.json` 中的 `skins` 保存导入列表、当前选择和每张皮肤的可选体型覆盖，
沿用设置工作线程、备份和损坏保护；旧设置缺少该字段时显示默认角色。
加载失败保留当前角色和已保存列表，在视觉设置中显示具体错误，可重新导入恢复文件或选择默认角色。
