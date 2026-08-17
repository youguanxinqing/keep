# ADR：进程输出、本地日志与一次性任务

状态：已采纳并实现。

## 背景

`keep` 需要同时管理多个本地进程，并在一个前台终端中汇总它们的输出。直接使用 pipe
会让 Python 等终端感知程序切换到块缓冲，造成日志延迟；用户也可能希望把部分进程的
输出保存在本地。迁移、代码生成等运行后退出的命令，还需要和长期运行的服务使用不同
的生命周期语义。

首版不提供交互式终端、日志服务或独立守护进程，因此这些能力由前台运行的 `keep`
进程负责，并保持配置简单。

## 决定

### 前台输出

每个子进程通过 `sh -c` 启动，stdin 关闭，stdout 和 stderr 分别连接只输出的原生
PTY。读取线程把完整行送入有界队列，再由单个写入线程按收到的顺序写回 `keep` 的
stdout 或 stderr。

终端输出使用 `<进程名> | <内容>` 前缀。进程名就是配置中 `processes` 下的键名，在
项目内唯一，因此不再重复打印项目名。前缀在终端中使用稳定颜色；输出重定向到文件或
管道，或设置 `NO_COLOR=1` 时，不写入颜色控制符。子进程自己产生的 ANSI 序列保持不变。

`keep` 退出前关闭队列并等待写入线程排空，避免快速任务丢失末尾输出。完整的交互式
stdin、终端 resize、attach 和 tmux 后端不属于首版。

### 本地日志

需要保存输出的进程使用两个独立字段：

```yaml
processes:
  api:
    command: npm run dev
    log_directory: .keep/logs
    console: true
```

- `log_directory` 为每个进程创建 `<directory>/<process>.log`。
- stdout 和 stderr 按收到的顺序追加到同一个文件。
- 文件保存子进程原始字节，不包含 `keep` 添加的前缀和颜色。
- 相对目录从项目根目录解析；目录不存在时自动创建。
- `console` 默认为 `true`。设置为 `false` 时必须同时配置 `log_directory`。
- 运行中写文件失败时，`keep` 报错并把后续输出回退到终端。
- 进程重启或再次运行 `keep start` 时继续追加，不覆盖已有日志。

`keep` 不负责日志轮转、压缩、保留期限或远程收集。这些需求交给现有系统工具处理。

### 一次性任务

一次性命令使用现有的 `mode: task`：

```yaml
processes:
  migrate:
    command: ./scripts/migrate.sh
    mode: task

  api:
    command: npm run dev
    depends_on:
      migrate: completed_successfully
```

成功的 `task` 进入 `completed`，不会停止仍在运行的 `service`。依赖条件
`completed_successfully` 只接受状态码为 0 的 `task`；失败的 `task` 会阻止依赖项启动。
`task` 不能配置就绪检测。所有启用进程都是 `task` 时，最后一个任务成功完成后
`keep start` 正常退出。

不增加 `once` 别名，也不采用 Overmind 的 `can-die`。前者会为同一状态机引入第二种
写法；后者只允许进程退出，不能表达可供依赖项判断的“成功完成”。

### 进程后端

首版继续直接启动 Unix 进程组，不引入 tmux 或新的运行时依赖。只有在实现完整交互式
PTY 或 attach 时，才需要抽取新的后端接口；配置、依赖图、运行时注册和控制协议
不随后端改变。

## 后果

- 终端感知程序能正常刷新，同时保留 stdout/stderr 的区分。
- 前台输出和本地日志共用一个写入点，不需要额外读取或轮询日志文件。
- 有界队列会对输出施加背压，避免日志无限占用内存。
- 已有 `keep` 进程的输出仍属于启动它的终端；其他终端发出的控制命令不会接管日志。
- 本地日志是开发期便利功能，不是完整的日志归档系统。

## 验证范围

端到端测试覆盖以下行为：

- 多个 `service` 的 stdout/stderr 前缀和目标流；
- 快速 `task`、非 UTF-8、超长行和无结尾换行的完整排空；
- `log_directory` 的目录创建、追加写入和无前缀内容；
- `console: false`、日志写入失败和终端回退；
- `task` 成功、失败以及 `completed_successfully` 依赖。

用户可见配置以[配置参考](configuration.zh-CN.md)为准；生命周期和整体约束以
[产品规范](product-spec.md)为准。
