# Codex 额度

一个使用 Rust、Tauri 2、React 和 TypeScript 构建的 Windows 桌面额度监测器。

应用使用独立设计的深色额度圆环图标，并为 Windows EXE、安装程序、任务栏和系统托盘生成完整尺寸资源。

应用通过本机 `codex app-server` 的官方 JSON-RPC 接口读取当前 ChatGPT 账户的 Codex 速率限制，不读取或保存登录令牌。

## 窗口行为

- 主窗口右上角的“悬浮窗”开关用于显示或隐藏额度悬浮窗；设置里可选择紧凑卡片或浮动小球。
- 点击主窗口最小化按钮会隐藏到 Windows 系统托盘。
- 左键点击托盘图标可恢复并聚焦主窗口。
- 右键点击托盘图标可显示主窗口、切换悬浮窗或完全退出。
- 点击主窗口关闭按钮会完全退出应用；悬浮窗上的短横线只隐藏悬浮窗。
- 紧凑卡片未固定时可拖动；固定后不可拖动，并通过 Tauri 原生窗口接口启用鼠标穿透，不会挡住下方应用。
- 固定状态下可从主窗口或托盘菜单取消固定。
- 固定后悬浮窗主体鼠标穿透，但悬浮窗上的取消固定按钮仍可点击。
- 小球采用类似 360 加速球的“圆环 + 信息胶囊”样式；贴边时收起为独立小球并在悬停后展开额度摘要，不贴边时保持展开。小球可拖到任意位置，靠近屏幕边缘会自动吸附，点击小球不会打开主窗口。
- 浮动小球可在设置中选择自动、向左展开或向右展开，方向会写入本地配置并在下次启动时保留。
- 主窗口可将悬浮窗黑色背景透明度设为 0%–100%；数值越高背景越透明，文字、数值、进度条和按钮不会随之变淡。
- 主窗口使用绿色自定义标题栏，与应用界面保持一致。
- 设置可在“显示可用”和“显示已使用”两种额度形式间切换；主窗口圆环显示所选形式，圆环外侧显示相反形式，悬浮窗同步使用所选形式。
- 设置提供青柠、海蓝、紫罗兰、琥珀和玫瑰五种主题；选择会保存到 `data/state.json`，并同步应用到主窗口与悬浮窗。
- 网络支持系统代理（默认）、无代理和自定义本地代理地址；自定义地址支持 `http`、`https` 与 `socks5`。
- 悬浮窗可单独配置是否始终置顶。
- Windows 使用单实例运行；再次从任务栏启动应用时会恢复并聚焦已有主窗口。
- 任务栏右键跳转列表提供“显示主窗口”“显示 / 隐藏悬浮窗”“固定 / 取消固定悬浮窗”和“完全退出”。
- 安装与覆盖升级都会创建或刷新桌面快捷方式，并绑定独立应用标识和图标。

## 响应与线程

- Codex 额度读取在独立阻塞工作线程执行，不占用 GUI 事件线程。
- `state.json` 由独立后台写入线程保存；连续移动窗口时自动合并待写状态。
- 固定悬浮窗的鼠标穿透检测由独立轻量线程处理。
- 窗口按钮和拖动只负责原生窗口操作，彼此不会等待额度刷新。

## 本地状态与启动速度

应用会在 EXE 所在目录创建 `data/state.json`。这里会保存首次检索到的 Codex 路径、最后一次成功额度、主窗口位置，以及卡片悬浮窗位置和小球悬浮窗位置两套独立坐标；同时保存悬浮窗样式、展开方向、显示、固定和透明度设置。切换样式不会覆盖另一种样式的位置。下次启动先读取这些信息，再在后台刷新额度；不会保存登录令牌。缓存的 Codex 路径不存在或不能正常响应时，应用会自动重新检索并覆盖旧路径。首次升级到此版本时，如果旧版 `%APPDATA%` 数据存在，会自动迁移一次。

如果旧版本保存的主窗口坐标已经位于断开的显示器或屏幕外，启动时会自动将主窗口移回当前主显示器并保持可见。

## 环境要求

- Windows 10/11 与 WebView2
- Rust MSVC 工具链
- Node.js 20+
- 已安装 Codex 桌面应用或 Codex CLI
- 已在该电脑上使用 ChatGPT 账号登录 Codex；API Key 登录没有 ChatGPT 订阅额度窗口

应用会自动查找 Codex Desktop 的版本目录、npm/nvm 全局安装目录及系统 `PATH`。如果使用自定义安装位置，可设置环境变量 `CODEX_QUOTA_CODEX_PATH` 指向 `codex.exe`。

首次在其他电脑使用时，建议先运行：

```powershell
codex login
codex login status
```

## 开发

```powershell
npm install
npm run tauri dev
```

## 构建安装包

```powershell
npm run tauri build
```

安装包会生成到 `src-tauri/target/release/bundle/`。

## GitHub Actions 自动构建

`.github/workflows/windows-build.yml` 会在推送分支、创建 Pull Request 或手动运行时，分别构建 Windows x64 和 ARM64 版本。每个架构都会输出：

- `Codex-Quota-Windows-x64-Setup.exe` / `Codex-Quota-Windows-arm64-Setup.exe`：NSIS 安装包，会创建或更新桌面快捷方式。
- `Codex-Quota-Windows-x64.exe` / `Codex-Quota-Windows-arm64.exe`：不经安装器的独立可执行文件。

普通工作流运行的文件可从 GitHub Actions 对应运行记录的 Artifacts 下载，保留 14 天。推送 `v` 开头的标签（例如 `v0.6.1`）时，工作流还会将两个架构的安装包和独立 exe 附加到 GitHub Release。ARM64 产物在 Windows 11 ARM64 runner 上原生编译；x64 产物可运行于常见 Intel/AMD 64 位 Windows 电脑。该工作流仅生成 Windows 产物。

## 本地交叉编译

安装 Rust MSVC 工具链以及对应架构的 C++ 构建工具后，可按需编译单个目标：

```powershell
# Windows x64（Intel / AMD）
rustup target add x86_64-pc-windows-msvc
npm run tauri -- build --target x86_64-pc-windows-msvc --bundles nsis

# Windows ARM64
rustup target add aarch64-pc-windows-msvc
npm run tauri -- build --target aarch64-pc-windows-msvc --bundles nsis
```
