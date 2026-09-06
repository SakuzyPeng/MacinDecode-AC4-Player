# 数据目录与 SOFA 接入

播放器沿用最新版的多播放列表数据库、JSON 设置和 eframe 窗口存档，详见 [PLAYLISTS.md](PLAYLISTS.md)。本次不引入旧方案的第二套单播放列表数据库，也不搬移已存在的用户资料。

- macOS：`~/Library/Application Support/com.macinrender.macindecode-ac4-player/`
- Windows：`%APPDATA%/com.macinrender.macindecode-ac4-player/data/`
- 开发隔离：`MACINDECODE_PLAYER_DATA_DIR` 或命令行 `--data-dir PATH`。

业务目录含 `library.sqlite3`、`settings.json`、`app.ron` 和新建的 `sofa/`。同一数据目录仍由现有操作系统文件锁保护。设置与播放列表继续通过 `LibraryController` 的单一工作线程提交，原有损坏保护、备份、浏览/播放分离及断点恢复均保留。

SOFA 在后台递归扫描，不跟随符号链接。导入先写同目录临时文件，完成并同步后原子提交；同名同内容复用，不同内容追加指纹与序号，取消时删除本次临时文件。相对路径、SHA-256 和文件状态作为版本化派生索引写入现有 SQLite `metadata` 的 `sofa-index-v1`，不更改媒体库 schema 或覆盖用户设置。

Audio settings 的文件选择器默认打开 `sofa/`，外部选择的文件复制到该目录后交给既有异步 HRTF 切换机制。已有外部 SOFA 路径继续有效。扫描本身不证明格式合法；真实加载由 MacinRender 校验，失败沿用现有输出设置恢复行为。缺失条目保留在索引中，不自动替换用户选择。

`--check-install --data-dir PATH` 验证真实多列表库和设置的关闭/重开，并初始化 MacinRender 空输出会话，不打开音频设备。`--smoke-test --data-dir PATH` 显示真实场景窗口后自动退出；两者都要求显式隔离目录。eframe 存档明确指向该目录内的 `app.ron` 文件。
