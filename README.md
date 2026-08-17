# PDF Watermark Remover (Rust)

[![Build Windows Release](https://github.com/zergfff/pdf-wm-remover/actions/workflows/build.yml/badge.svg)](https://github.com/zergfff/pdf-wm-remover/actions/workflows/build.yml)

PDF 去水印工具 —— Rust 版重构。核心逻辑与
[Smart-PDF-Watermark-Remover](https://github.com/zergfff/Smart-PDF-Watermark-Remover)
（Python/PyQt6）一致，但删除方式完全不同：

**内容流级（content-stream level）删除**，而不是"红色矩形遮盖"（redaction）。

## 解决的问题

| 方案 | 问题 |
|---|---|
| 红色遮盖 Redaction | 斜 45° 水印的 bbox 是横跨半页的大矩形，`apply_redactions` 会把矩形内所有正文一起抹掉 |
| bbox 精确匹配 | 旋转水印的 bbox 稍有舍入差异就匹配失败（"删不了"） |

Rust 版做法：

1. 用 `lopdf` 解析每页 Contents 内容流为操作序列（operations）
2. 按 `BT...ET` 切分文本块，提取块内 `Tj`/`TJ` 绘制的字符串
3. **分析**：跨页聚类重复文本块（文本内容 + 字号），识别疑似水印候选
4. **删除**：只删命中关键词的 `BT...ET` 块，正文/图形完全不碰
5. **权限**：保存时移除 `/Encrypt`，彻底去掉禁打印/复制/修改等权限限制

## 二进制

| 组件 | 说明 |
|---|---|
| `pdf-wm-remover.exe` | 命令行版（CLI） |
| `pdf-wm-remover-gui.exe` | 图形界面版（egui，Windows 原生窗口） |

GitHub Actions 自动编译，产物在
[Releases](../../releases) 或每次 CI 的 Artifacts 中下载。

## CLI 用法

```bash
# 分析 PDF，列出跨页重复的文本候选（可能是水印）
pdf-wm-remover analyze input.pdf

# 删除水印并保存（可多个关键词，大小写不敏感子串匹配）
pdf-wm-remover remove input.pdf -o output.pdf -k "C2 - Confidential" -k "exclusive use of Sichuan"
```

带权限限制（加密、空密码）的 PDF 自动解锁处理。

## 本地编译

```bash
# CLI only
cargo build --release --no-default-features --features cli

# GUI only
cargo build --release --no-default-features --features gui

# 全部
cargo build --release
```

## 依赖

- [lopdf](https://github.com/J-F-Liu/lopdf) — PDF 解析/加密处理（支持 AES-128 V4 R4 自动解密）
- [clap](https://clap.rs) — CLI 参数解析
- [egui/eframe](https://github.com/emilk/egui) — GUI
- [rfd](https://github.com/PolyMeilex/rfd) — 原生文件对话框

## License

MIT