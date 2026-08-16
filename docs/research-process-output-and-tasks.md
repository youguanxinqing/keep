# 进程输出、日志与一次性任务调研

本文只研究前台聚合输出、可选日志落盘、一次性命令生命周期，依据当前 `keep`、仓库内 Overmind/dockerize 源码和 Kubernetes 官方语义。

## 结论

1. `keep` 已经会捕获 stdout/stderr，并以 `<进程名> | <内容>` 输出。实际复现确认
   pipe 会让 Python 等程序默认缓冲；现已决定使用轻量的 output-only native PTY。
2. `keep` 使用每进程一个可选字段 `log_directory`，默认保留终端输出，同时把该进程
   的 stdout/stderr 按收到的顺序追加到同一个日志文件；`console: false` 可以只保留
   文件输出。
3. 一次性命令已经由 `mode: task` 表达。保留这个名字和现有
   `completed_successfully` 依赖条件，不增加 `once` 别名。
4. 首版继续使用直接进程 backend；启用现有 `nix` 的 PTY 能力，但不引入 tmux、
   交互式 attachment、日志服务或新的第三方组件。

## 1. 前台 stdout/stderr

### 当前行为

`keep start` 首次启动项目时是前台 supervisor。每个命令通过 `sh -c` 启动，stdin
设为 null，stdout/stderr 分别连接 output-only PTY；读取线程按行送入容量为 128 的
有界队列，再由单个 writer 分别写回 `keep` 的 stdout/stderr。supervisor 退出前会关闭
队列并等待 writer 排空。前缀使用 YAML `processes` 下的 key，例如：

```text
api | listening on 127.0.0.1:3000
worker | processing job 42
```

这个 process key 就是最稳定的 APP 标签：它在项目内必填且唯一。一个前台 supervisor
只管理一个项目，因此默认不再重复打印 `project.name`。

终端输出时进程名前缀带稳定颜色；被重定向到文件或管道时不输出颜色。读取逻辑保留
非 UTF-8 字节，并会为没有换行的最后一段补换行。

来源：

- `../src/supervisor.rs:325-385`：子进程 PTY 和两个输出读取线程。
- `../src/supervisor.rs:1005-1050`：进程名前缀、终端颜色、逐行读取和写回目标流。
- `../tests/e2e_lifecycle.rs:524-562`：非 UTF-8 输出的端到端覆盖。
- `../docs/product-spec.md:139-160`：前台聚合输出和 output-only PTY 约束。

### 为什么用户仍可能看不到或延迟看到输出

当前实现存在两个明确边界：

1. **已有 supervisor 的输出仍属于原终端。** 项目已经运行时，另一个终端执行
   `keep start <process>` 只通过 Unix socket 发送启动请求并立即返回；控制协议没有
   日志流。因此新终端不会接管该进程输出。
   来源：`../src/cli.rs:305-346`、`../src/supervisor.rs:609-623`。
2. **修复前的 pipe 不是 TTY。** Python 的默认 `print` 已实际复现为块缓冲，导致
   长期运行进程看不到日志。output-only PTY 让程序恢复终端刷新行为，同时不引入
   tmux 或交互式 stdin。
   来源：`../docs/product-spec.md:146-157`、`../third-party/overmind/README.md:20`、`:48`。

### Overmind 可借鉴的设计

Overmind 让每个进程运行在独立 tmux window 中，通过 tmux control mode 把 pane 输出
映射回进程。每一行经过一个统一输出器：进程名按最长名称右侧补齐、添加颜色和
` | `，再由单个 writer 串行写到终端。它的 channel 容量固定为 128，`Stop` 会关闭
channel 并等待 writer 排空。

来源：

