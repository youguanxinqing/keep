# keep 配置参考

原生配置文件放在 `~/.config/keep/*.yaml` 或 `~/.config/keep/*.yml`。一个文件描述
一个项目；也可以用 `KEEP_CONFIG_DIR` 改变配置目录。

修改后先检查配置：

```bash
keep config validate --all
keep config resolve
```

`keep` 会拒绝未知字段、重复项目 ID、无效依赖和依赖环，避免拼写错误被静默忽略。

## 创建最小配置

```bash
keep config init
keep config init --project shop
keep config init --local
```

默认写入 `~/.config/keep/<项目 ID>.yaml`。`--local` 改为在当前 Git 根目录写入
`keep.yaml`，启动时使用提示中的 `keep start --config <文件>`。项目 ID 默认取 Git
根目录或当前目录名称，也可以用 `--project` 指定。

生成器优先写入规范化且不含凭据的 Git remote，便于同一配置跨 worktree 使用；
没有 remote 时写入当前项目的绝对 `path`。目标文件存在时会拒绝覆盖。

版本 1 真正的最小配置只有这些字段：

```yaml
version: 1
project:
  id: shop
processes:
  app:
    command: npm run dev
```

其余字段全部按需使用。版本号保证未来可以安全升级格式；项目 ID 用于全局
`ls/stop/status`；进程名和 command 是启动进程所必需的信息。配置没有引入
`depends_on` 列表短写、readiness 字符串短写等第二套语法，因为减少几行 YAML
却会增加需要记忆的规则和解析分支。

## 完整结构

下面的骨架列出了版本 1 的所有配置字段。没有需要的可选字段可以直接删除。

```yaml
version: 1

project:
  id: shop
  name: Shop
  path: ~/projects/shop
  git:
    - git@github.com:acme/shop.git
  aliases:
    - shop-api

env_files:
  - .env

defaults:
  stop:
    signal: TERM
    timeout: 5s
  restart:
    policy: on-failure
    backoff: 1s
    max_attempts: 5

processes:
  database:
    command: docker compose up postgres
    readiness:
      type: tcp
      target: 127.0.0.1:5432

  migrate:
    command: ./scripts/migrate.sh
    mode: task
    depends_on:
      database: ready

  api:
    command: npm run dev
    mode: service
    working_directory: services/api
    env_files:
      - services/api/.env
    env:
      PORT: "3443"
    depends_on:
      database: ready
      migrate: completed_successfully
    readiness:
      type: https
      target: https://127.0.0.1:${PORT}/health
      interval: 1s
      attempt_timeout: 1s
      startup_timeout: 30s
      success_threshold: 1
      method: GET
      headers:
        Authorization: Bearer ${DEV_TOKEN}
      expected_status: 200
      tls_ca: certs/development-ca.pem
    restart:
      policy: on-failure
      backoff: 1s
      max_attempts: 5
    stop:
      signal: TERM
      timeout: 5s
```

## 顶层字段

| 字段 | 类型 | 必填 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| `version` | 整数 | 是 | 无 | 配置格式版本，当前只能是 `1`。 |
| `project` | 对象 | 是 | 无 | 项目标识和自动匹配规则。 |
| `env_files` | 字符串列表 | 否 | `[]` | 所有进程共同加载的环境文件。 |
| `defaults` | 对象 | 否 | `{}` | 全项目的停止和重启默认值。 |
| `processes` | 对象 | 是 | 无 | 进程名到进程配置的映射，至少包含一个进程。 |

## `project`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| `id` | 字符串 | 是 | 无 | 稳定且全局唯一的项目 ID，也是 `keep stop shop` 中的 `shop`。 |
| `name` | 字符串 | 否 | `id` | 展示名称，也可用于较弱的目录名匹配。 |
| `path` | 字符串 | 否 | 无 | 项目目录。支持绝对路径、`~/...`；其他相对路径从家目录解析。 |
| `git` | 字符串列表 | 否 | `[]` | 可匹配的 Git remote。常见 SSH 和 HTTPS 写法会标准化后比较。 |
| `aliases` | 字符串列表 | 否 | `[]` | 额外的项目目录名或 Git 根目录名。 |

