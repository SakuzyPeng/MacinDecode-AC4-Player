# 数据目录与托管文件夹

播放器沿用最新版的多播放列表数据库、JSON 设置和 eframe 窗口存档，详见 [PLAYLISTS.md](PLAYLISTS.md)。数据目录由应用 ID `com.macinrender.macindecode-spatial-player` 决定；改名前的目录只在下文「从旧应用 ID 迁移」描述的条件下复制一次。

- macOS：`~/Library/Application Support/com.macinrender.macindecode-spatial-player/`
- Windows：`%APPDATA%/com.macinrender.macindecode-spatial-player/data/`
- Linux：`${XDG_DATA_HOME:-~/.local/share}/com.macinrender.macindecode-spatial-player/`
- 开发隔离：`MACINDECODE_PLAYER_DATA_DIR` 或命令行 `--data-dir PATH`。

## 从旧应用 ID 迁移

改名前应用 ID 是 `com.macinrender.macindecode-ac4-player`，数据目录跟着它走。`preferences::migration`
在 `DataDirectory::acquire` 里、加锁之前处理一次，规则如下：

- **只在默认位置**：显式 `--data-dir` 或 `MACINDECODE_PLAYER_DATA_DIR` 没有前身，不迁移。
- **只在新目录不存在时**：新目录一旦存在（哪怕是空的），就以它为准，永不覆盖。
- **复制，不搬移**：先拿旧目录的 `player.lock`，旧版仍在运行就报错退出、什么都不建；拿到锁后把整棵树
  复制到同级的 `<新目录>.migrating`（逐文件 fsync，跳过 `player.lock`，符号链接按它指向的文件复制），
  完成后一次 `rename` 成新目录。中断留下的 `.migrating` 下次启动删掉重来，不会被当成已迁移的数据。
- **旧目录原样保留**：只多写一份说明去向的 `MOVED.txt`，旧版仍能用它启动。
- **失败即报错，不新建**：若迁移失败就开一个空目录，新目录存在后再也不会迁移，用户会以为数据丢了。

设置里的 SOFA 与 HpTF 是绝对路径，托管文件在数据目录里。`AppPreferences::relocated` 在每次加载设置时把
落在旧目录下的路径换到新目录——每次而不是一次，这样日后从旧目录拷回来的 `settings.json` 也能找到副本。
数据库里的媒体路径不在数据目录内，SOFA / HpTF 索引存的是相对路径，二者都不需要改写。
macOS 的 Atmos 标识辅助音轨是缓存，换到新 ID 的缓存目录后按需重新生成，不迁移。

业务目录含 `library.sqlite3`、`settings.json`、`app.ron`，以及两个托管文件夹：`sofa/` 放 HRIR，`hptf/` 放 AutoEq 耳机补偿曲线。同一数据目录仍由现有操作系统文件锁保护。设置与播放列表继续通过 `LibraryController` 的单一工作线程提交，原有损坏保护、备份、浏览/播放分离及断点恢复均保留。

两个文件夹共用 `file_catalog` 的同一套机制，只由 `Kind` 区分接受的扩展名和用户看到的名词。后台递归扫描，不跟随符号链接；导入先写同目录临时文件，完成并同步后原子提交；同名同内容复用，不同内容追加指纹与序号，取消时删除本次临时文件。相对路径、SHA-256 和文件状态作为版本化派生索引写入现有 SQLite `metadata`，不更改媒体库 schema 或覆盖用户设置。

派生名称全部来自 `Kind::slug`：工作线程是 `<slug>-catalog`，临时文件前缀是 `.<slug>-import-`，索引键是 `<slug>-index-v1`。SOFA 的 slug 就是 `sofa`，因此 `sofa-index-v1` 与旧版本写入的键一致，升级不会丢索引。一个文件夹只收自己的扩展名：`.sofa` 不会被当成补偿曲线，`.txt` 也不会被当成 HRIR，扫描时同样只清点自己那一类。

Audio settings 的两个文件选择器分处 `HRTF` 与 `Headphones` 两页，各自默认打开对应的文件夹，外部选择的文件复制进去之后再交给对应的异步切换机制：SOFA 走 HRTF 热切换，补偿曲线走 `adm_scene_output_set_hptf_ex`。已有的外部路径继续有效。扫描本身不证明格式合法——`hptf/` 里任何 `.txt` 都会被列出——真实解析由 MacinRender 在调用线程同步完成，失败沿用现有输出设置恢复行为。缺失条目保留在索引中，不自动替换用户选择。

文件列表将可读取条目显示为 `available`，将当前原生双耳输出实际启用的文件显示为 `in use`。
列表高度封顶在 `CATALOG_ROWS_SHOWN` 行（按样式的 `interact_size` 与 `item_spacing` 换算，不写死像素），超出部分就地滚动：文件夹条目数无上限，而设置窗口是唯一会被它撑破的地方。封顶之后选中行可能落在视口外，因此列表记住上一次滚到的行，仅在选中项变化时滚动一次——滚轮不会被每帧拉回，而「Save as profile…」这种由导入而非点击改变选中的情况也能滚到新文件。
`SpatialOutputController::active_sofa` 仅在原生输出已成功初始化时返回路径；热切换失败仍返回先前生效的路径。
仅保存或选中 SOFA、输出初始化尚未完成、系统空间音频模式和内置 KEMAR 均不会把文件标为正在使用。
`active_hptf` 使用最后成功准备的配置身份；请求失败时恢复该配置及自动衰减策略，允许修正后重试。它是同一套语义，再多一道：补偿是渐进换入的，只有渲染器回显的 `applied_revision` 等于播放器送出的 revision 才算生效，所以选中之后、音频回调取用之前会短暂显示为未运行。系统空间音频和 Windows 对象直通不生成本侧的两声道，永远不会有 `in use`。
使用状态从输出实时派生，刷新文件夹不会清除它；持久化索引仍只记录扫描状态，不把过去的加载成功当作本次启用证明。

`--check-install --data-dir PATH` 验证真实多列表库和设置的关闭/重开，并初始化 MacinRender 空输出会话，不打开音频设备。`--smoke-test --data-dir PATH` 显示真实场景窗口后自动退出；两者都要求显式隔离目录。eframe 存档明确指向该目录内的 `app.ron` 文件。

## 皮肤

`Visual settings → Listener skin` 的导入会在后台验证 PNG 格式、64×64 / 64×32 尺寸和 1 MiB 文件上限，
成功后原子复制到 `skins/<SHA-256>.png`。相同内容复用，不同内容互不覆盖；重命名或移动外部原图
不影响已导入皮肤。`settings.json` 中的 `skins` 保存导入列表、当前选择和每张皮肤的可选体型覆盖，
沿用设置工作线程、备份和损坏保护；旧设置缺少该字段时显示默认角色。
加载失败保留当前角色和已保存列表，在视觉设置中显示具体错误，可重新导入恢复文件或选择默认角色。
