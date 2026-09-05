# 应用图标

采用定稿 B：陶橙色 16×16 像素纹理，播放三角保留垂直左边及两条阶梯斜边。

- `app-macos.svg`：圆角外形，用于当前 `.icns` 打包流程。
- `app-windows.svg`：直角方块外形，Windows 和 Linux 窗口使用这一版。
- `app-macos.icns`：16–1024 px 的 macOS 图标，打包为 `Resources/AppIcon.icns`。
- `app-windows.ico`：16、20、24、32、40、48、64、96、128、256 px，由 `windows.rc` 嵌入 Windows 可执行文件。
- 两份 256 px PNG：编译进程序，供主窗口和独立详情、诊断窗口使用。

生成文件随源码保存；正常 Cargo 构建和打包不需要 Node.js 或 SVG 渲染器。
修改 SVG 后，在仓库根目录运行以下命令重新生成全部二进制图标：

```sh
npm install --prefix target/icon-tools --no-audit --no-fund --save-exact @resvg/resvg-js@2.6.2
node scripts/generate-icons.cjs
```

每个尺寸直接从 SVG 渲染，保留透明背景及像素边缘。若改用 Icon Composer，需另行准备未裁圆的
方形图层，由系统施加外形蒙版；当前圆角 SVG 是已有 `.icns` 流程的成品外形。
