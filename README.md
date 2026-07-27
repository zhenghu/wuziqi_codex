# wuziqi_codex — 五子棋 (Gomoku)

用 Rust 2024 Edition + [macroquad](https://github.com/not-fl3/macroquad) 编写的五子棋小游戏,支持人机对战和双人对战。附带一个逻辑完全一致的网页移植版,无需安装任何东西即可游玩。

当前版本：`0.2.0-beta.1`

## 版本管理

项目采用[语义化版本](https://semver.org/lang/zh-CN/)：

- Beta 版本使用 `主版本.次版本.修订版本-beta.序号`，例如 `0.2.0-beta.1`
- 同一轮 Beta 修复依次升级为 `beta.2`、`beta.3`
- 功能或兼容性目标变化时升级次版本，例如 `0.3.0-beta.1`
- Beta 验证完成后发布对应稳定版，例如 `0.2.0`

原生版以 `Cargo.toml` 的 `package.version` 为版本源，并在编译时自动显示到窗口标题和界面底部。发布时同时更新网页版 `wuziqi.html` 中的 `APP_VERSION`，并保持 `Cargo.lock` 与 README 一致。

版本变更记录见 [`CHANGELOG.md`](CHANGELOG.md)。

## 玩法

- 🖱️ 鼠标点击交叉点落子(默认你执黑,AI 执白)
- `R` 重新开始
- `U` 悔棋(人机模式回退一整轮；AI 尚未回应时只回退玩家刚落的一步)
- `M` 切换 人机 / 双人 模式
- `A` 切换 经典战术搜索 / 大模型 AI（原生版）
- `C` 打开大模型配置页面（原生版）

人机模式下，棋盘底部会持续显示当前 AI 引擎；OpenRouter 首次成功返回后，显示响应中的实际模型 ID，以及响应提供的供应商信息。

## 运行

### 原生版 (Rust)

```bash
cargo run --release
```

### 大模型 AI（原生版）

大模型模式使用 OpenRouter Chat Completions API。点击顶部 `Config (C)` 或按 `C` 打开配置页面，可填写 OpenRouter API Key 和模型名称；为防止密钥被发送到第三方服务器，API 地址仅接受 OpenRouter 官方 HTTPS 端点。配置保存在系统用户配置目录，并在 macOS/Linux 上设置为仅当前用户可读写。

macOS 配置路径为 `~/Library/Application Support/Wuziqi/llm_config.json`。旧版项目目录中的 `llm_config.json` 会在首次启动时自动迁移到新位置，并归档为 `llm_config.json.migrated`，避免误删原文件。也可以复制示例文件后直接编辑 JSON：

```bash
mkdir -p "$HOME/Library/Application Support/Wuziqi"
cp llm_config.example.json "$HOME/Library/Application Support/Wuziqi/llm_config.json"
cargo run --release
```

```json
{
  "api_key": "YOUR_OPENROUTER_API_KEY",
  "api_url": "https://openrouter.ai/api/v1/chat/completions",
  "model": "openai/gpt-5-mini"
}
```

仓库中的 `llm_config.example.json` 不包含真实密钥。配置页面支持 API Key 脱敏显示、显示/隐藏、`Paste` 按钮、`Cmd/Ctrl+V` 粘贴和保存前校验。保存后自动切换到 OpenRouter AI。请求超时、服务报错或模型返回非法坐标时，最多自动尝试 3 次；界面会显示经过单行化和长度限制的失败原因，全部失败后才降级到经典搜索。重试期间仍可通过悔棋、重开、切换模式或切换 AI 取消请求。

没装 Rust 的话,macOS 下直接双击 `run_wuziqi.command`,脚本会自动通过 [rustup](https://rustup.rs) 安装工具链并编译运行。

### 网页版

将 `wuziqi.html` 和 `ai.js` 放在同一目录,直接用浏览器打开 `wuziqi.html` 即可。棋局规则与原生版一致，AI 使用无需联网的经典战术搜索。

## AI 实现

经典算法采用战术约束下的迭代加深 Alpha-Beta 搜索：先识别立即获胜、唯一必防和一步双杀，再根据开局、中盘、后期动态调整候选宽度，在固定节点预算内搜索最多 4 层，并保留宽松的时间上限作为安全保护。固定节点预算让相同局面在不同平台得到一致结果。评分同时识别连续棋形与跳三、跳四，搜索使用局面哈希和置换表减少重复计算。大模型算法复用同一套战术约束生成候选集，再由语言模型结合攻防、后续威胁和中心控制做最终决策。网络请求在独立线程执行，不会阻塞窗口渲染。

## 项目结构

```
├── Cargo.toml              # Rust 项目配置 (依赖: macroquad 0.4)
├── CHANGELOG.md            # 版本变更记录
├── src/
│   ├── main.rs             # 程序入口
│   ├── app.rs              # 主循环、输入调度与 AI 请求状态
│   ├── game.rs             # 棋局状态与规则
│   ├── board_view.rs       # 棋盘绘制与坐标换算
│   ├── ai.rs               # Rust 版 AI 搜索
│   ├── config_ui.rs        # 大模型配置页面
│   └── llm_ai.rs           # 大模型 API、提示词、结果校验
├── llm_config.example.json # OpenRouter 配置示例（不含真实 Key）
├── ai.js                   # 网页版 AI 搜索
├── wuziqi.html             # 网页版游戏界面与交互
├── tests/unit_tests/mod.rs # Rust 单元测试
└── run_wuziqi.command      # macOS 一键启动脚本
```