- `../third-party/overmind/start/tmux.go:69-105`、`:125-190`：tmux control mode 与 pane 到进程的映射。
- `../third-party/overmind/start/process.go:161-169`：按行交给统一输出器。
- `../third-party/overmind/start/multi_output.go:15-77`、`:101-132`：有界 channel、drain、对齐前缀和单 writer。
- `../third-party/overmind/utils/utils.go:86-115`：避免普通 Scanner 长行上限的读取器。
- `../third-party/overmind/README.md:40-51`：tmux/PTY 输出是 Overmind 的明确取舍。

`keep` 借鉴的是“有界队列、统一 writer、停止时排空”，不是 tmux 本身。supervisor
现在持有输出 multiplexer，在所有子进程 PTY 读完后关闭队列并等待 writer 完成，
从而保住短任务的尾部输出，也为日志 tee 提供唯一写入点。名称补齐只是外观选择，
当前不复制。

当前只使用 output-only PTY。完整交互、stdin 转发、终端 resize 和 attach 仍不属于首版。

### 不采用 dockerize 的输出模型

dockerize 只有一个主命令，直接继承父进程 stdin/stdout/stderr，并不做多进程聚合或
添加进程名。它的 `-stdout`/`-stderr` 是把应用已经写入的文件 tail 回终端，方向与
“捕获子进程输出并落盘”相反。

来源：

- `../third-party/dockerize/exec.go:14-23`：主命令直接继承标准流。
- `../third-party/dockerize/README.md:6-22`、`:73-126`，`../third-party/dockerize/main.go:247-258`、`:371-380`，
  `../third-party/dockerize/tail.go:13-69`：tail 文件的接口、启动和 follow/reopen。

因此没有必要把文件监听、轮询或 reopen 机制移植到 `keep`；在现有 PTY 读取点 tee
即可。

## 2. 日志落盘

### 当前行为

配置的 `ProcessConfig` 支持 `log_directory`。日志 tee 复用统一输出 writer，不引入
额外 tailer；轮转、压缩、保留期限和远程日志仍不属于 v1。

来源：`../src/config.rs:320-340`、`:394-459`，`../docs/product-spec.md:146-154`。

### 两个最小接口候选

| 候选 | 示例 | 优点 | 代价 |
| --- | --- | --- | --- |
| A. `log_directory` + `console` | `log_directory: .keep/logs` | 两个正交字段；每进程一个日志文件，默认保留终端 | 不能自定义文件名 |
| B. `output` 对象 | `output: { console: true, file: .keep/logs/api.log }` | 可选择 console 和文件 | 配置面更大；合并文件需要定义 stdout/stderr 标记与顺序 |

采用 **A**。当前需求不需要自定义文件名，先增加 `output` 对象会扩大配置接口。

### 推荐配置和语义

```yaml
version: 1

project:
  name: shop

processes:
  migrate:
    command: ./scripts/migrate.sh
    mode: task
    log_directory: .keep/logs

  api:
    command: npm run dev
    log_directory: .keep/logs
    depends_on:
      migrate: completed_successfully
```

规则：

- 不配置 `log_directory`：只按现有行为输出到前台终端。
- 配置后：仍输出到前台，同时把 stdout/stderr 按收到的顺序 tee 到
  `<directory>/<process>.log`。
- 配置 `console: false`：不再把该进程的 stdout/stderr 写到终端；为避免静默丢失输出，
  此时必须配置 `log_directory`；运行中写文件失败则回退到终端。
- 相对路径从项目根目录解析；目录不存在时递归创建。
- 文件保存子进程原始字节，不写进程名前缀和 keep 自己的颜色；文件名标识进程。
- 文件采用 append；同一 supervisor 内的进程重启以及下一次 `keep start` 都继续
  追加。无法创建目录或打开文件时，该进程不应静默启动。
- 首版不支持自定义文件名、轮转、压缩、保留期限和远程日志。

这只是开发期日志 tee，不是完整日志归档系统。需要控制磁盘占用时先使用系统现有的
logrotate；只有跨平台体验确实不足时，再增加 keep 自己的 rotation。

## 3. 一次性进程

### 当前行为与根因

