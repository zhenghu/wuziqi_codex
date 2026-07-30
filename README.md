# wuziqi_codex — 五子棋 (Gomoku)

用 Rust 2024 Edition + [macroquad](https://github.com/not-fl3/macroquad) 编写的五子棋小游戏。原生版支持人机对战、双人对战和双模型擂台，网页移植版支持无需安装的经典 AI 与双人对战。

当前版本：`1.0.0`

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
- `M` 切换 人机 / 双人 / 双模型擂台模式（擂台仅原生版）
- `A` 切换 经典战术搜索 / 大模型 AI（原生版）
- `C` 打开大模型配置页面（原生版）

人机模式下，棋盘底部会持续显示当前 AI 引擎；大模型首次成功返回后，会显示实际模型 ID，以及响应提供的供应商信息。

双模型擂台中，黑白双方各使用一份独立配置，黑棋先行并自动轮流请求模型。空格用于开始、暂停和继续，`R` 清空棋盘并回到准备状态，`S` 交换双方颜色并重开，`C` 编辑双方配置。擂台禁止人工落子和悔棋；一方连续 3 次请求失败后判技术负，不会由战术 AI 代走。底部的 `Duel` 状态会分别保留双方最近一次实际模型路由。

## 运行

### 原生版 (Rust)

```bash
cargo run --release
```

### 大模型 AI（原生版）

大模型模式支持 OpenRouter 和本地 OpenAI-compatible 服务。点击顶部 `Config (C)` 或按 `C` 打开配置页面；普通模式使用 `Primary` / `Add` 管理配置，擂台设置则以 `Black` / `White` 标识当前棋色。普通人机模式可以只保存一份配置，进入擂台时必须保存两份。OpenRouter 后端需要 API Key，且只接受 OpenRouter 官方 HTTPS 端点；本地后端默认连接 Ollama，也兼容提供 `/v1/chat/completions` 的 LM Studio 和 llama.cpp。

为避免云端密钥泄露，本地后端只接受 `http` 或 `https` 的数字回环地址（`127.0.0.1` 或 `::1`，不接受 `localhost`），并且请求本地服务时不会发送 OpenRouter API Key。配置保存在系统用户配置目录，并在 macOS/Linux 上设置为仅当前用户可读写。

macOS 配置路径为 `~/Library/Application Support/Wuziqi/llm_config.json`。旧版项目目录中的 `llm_config.json` 会在首次启动时自动迁移到新位置，并归档为 `llm_config.json.migrated`，避免误删原文件。也可以复制示例文件后直接编辑 JSON：

```bash
mkdir -p "$HOME/Library/Application Support/Wuziqi"
cp llm_config.example.json "$HOME/Library/Application Support/Wuziqi/llm_config.json"
cargo run --release
```

```json
{
  "schema_version": 2,
  "profiles": [
    {
      "name": "GPT-5 mini",
      "backend": "openrouter",
      "api_key": "YOUR_OPENROUTER_API_KEY",
      "api_url": "https://openrouter.ai/api/v1/chat/completions",
      "model": "openai/gpt-5-mini"
    },
    {
      "name": "Qwen Local",
      "backend": "local",
      "api_url": "http://127.0.0.1:11434/v1/chat/completions",
      "model": "qwen3:4b"
    }
  ],
  "active_profile": 0
}
```

仓库中的 `llm_config.example.json` 不包含真实密钥。两份 profile 会通过一次原子替换共同保存，旧版单配置会自动迁移为一个 profile，不会擅自复制成第二名选手。配置页面支持 API Key 脱敏显示、显示/隐藏、`Paste` 按钮、`Cmd/Ctrl+V` 粘贴和保存前校验。请求超时、服务报错或模型返回非法坐标时，最多自动尝试 3 次；人机模式随后降级到经典搜索，擂台模式则判当前模型技术负。暂停、重开、切换模式或打开配置都会真正取消在途请求，迟到响应不会改变新棋局。

#### 使用本地 Ollama

安装 Ollama 后启动本地服务：

```bash
ollama serve
```

如果 Ollama 桌面应用已经在后台运行，无需再次执行 `ollama serve`。然后在另一个终端下载默认推荐的轻量模型：

```bash
ollama pull qwen3:4b
```

在配置页面选择 `Local` 后端即可使用默认地址；也可以直接保存以下配置：

```json
{
  "schema_version": 2,
  "profiles": [
    {
      "name": "Qwen Local",
      "backend": "local",
      "api_url": "http://127.0.0.1:11434/v1/chat/completions",
      "model": "qwen3:4b"
    }
  ],
  "active_profile": 0
}
```

使用 LM Studio 或 llama.cpp 时，先启动其 OpenAI-compatible 本地服务器，再把 `api_url` 改为对应数字回环地址，并将 `model` 改为服务暴露的模型 ID。本地配置不需要 `api_key`，应用不会把云端 Key 附加到本地请求。

没装 Rust 的话,macOS 下直接双击 `run_wuziqi.command`,脚本会自动通过 [rustup](https://rustup.rs) 安装工具链并编译运行。

### 网页版

将 `wuziqi.html` 和 `ai.js` 放在同一目录,直接用浏览器打开 `wuziqi.html` 即可。棋局规则与原生版一致，AI 使用无需联网的经典战术搜索。

## AI 实现

经典算法采用战术约束下的迭代加深 Alpha-Beta 搜索：先识别立即获胜、唯一必防和一步双杀，再根据开局、中盘、后期动态调整候选宽度，在固定节点预算内搜索最多 4 层，并保留宽松的时间上限作为安全保护。固定节点预算让相同局面在不同平台得到一致结果。评分同时识别连续棋形与跳三、跳四，搜索使用局面哈希和置换表减少重复计算。大模型算法复用同一套战术约束生成候选集，再由语言模型结合攻防、后续威胁和中心控制做最终决策，因此当前擂台属于“战术候选辅助”对战，而不是完全无辅助的纯模型棋力测试。网络请求在独立线程执行，不会阻塞窗口渲染。

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
│   ├── llm_ai.rs           # 大模型共享配置、提示词与结果校验
│   ├── test_support.rs      # Rust 测试共享构造器
│   └── llm_ai/
│       ├── openrouter.rs   # OpenRouter 请求与响应适配
│       └── local.rs        # 本地兼容请求与安全校验
├── llm_config.example.json # 大模型配置示例（不含真实 Key）
├── ai.js                   # 网页版 AI 搜索
├── wuziqi.html             # 网页版游戏界面与交互
├── tests/
│   ├── version_consistency.rs # 公共契约集成测试
│   └── ai_js.test.js          # 网页 AI 合约测试
└── run_wuziqi.command      # macOS 一键启动脚本
```

需要访问私有实现的 Rust 单元测试分别位于 `src/ai/tests.rs`、`src/app/tests.rs`、`src/game/tests.rs` 和 `src/llm_ai/tests.rs`。
