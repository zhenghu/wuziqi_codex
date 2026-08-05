# 更新日志

本项目的版本号遵循[语义化版本](https://semver.org/lang/zh-CN/)，重要变更记录在此文件。

## [Unreleased]

### 新增

- 云端大模型支持任意 HTTPS OpenAI 兼容服务（DeepSeek、ModelArts 等），不再局限于 OpenRouter 域名
- 新增 `no_reasoning` 配置项，对推理模型发送 `thinking: {"type":"disabled"}`，避免复杂局面下推理耗尽 token 导致答案为空；配置页面提供对应勾选框
- 大模型落点失败重试时排除模型选错的位置并在提示词中告知模型避开，提升不稳定模型的成功率

### 改进

- `openrouter.rs` 重构为通用 `cloud.rs`，统一使用标准 OpenAI 兼容请求格式（`max_tokens`，不带 OpenRouter 专属 `reasoning` 字段）
- 落点解析支持带解释文字、Markdown 代码块、中文标点和多候选讨论等常见模型返回格式

## [1.0.0] - 2026-07-30

### 新增

- OpenRouter 请求失败后显示失败原因，最多自动尝试 3 次，再降级到战术搜索
- 支持 Ollama、LM Studio 和 llama.cpp 等本地 OpenAI-compatible 大模型服务
- 新增双模型擂台模式，支持黑白独立配置、自动轮流落子、暂停、重开和交换颜色
- 擂台分别展示双方实际模型路由，请求重试耗尽后按技术负结束且不使用战术 AI 代走

### 改进

- 本地后端只允许数字回环地址，且不会保存或发送 OpenRouter API Key
- 大模型配置升级为带版本的双 profile 格式，保留旧单配置自动迁移并继续采用原子写入
- 大模型请求显式携带当前棋色，并通过对局、请求和手数快照丢弃迟到响应

## [0.2.0-beta.1] - 2026-07-27

### 新增

- 战术约束下的迭代加深 Alpha-Beta 搜索、动态候选宽度和置换表
- 跳三、跳四等非连续棋形评分
- 真正取消进行中的大模型请求
- 原生版与网页版的版本信息展示
- 跨原生版、网页版和文档的版本一致性测试

### 改进

- 迁移到 Rust 2024 Edition，并显式处理异步临时值的生命周期
- 将大模型配置迁移到系统应用目录，并采用可靠的原子写入
- 强制大模型 API 使用 OpenRouter 官方 HTTPS 端点
- 加强跨平台 CI、错误处理和配置安全性

[Unreleased]: https://github.com/zhenghu/wuziqi_codex/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/zhenghu/wuziqi_codex/compare/v0.2.0-beta.1...v1.0.0
[0.2.0-beta.1]: https://github.com/zhenghu/wuziqi_codex/compare/2b2cf6e...v0.2.0-beta.1