`mode` 已有两个值：默认 `service` 和显式 `task`。

- `service` 被定义为长期服务。无论退出码是否为 0，只要没有命中 restart policy，
  都会产生 `UnexpectedExit`，然后 supervisor 停止项目。所以把一次性命令省略
  `mode` 后出现“整个 keep 挂掉”是类型配置不符，不是缺少生命周期模型。
- `task` 退出 0 后进入 `completed`，不会关闭其他 service；非零退出在用尽 restart
  policy 后使项目失败。
- 依赖者可以使用 `completed_successfully`，只在 task 成功后启动。
- task 不允许 readiness；如果全部启用的进程都是 task，最后一个成功完成后
  `keep start` 正常退出。

来源：

- `../src/config.rs:320-355`、`:427-455`：类型、依赖条件和配置校验。
- `../src/supervisor.rs:379-410`、`:454-513`、`:826-834`：task/service 的运行、退出和完成状态。
- `../tests/e2e_runtime.rs:130-208`、`../tests/e2e_control.rs:215-286`：task 依赖的端到端场景。

这与 Kubernetes 的概念一致：Job 表示运行到完成的任务；Init Container 成功后才开始
下一个 init container 或主容器，并且不支持 readiness probe。keep 不需要复制其
对象，只需保留已经具备的轻量语义。

官方参考：

- [Kubernetes Jobs](https://kubernetes.io/docs/concepts/workloads/controllers/job/)
- [Kubernetes Init Containers](https://kubernetes.io/docs/concepts/workloads/pods/init-containers/)

### 不增加 `once`

不建议让 `once` 成为第三个 enum 值或 `task` 别名。两个拼法表达同一状态机会增加
文档、校验、序列化和测试分支，却没有增加能力。README 直接说明一次性命令使用
`mode: task` 即可。

Overmind 的 `can-die` 也不应照搬。它允许指定进程退出而不打断其他进程，但不把退出 0
建模为可供依赖判断的“成功完成”，不适合迁移、代码生成这类任务。

来源：`../third-party/overmind/README.md:223-256`、
`../third-party/overmind/start/process.go:171-211`、
`../third-party/overmind/start/command.go:210-255`。

## 建议实施顺序

1. [已完成] 把输出 reader 纳入 supervisor 生命周期：有界队列、单 writer、EOF
   排空，并补短 task 尾部输出回归测试。
2. [已完成] 在同一个输出 writer 增加 `log_directory` tee，不另建 tailer 或日志服务。
3. [已完成] 更新 README/configuration：说明默认终端输出、日志文件名，以及 `mode: task`。
4. 不改 task 状态机；只补缺失的用户文档和必要回归测试。

## 端到端测试清单

每项用户可见行为都应从编译后的 `keep` 二进制测试：

1. 两个 service 同时写 stdout/stderr，终端每行都带正确的进程名前缀，原始流
   仍分别进入 `keep` 的 stdout/stderr。
2. task 快速写多行、非 UTF-8、超长行以及无结尾换行后退出；`keep start` 返回前所有
   输出都已排空。
3. 配置 `log_directory` 后终端输出仍存在，并为两个进程各生成一个合并日志文件；
   文件内容不含 keep 前缀。
4. 配置 `console: false` 后只生成合并日志文件，终端不出现该进程的 stdout/stderr。
5. 日志目录自动创建；进程 restart 后追加而非覆盖；不可写路径在启动子进程前明确
   报错。
6. 成功 task 不停止正在运行的 service；依赖它的 service 只在
   `completed_successfully` 后启动。
6. 失败 task 阻止依赖启动并使 `keep start` 失败；全部为成功 task 时命令以 0 退出。
7. `mode: task` 配 readiness、service 被 `completed_successfully` 依赖，继续在配置
   校验阶段拒绝。

现有非 UTF-8、task 依赖和配置校验测试可以扩展；不需要为同一语义新增第二套测试框架。
