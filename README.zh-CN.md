# herdr


<p align="center">
  <img src="assets/logo.png" alt="herdr" width="100" />
</p>

<p align="center">
  <a href="https://herdr.dev">herdr.dev</a> · <a href="#安装">安装</a> · <a href="https://herdr.dev/zh-cn/docs/quick-start/">快速开始</a> · <a href="https://herdr.dev/zh-cn/docs/">文档</a> · <a href="#赞助">赞助</a>
</p>

<p align="center">
  <a href="README.md">English</a> · 简体中文
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-666666?labelColor=333333" alt="Apache 2.0 license" /></a>
  <a href="https://github.com/herdrdev/herdr/releases"><img src="https://img.shields.io/github/downloads/herdrdev/herdr/total?labelColor=333333&color=666666" alt="total GitHub release downloads" /></a>
  <a href="https://github.com/herdrdev/herdr/stargazers"><img src="https://img.shields.io/github/stars/herdrdev/herdr?labelColor=333333&color=666666&logo=github" alt="GitHub stars" /></a>
  <a href="https://github.com/herdrdev/herdr/releases/latest"><img src="https://img.shields.io/github/v/release/herdrdev/herdr?label=release&labelColor=333333&color=666666" alt="latest stable release" /></a>
  <a href="https://formulae.brew.sh/formula/herdr"><img src="https://img.shields.io/homebrew/v/herdr?label=homebrew&labelColor=333333&color=666666" alt="Homebrew version" /></a>
  <a href="https://x.com/herdrdev"><img src="https://img.shields.io/badge/follow-%40herdrdev-000000?logo=x&logoColor=white" alt="follow @herdrdev on X" /></a>
</p>

---

https://github.com/user-attachments/assets/043ec09f-4bdd-41d5-aee0-8fda6b83e267

**智能体复用器，住在你的终端里。**

- **每个智能体一目了然**——`blocked`、`working`、`done`。真实的终端视图，而不是包装过的转述。
- **分离后智能体继续运行**——从任意终端重新连接，或通过 ssh。会话在重启后依然保留。
- **智能体也能使用 herdr**——纯 socket api：智能体可以创建窗格、读取输出、互相等待。[智能体技能 →](https://herdr.dev/zh-cn/docs/agent-skill/)
- **键盘和鼠标都是一等公民**——tmux 风格的前缀键，*以及*点击、拖动、分割。按当下的场景选择，而不是被工具锁死。
- **插件**——扩展窗格和工作流。[浏览插件市场 →](https://herdr.dev/plugins/)
- **单个 rust 二进制，没有 electron**——运行在你已经在用的任何终端里。

---

## 安装

```bash
curl -fsSL https://herdr.dev/install.sh | sh
```

或者 `brew install herdr` · `mise use -g herdr` · Windows 测试版：`powershell -ExecutionPolicy Bypass -c "irm https://herdr.dev/install.ps1 | iex"` · [二进制文件](https://github.com/herdrdev/herdr/releases)

然后在工作所在的目录启动它：

```bash
herdr
```

运行你的智能体、分割窗格，然后安心离开。`ctrl+b q` 分离，`herdr` 重新连接。[快速开始 →](https://herdr.dev/zh-cn/docs/quick-start/)

## 文档

所有文档都在 [herdr.dev/docs](https://herdr.dev/zh-cn/docs/)：[快速开始](https://herdr.dev/zh-cn/docs/quick-start/) · [核心概念](https://herdr.dev/zh-cn/docs/concepts/) · [受支持的智能体](https://herdr.dev/zh-cn/docs/agents/) · [键盘](https://herdr.dev/zh-cn/docs/keyboard/) · [配置](https://herdr.dev/zh-cn/docs/configuration/) · [会话状态](https://herdr.dev/zh-cn/docs/session-state/) · [远程访问](https://herdr.dev/zh-cn/docs/persistence-remote/) · [集成](https://herdr.dev/zh-cn/docs/integrations/) · [插件](https://herdr.dev/zh-cn/docs/plugins/) · [socket api](https://herdr.dev/zh-cn/docs/socket-api/)

## 赞助

herdr 以全职方式公开开发。赞助将直接资助开发、稳定性，以及通往真正的智能体运行时之路。

### 金牌

<a href="https://terminaltrove.com/"><img src="assets/sponsors/terminal-trove.png" alt="Terminal Trove" width="200" /></a>

[**→ 成为赞助者**](https://github.com/sponsors/ogulcancelik) · 企业/合作：hey@herdr.dev · 档位说明见 [SPONSORS.md](./SPONSORS.md)。谢谢 🐑

## 智能体须知

如果你是协助本仓库的 AI 智能体：在改动代码前阅读 [`AGENTS.md`](./AGENTS.md)，在创建 issue 或 PR 前阅读 [`CONTRIBUTING.md`](./CONTRIBUTING.md)。

## 开发

```bash
git clone https://github.com/herdrdev/herdr
cd herdr
cargo build --release

just test        # 单元测试
just check       # 格式检查、测试和维护性检查
```

## 许可证

herdr 基于 [Apache License 2.0](LICENSE) 许可证发布。
