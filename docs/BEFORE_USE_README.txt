GameOptimizer 使用说明 (v1.0.0)
======================================

用途：Windows 端 8 款热门游戏（三角洲行动/英雄联盟/CS2/绝地求生/无畏契约/
Apex Legends/Dota 2/守望先锋2）一键优化工具（硬件无感适配）。
安全约束：仅用官方 WinAPI 改进程属性；无注入、无内核 Hook；优先级上限 HIGH；
          每次应用自动快照（持久化，跨进程可回滚）；电源切换默认关闭。

【0】自检（复制到目标机后先跑一次）
      双击 自检.cmd —— 机制自检输出 PASS（真实进程的优先级/亲和性 设置+恢复），
      再打印硬件指纹与预设。

【1】图形界面（推荐）
      gopt_gui.exe —— 选择游戏 → 应用优化；可填"代启动路径"让工具拉起游戏；
      "启用电源方案切换 (Pro)" 勾选项需 Pro 授权 + 管理员。

【2】命令行
      gopt_cli --version / status / fingerprint
      gopt_cli apply cs2                    游戏运行中 attach
      gopt_cli apply cs2 --game-exe "<路径>"  代启动
      gopt_cli rollback / rollback-all
      gopt_cli license status / activate <授权码>

【3】授权（免费版 = 优先级+亲和性；Pro 增加 电源/帧延迟/工作集）
      授权码由开发者用 `gopt_cli license gen <本机指纹哈希> Pro [到期]` 生成，绑定本机。

注意：64 位 Windows；无注入、无内核 Hook，可放心用于带反作弊的游戏。
若 Windows 弹 SmartScreen，选"更多信息 → 仍要运行"。
