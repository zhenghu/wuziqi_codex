# 更新日志

本项目的版本号遵循[语义化版本](https://semver.org/lang/zh-CN/)，重要变更记录在此文件。

## [Unreleased]

### 新增

- OpenRouter 请求失败后显示失败原因，最多自动尝试 3 次，再降级到战术搜索

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
