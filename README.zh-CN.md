[English](README.md) | 简体中文

# keep

`keep` 是一个用于本地开发的轻量进程管理工具。它按依赖顺序启动多个进程，等服务就绪
后再启动依赖项。项目启动后，可以在任意目录查看或停止进程。

`keep` 始终在前台运行，不需要守护进程，也不依赖 tmux、Overmind、OpenSSL 或其他
进程管理器。目前支持 macOS 和 Linux。

## 安装

需要 Rust 1.85+ 和 [just](https://github.com/casey/just)。克隆仓库并进入目录后运行：

```bash
cd keep
just install
keep --version
```

开发时可运行 `just check` 完成格式、静态检查和全部测试。

## 配置

配置既可以放在项目 Git 根目录的 `keep.yaml`，也可以放在
`~/.config/keep/*.yaml`。自动识别时优先使用本地 `keep.yaml`，没有时再匹配全局配置。

在项目目录中生成最小模板：

```bash
keep config init                    # 写入 ~/.config/keep/<项目名称>.yaml
keep config init --local            # 写入当前 Git 根目录的 keep.yaml
keep config init --project shop     # 显式指定项目名称
```

命令会优先记录 Git remote，没有 remote 时记录本地路径，并且不会覆盖已有文件。
生成后只需修改 `processes.app.command`。

显式传入 `--config` 时以指定配置为准；否则查找顺序为本地 `keep.yaml`、全局配置。

```yaml
version: 1

project:
  name: shop                  # 未配置 id 时，同时作为运行时 ID
  git:                        # 推荐：同一配置可用于不同 Git worktree
    - git@github.com:acme/shop.git

env_files:
  - .env

processes:
  database:
    command: docker compose up postgres
    readiness:
      type: tcp
      target: 127.0.0.1:5432
      interval: 500ms
      attempt_timeout: 1s
      startup_timeout: 30s

  migrate:
    command: ./scripts/migrate.sh
    mode: task
    depends_on:
      database: ready

  api:
    command: npm run dev
    color: red                  # 可选：突出重要进程
    log_directory: .keep/logs  # 可选：终端显示的同时追加到文件
    depends_on:
      database: ready
      migrate: completed_successfully
    readiness:
      type: http
      target: http://127.0.0.1:3000/health
      expected_status: 200
      startup_timeout: 30s
    restart:
      policy: on-failure
      max_attempts: 3
```

这个配置表示：

1. 启动 `database`，等 TCP 端口可连接。
2. 运行一次 `migrate`，等它成功退出。
3. 启动 `api`，等健康检查返回 HTTP 200。

普通进程默认是长期运行的 `service`；`mode: task` 表示执行成功后正常退出的一次性任务。
`readiness` 还支持 `tcp4`、`tcp6`、`https`、`unix`、`file` 和 `command`。

全部字段、默认值和约束见 [配置参数参考](docs/configuration.zh-CN.md)。

先检查配置：

```bash
keep config validate          # 检查当前项目选中的配置
keep config validate --all    # 检查全部全局配置
keep config resolve            # 查看当前目录会匹配哪个项目
```

## 使用

在项目目录启动全部进程。`keep start` 会留在前台并汇总日志，按 `Ctrl-C` 可停止整个项目：

```bash
cd ~/projects/shop
keep start
```

### 输出和日志

子进程的 stdout/stderr 会实时显示，并带进程名前缀，例如
`api | listening on :3000`。Python 等终端感知程序也能正常刷新输出。

进程名前缀在终端中使用稳定的颜色。每个进程可以通过 `color`、`log_directory` 和
`console` 配置颜色、本地日志和仅写文件模式；详见[日志颜色](docs/configuration.zh-CN.md#日志颜色)
和[日志落盘](docs/configuration.zh-CN.md#日志落盘)。

也可以显式指定配置，或只启动某个进程及其依赖：

```bash
keep start --config shop
keep start api
```

在另一个终端、任意目录管理运行中的项目：

```bash
keep ls                       # 查看所有项目和进程
keep status shop              # 查看项目详情
keep status shop/api          # 查看一个进程
keep restart shop/api         # 重启一个进程
keep restart api              # 进程名唯一时可省略项目
keep stop shop/api            # 停止一个进程
keep stop shop                # 停止整个项目
keep wait shop/api            # 阻塞等待进程变为 running（默认超时 5 分钟）
keep wait api -s stopped -t 30  # 等待其他状态，自定义超时秒数

keep stop --all               # 停止 keep 管理的所有项目
```

常用命令有短别名：`s`（start）、`l`（ls）、`ps`（status）、`st`（stop）、
`r`（restart）、`w`（wait）、`q`（quit）。

## Procfile 兼容模式

Procfile 不参与自动匹配，需要显式运行：

```bash
keep procfile start --file Procfile --project shop
keep procfile convert --file Procfile --project shop > shop.yaml
```

架构和开发计划见[设计文档](docs/README.zh-CN.md)。
