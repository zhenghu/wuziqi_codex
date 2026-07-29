# 更新日志

本项目的版本号遵循[语义化版本](https://semver.org/lang/zh-CN/)，重要变更记录在此文件。

## [Unreleased]

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

[Unreleased]: https://github.com/zhenghu/wuziqi_codex/compare/v0.2.0-beta.1...HEAD
[0.2.0-beta.1]: https://github.com/zhenghu/wuziqi_codex/compare/2b2cf6e...v0.2.0-beta.1
