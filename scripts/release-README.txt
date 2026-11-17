批量重命名工具（rename-rust）发布产物
======================================

目录结构
--------
macos/
  批量重命名工具.app            双击即用的 macOS 应用（Apple Silicon）
  批量重命名工具-0.1.0-macos.zip  应用压缩包（便于拷贝）
bin/
  rename-rust-cli               命令行版本（单文件可执行）

系统要求
--------
- macOS 10.15+（Apple Silicon；Intel 需在 Intel 机器上重新构建）
- GUI 版为本地构建，未做公证/签名；如系统提示“无法验证开发者”，
  可在“系统设置 → 隐私与安全性”中允许，或右键 → 打开。

GUI 使用
--------
双击 批量重命名工具.app。选择文件夹 → 选择命名模式/填写规则 → 预览变更 →
勾选确认 → 执行重命名。支持撤销上次、深/浅色主题、搜索过滤。

CLI 使用
--------
  ./bin/rename-rust-cli <目录> [选项]

  # 预览（默认 dry-run，不改磁盘）
  ./bin/rename-rust-cli ~/Pictures --filter jpg,png --find IMG_ --replace PIC_

  # 确认后执行
  ./bin/rename-rust-cli ~/Pictures --filter jpg,png --find IMG_ --replace PIC_ --apply

  # 递归 + 序号命名（全局连续、补零 3 位）
  ./bin/rename-rust-cli ~/Photos -r -m seq --prefix Trip --pad 3 --scope global --apply

  # 模板命名 + JSON 输出（便于脚本消费）
  ./bin/rename-rust-cli ~/Downloads -m pattern --template '{date}_{name}' --json

  # 完整帮助
  ./bin/rename-rust-cli --help

从源码重新构建
--------------
  bash scripts/build-release.sh              # .app + CLI
  BUILD_DMG=1 bash scripts/build-release.sh  # 额外生成 .dmg
