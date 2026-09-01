GameOptimizer 使用说明 (v1.0.8)
======================================

用途：Windows 端 8 款热门游戏（三角洲行动/英雄联盟/CS2/绝地求生/无畏契约/
Apex Legends/Dota 2/守望先锋2）一键优化工具（硬件无感适配）。
安全约束：仅用官方 WinAPI 改进程属性；无注入、无内核 Hook；优先级上限 HIGH；
          每次应用自动快照（持久化，跨进程可回滚）；电源切换默认关闭（功能本身免费）。

【0】自检（复制到目标机后先跑一次）
      双击 自检.cmd —— 机制自检输出 PASS（真实进程的优先级/亲和性 设置+恢复），
      再打印硬件指纹与预设。

【1】图形界面（推荐）
      gopt_gui.exe —— 左栏功能：
        总览     硬件信息 + 实时 CPU 负载 + 「一键性能优化」（自动检测并优化
                 所有运行中的支持游戏；无游戏时执行系统级优化；无注入/无 Hook）
        游戏优化 选择游戏 → 应用优化；可填"代启动路径 + 参数"让工具代启动；
                 「保存游戏设置」持久化；选中游戏即显示预设摘要
        系统调优 高性能/平衡档（电源+处理器档+调度优先级，需管理员；可恢复）
                「清理临时文件」（24 小时内文件保留，安全策略）
        进程     查看运行中的支持游戏与当前优先级；「提升优先级/恢复正常」
        启动项   开机启动项启用/禁用（禁止=改名保留并备份；可一键还原）
      优化时会显示深色科技感流程面板：步骤节点 + 每步耗时 + 百分比动画。
      界面右上角可切换 中文/English。

【2】命令行
      gopt_cli --version / status / fingerprint
      gopt_cli apply cs2                       游戏运行中 attach
      gopt_cli apply cs2 --game-exe "<路径>"    代启动
      gopt_cli optimize [游戏|system]          一键：优化全部运行中的支持游戏
      gopt_cli rollback / rollback-all         回滚（跨进程）
      gopt_cli tune [high|balanced|restore]    系统调优（电源/处理器档/调度优先级）
      gopt_cli startup list|disable|enable|restore   启动项管理
      gopt_cli prio <pid> high|above|normal|below|idle  进程优先级
      gopt_cli clean                           清理临时文件（24 小时内保留）
      gopt_cli list                            运行中支持游戏概览
      gopt_cli license status / activate <码>  授权（所有功能免费，授权可选）

【3】授权（所有功能免费；仅机器指纹用于潜在企业/捐赠场景）
      所有优化项（优先级/亲和性/工作集/电源/帧延迟）免费开放，无任何门控。
      如需绑定授权，用 `gopt_cli fingerprint` 获取本机指纹哈希。

注意：64 位 Windows；无注入、无内核 Hook，可放心用于带反作弊的游戏。
若 Windows 弹 SmartScreen，选"更多信息 → 仍要运行"。