`id` 最长 48 个字符，只能使用 ASCII 字母、数字、`_` 和 `-`。同一配置目录内
不能出现重复 ID。

自动匹配顺序为：`--config` 显式指定、`project.path`、`project.git`，最后是
`id`、`name` 或 `aliases` 与目录名匹配。同一级出现多个候选时会报错，不会猜测。

### Git worktree

需要在不同 worktree 间复用一份配置时，推荐省略 `path`，使用 `git`：

```yaml
project:
  id: shop
  name: Shop
  git:
    - git@github.com:acme/shop.git
```

在任意 worktree 中执行 `keep start`，当前 Git 根目录会成为项目目录。同一个
`project.id` 目前只能同时运行一个实例；并行启动多个 worktree 需要不同的 ID。

## `defaults`

### `defaults.stop`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| `signal` | 字符串 | 否 | `TERM` | 优雅停止时发送给整个进程组的信号。 |
| `timeout` | 时长 | 否 | `5s` | 等待退出的最长时间，超时后发送 `KILL`。 |

支持 `ABRT`、`HUP`、`INT`、`KILL`、`QUIT`、`STOP`、`TERM`、`USR1`、`USR2`，
也可以使用 `SIGTERM` 这样的 `SIG` 前缀。

### `defaults.restart`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| `policy` | 枚举 | 否 | `never` | `never`、`on-failure` 或 `always`。 |
| `backoff` | 时长 | 否 | `1s` | 两次启动之间的等待时间。 |
| `max_attempts` | 非负整数 | 否 | 不限制 | 最大重试次数，不包含第一次启动。 |

`on-failure` 只在非零退出、信号退出或就绪检测失败时重启；`always` 也会在 service
正常退出后重启。成功完成的 task 不会重启。达到重试上限后，进程进入失败状态。

进程自己的 `stop` 或 `restart` 对象会整体替代对应的 `defaults` 对象，而不是逐字段
合并。例如进程写了 `restart: { policy: always }`，其 `max_attempts` 就是不限制。

## `processes.<name>`

`<name>` 是进程名，格式限制与 `project.id` 相同，并且在项目内唯一。YAML 中的声明
顺序用于稳定日志顺序，以及同时满足条件时的启动顺序。

| 字段 | 类型 | 必填 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| `command` | 字符串 | 是 | 无 | 使用 POSIX `sh -c` 执行的非空命令。 |
| `mode` | 枚举 | 否 | `service` | `service` 是长期服务；`task` 是成功退出的一次性任务。 |
| `depends_on` | 对象 | 否 | `{}` | 依赖进程名到依赖条件的映射。 |
| `readiness` | 对象 | 否 | 无 | 服务的就绪检测；`task` 不能配置此字段。 |
| `restart` | 对象 | 否 | `defaults.restart` | 该进程的重启设置。字段与 `defaults.restart` 相同。 |
| `stop` | 对象 | 否 | `defaults.stop` | 该进程的停止设置。字段与 `defaults.stop` 相同。 |
| `working_directory` | 字符串 | 否 | 项目根目录 | 命令工作目录；相对路径从项目根目录解析。 |
| `env_files` | 字符串列表 | 否 | `[]` | 仅该进程加载的环境文件；相对路径从项目根目录解析。 |
| `env` | 字符串映射 | 否 | `{}` | 仅该进程使用的环境变量，值必须是字符串。 |

命令继承启动 `keep` 时的系统环境，然后按以下顺序覆盖同名变量：

1. 顶层 `env_files`，按声明顺序加载。
2. 进程 `env_files`，按声明顺序加载。
3. 进程 `env`。

环境文件不存在或内容无效时，进程不会启动。`command` 由 shell 展开环境变量；
就绪检测的 `target`、`tls_ca` 和 header 值支持 `${NAME}`。

## `depends_on`

```yaml
depends_on:
  database: ready
  migrate: completed_successfully
```

| 条件 | 含义 |
| --- | --- |
| `ready` | 依赖已通过就绪检测；未配置检测时，成功启动即视为 ready。 |
| `completed_successfully` | 依赖是 `mode: task`，并且已经以状态码 0 退出。 |

依赖必须存在，不能依赖自身，也不能形成环。`completed_successfully` 只能指向
`task`。执行 `keep start api` 时，`api` 的全部传递依赖也会自动启动。

## `readiness`

### 通用字段

| 字段 | 类型 | 必填 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| `type` | 枚举 | 是 | 无 | `tcp`、`tcp4`、`tcp6`、`http`、`https`、`unix`、`file` 或 `command`。 |
| `target` | 字符串 | 是 | 无 | 检测目标，不能为空；格式由 `type` 决定。 |
| `interval` | 时长 | 否 | `1s` | 两次检测之间的等待时间。 |
| `attempt_timeout` | 时长 | 否 | `1s` | 单次 TCP、HTTP(S) 或 command 检测的最长时间。 |
| `startup_timeout` | 时长 | 否 | `30s` | 服务通过检测的总时限。 |
| `success_threshold` | 正整数 | 否 | `1` | 连续成功多少次才视为 ready；失败会重新计数。 |
| `method` | 字符串 | 否 | `GET` | HTTP(S) 请求方法。其他检测类型忽略。 |
| `headers` | 字符串映射 | 否 | `{}` | HTTP(S) 请求头；值支持 `${NAME}`。 |
| `expected_status` | 整数 | 否 | 任意 `200..299` | 指定唯一可接受的 HTTP 状态码，范围为 100 到 599。 |
| `tls_ca` | 字符串 | 否 | 系统根证书 | HTTPS 使用的 PEM CA 文件；相对路径从项目根目录解析。 |

所有时长都必须大于零。支持 `500ms`、`2s`、`1m 30s` 等 humantime 格式。
就绪检测超过 `startup_timeout` 后会停止该进程；依赖它的进程保持 blocked。
如果启用了重启策略，就绪检测失败也会计入重试。

配置 `tls_ca` 后，该文件中的证书会作为本次检测的信任根，适合本地自签名证书。
不要把 header 中的密钥提交到版本库，优先通过环境变量引用。

### 检测类型和 target

| `type` | `target` 示例 | 成功条件 |
| --- | --- | --- |
| `tcp` | `127.0.0.1:5432` | 任意解析出的 IPv4 或 IPv6 地址可连接。可带 `tcp://` 前缀。 |
| `tcp4` | `localhost:5432` | 解析出的 IPv4 地址可连接。可带 `tcp4://` 前缀。 |
| `tcp6` | `[::1]:5432` | 解析出的 IPv6 地址可连接。可带 `tcp6://` 前缀。 |
| `http` | `http://127.0.0.1:3000/health` | 请求成功且状态码符合要求。 |
| `https` | `https://localhost:3443/health` | TLS 请求成功且状态码符合要求。 |
| `unix` | `unix:///tmp/shop.sock` | 能连接 Unix socket。建议使用绝对路径。 |
| `file` | `tmp/ready` | 文件存在；相对路径从项目根目录解析。可带 `file://` 前缀。 |
| `command` | `pg_isready -h 127.0.0.1` | 在项目根目录执行 `sh -c` 并以状态码 0 退出。 |

command 检测继承该进程的环境，输出会被丢弃，超时会终止整个检测进程组。

## 路径、时长和校验规则

- `project.path` 的相对路径从用户家目录解析。
- `working_directory`、`env_files`、file target 和 `tls_ca` 的相对路径从项目根目录解析。
- Unix socket 建议始终配置绝对路径。
- 项目 ID 和进程名最长 48 个字符，只能包含 ASCII 字母、数字、`_` 和 `-`。
- 时长必须大于零，并且不能超过当前平台计时器可表示的范围。
- HTTP 状态码必须在 100 到 599 之间。
- 未知字段、空命令、空 target、缺失依赖、自依赖和依赖环都会导致校验失败。
