# Nexus Linux 服务端版（无界面）操作手册

> 本手册是通用说明，不绑定具体项目或服务器；示例里的路径、用户名、端口请按实际替换。
> 发布包内路径：`share/README.md`；仓库路径：`packaging/linux/README.md`。

## 1. 这是什么

在 Linux 服务器上用命令管理一组项目/服务，并在开机时：

- 按顺序启动服务；
- 做就绪检查（等待 / TCP / 日志关键字）；
- 未就绪时阻断后续服务（可选）；
- 稳定运行后崩溃自动重启；
- 记录每个服务的状态、PID、日志，并支持启停/重启。

**没有 GUI，也没有内置数据库**：管理入口是 `nexusctl`，常驻进程是 `nexus-boot-runner`，由 systemd 托管。
它不替代 Tomcat/Node/Java 本身，只负责“在正确的时机启动、等待就绪、失败重试、记录状态”。

## 2. 概念

| 概念 | 说明 |
|---|---|
| 项目（project） | 一个名字 + 一个已存在的目录，用于分组；服务未配置 `workdir` 时用项目路径 |
| 服务（service） | 一条启动命令 + 工作目录 + 环境变量 + 开机/顺序/就绪/重启设置 |
| manifest | 由配置生成的 JSON（`manifest.json`），runner 读它启动；**不要手改** |
| runner | 常驻进程，是开机服务的**唯一拥有者**；启停/重启都经它执行 |
| systemd unit | 托管 runner 本身；服务器用 system unit（默认），桌面才用 user unit |

## 3. 环境要求与选包

- Linux **x86_64** 或 **aarch64**（用 `uname -m` 确认）
- **glibc ≥ 2.35**（在 Ubuntu 22.04 上构建/交叉构建）
- **systemd**（user 或 system 模式）
- 不需要 WebKitGTK、Node、数据库；但你的服务自己需要的运行时（Java/Node/Python…）要自行安装

下载对应架构的资产：

| `uname -m` | 资产 |
|---|---|
| `x86_64` | `nexus-<版本>-linux-x86_64.tar.gz` |
| `aarch64` | `nexus-<版本>-linux-aarch64.tar.gz` |

同时下载 `.sha256` 并校验：

```bash
sha256sum -c nexus-<版本>-linux-<架构>.tar.gz.sha256
```

## 4. 安装（服务器：系统级）

无桌面服务器只需要系统级，不需要用户级，也不需要 linger。解压后一条命令：

```bash
tar xzf nexus-<版本>-linux-<架构>.tar.gz
cd nexus-<版本>-linux-<架构>
sudo ./install.sh --system
```

- 不写 `--run-as` 时，systemd unit 不设置 `User=`，runner 与所有服务**以 root 运行**。
  这是最简单的用法，但权限较大。
- 更安全（推荐）：用一个已存在的非 root 用户降权：
  ```bash
  sudo ./install.sh --system --run-as admin
  ```
  `admin` 按实际替换；该用户要能读项目目录并写它的日志/工作目录。
- 安装到：二进制 `/usr/local/bin/`，配置 `/etc/nexus/nexus.toml`，数据 `/var/lib/nexus/`。
- 脚本会立即写入 systemd unit、`enable --now` 并启动 runner。空配置也会常驻，
  之后加服务不用再执行 `boot install`。
- 安装完成后所有命令**不需要 `--system`**：默认就是系统级。

之后想从 root 改成非 root 运行（或换用户），执行一次：

```bash
sudo nexusctl --run-as admin boot install
# 反过来从非 root 改回 root：
sudo nexusctl --run-as root boot install
```

### 4.1 桌面/用户级（可选；服务器忽略）

只有 Linux 桌面、没有 root、想以当前用户运行服务时才用：

```bash
./install.sh --user
nexusctl --user boot install
sudo loginctl enable-linger "$USER"
```

用户级配置在 `~/.config/nexus/nexus.toml`，数据在 `~/.nexus/`，unit 是 `systemctl --user`。
服务器上没有桌面会话，这一套用不上；不需要安装、也不需要理解。

## 5. 五分钟快速开始

假设项目目录 `/srv/app` 已存在，启动命令 `npm run start`。

### 第 1 步：添加项目

```bash
sudo nexusctl project add /srv/app --name app
```

- 目录必须已存在。
- `--name app` 是项目名，后面 start/stop/status 都用它；不写则默认取目录名。
- 如果还没建项目，也可以直接 `nexusctl add /srv/app -c "..." --boot` 自动创建；显式加项目更容易理解。

### 第 2 步：添加服务（指定项目）

```bash
sudo nexusctl service add -p app -n web -c "npm run start" --boot
```

- `-p app`：第 1 步创建的项目名。
- `-n web`：服务名，后面 start/stop/logs 都用它。
- `-c`：启动命令。
- `--boot`：开机自启；runner 正在运行时**立即启动**。
- 要等端口就绪：加 `--ready tcp:127.0.0.1:3000`；不写就是进程存活即就绪。

如果项目名就是目录名，也可以用快捷写法（自动挂到同目录项目）：

```bash
sudo nexusctl add /srv/app -c "npm run start" --boot
```

快捷写法使用默认服务名（与项目名相同）；要指定服务名，或项目用了自定义名字，就用上面的 `service add -p ...`。

只测试命令本身：

```bash
sudo nexusctl service add -p app -n demo -c "sleep 3600" --boot
```

需要工作目录、环境变量、启动顺序、未就绪阻断等进阶选项时，再用完整写法（见 5.2）。

### 第 3 步：查看状态/日志

```bash
sudo nexusctl status                  # 总览：unit、runner、全部服务
sudo nexusctl service list -p app     # 只看 app 项目
sudo nexusctl service show app web    # 启动命令/目录/env/状态/日志路径
sudo nexusctl logs app web            # 最后 200 行
sudo nexusctl logs app web -f         # 持续跟踪（Ctrl-C 退出）
sudo nexusctl logs --runner -f        # 看 runner 自己的日志
```

`status` 输出示例：

```text
unit:    /etc/systemd/system/nexus-boot-runner.service
active:  active
enabled: yes（开机自启）
runner:  pid=1234 mode=system ... reachable
服务: 2 个（ready 1 / stopped 1）
PROJECT  SERVICE  BOOT  ORDER  STATUS   PID   READY
app      web      yes   10     ready    1234  TCP 127.0.0.1:3000
app      worker   no    0      stopped  -     进程存活即可
```

加 `--json` 可输出结构化数据。

在终端里状态会自动上色：`active`/`ready` 绿色、`failed` 红色、`starting` 黄色。
输出到管道/文件，或设置 `NO_COLOR=1` 时自动关闭颜色，不会污染脚本输出。

### 第 4 步：启动/停止/重启

```bash
# 单个服务
sudo nexusctl start app web
sudo nexusctl stop app web
sudo nexusctl restart app web

# 整个项目（按 order 启动、逆序停止；单服务项目用这个就够）
sudo nexusctl start app
sudo nexusctl stop app
sudo nexusctl restart app
```

这些是**立即操作**，不会改变 `--boot` 标记。它们要求 runner 在运行（安装脚本已经启动）。
`service add --boot` 在 runner 运行时也会立即启动服务；不加 `--boot` 的服务加完后用 `start`。

### 第 5 步：开机自启

装机时已经执行过 `boot install`，systemd unit 已启用；不写 `--run-as` 时以 root 运行，
写了则按 unit 里的 `User=` 降权。**不需要每个服务再安装一次**：
只要服务带 `--boot`，下次开机会自动拉起。可以随时确认：

```bash
sudo nexusctl status
systemctl is-enabled nexus-boot-runner.service
```

### 第 6 步：删除

```bash
sudo nexusctl stop app web
sudo nexusctl service remove app web
sudo nexusctl project remove app
```

### 5.1 默认值（不写就用）

| 参数 | 默认值 | 什么时候改 |
|---|---|---|
| `workdir` | 项目路径 | 服务不在项目根目录 |
| `env` | 空 | 需要 NODE_ENV、JAVA_HOME、HOME 等 |
| `boot` | `false` | 要开机自动拉起（加 `--boot`） |
| `order` | `0` | 多个开机服务需要固定先后顺序 |
| `ready` | `none`（进程存活即就绪） | 服务要等端口/日志关键字才算启动完成 |
| `required` | `false` | 该服务未就绪时要阻断后续开机服务 |
| `crash_restart` | `true` | 不想在崩溃后自动重启（加 `--no-crash-restart`） |

### 5.2 什么时候需要进阶参数

```bash
# 服务在自己的子目录
nexusctl service set app web --workdir /srv/app/web

# 等端口起来才算就绪
nexusctl service set app web --ready tcp:127.0.0.1:3000

# 多服务顺序 + 未就绪阻断
nexusctl service set app db --order 10 --required
nexusctl service set app api --order 20 --required

# 加环境变量
nexusctl service set app web --env NODE_ENV=production

# 关闭崩溃自动重启
nexusctl service set app web --no-crash-restart

# 非开机服务：不写 --boot，之后手动启动
nexusctl service add -p app -n tools -c "sleep 3600"
nexusctl start app tools
```

Tomcat / Java / Python 等完整示例见第 10 节。

## 6. 配置

### 6.1 配置位置

| 模式 | 配置文件 | 数据目录 |
|---|---|---|
| 用户级 | `~/.config/nexus/nexus.toml` | `~/.nexus/` |
| 系统级 | `/etc/nexus/nexus.toml` | `/var/lib/nexus/` |

确认当前用的是哪套：

```bash
nexusctl config path          # 默认系统级
nexusctl --user config path   # 桌面用户级
```

也支持覆盖：

```bash
nexusctl --config /path/nexus.toml --data-dir /path/data ...
# 或环境变量 NEXUS_CONFIG / NEXUS_DATA_DIR
```

### 6.2 两种修改方式

- **用 `nexusctl project/service ...`**：自动保存配置、生成 manifest、通知 runner reload。
  副作用：会重写 TOML，手写注释可能丢失。
- **直接编辑 TOML**：完全支持；改完执行 `nexusctl boot sync` 让 runner 重读。

### 6.3 字段参考

`[[projects]]`：

| 字段 | 说明 |
|---|---|
| `name` | 项目名；只允许字母/数字/`-`/`_` |
| `path` | 已存在的绝对路径 |

`[[services]]`：

| 字段 | 说明 |
|---|---|
| `project` / `name` | 所属项目 / 服务名（同样只允许字母/数字/`-`/`_`） |
| `command` | 启动命令；由 `sh -c` 执行，可写管道/重定向/多条命令 |
| `workdir` | 工作目录；省略或空 = 项目路径 |
| `env` | 服务级环境变量，覆盖全局 `[env]` |
| `boot` | 是否在 runner 启动时自动拉起（默认 false） |
| `order` | 启动顺序，小的先启动（默认 0） |
| `ready` | 就绪检查（见下） |
| `required` | 未就绪时是否阻断后续开机服务（默认 false） |
| `crash_restart` | 稳定运行后崩溃是否自动重启（默认 true） |

`[env]`：全局环境变量，叠加在环境快照之上、服务 `env` 之下。

### 6.4 就绪检查

| 写法 | 含义 |
|---|---|
| `none` / 空 | 进程存活即视为就绪 |
| `wait:10` | 启动后等 10 秒 |
| `tcp:127.0.0.1:3000` | 能建立 TCP 连接 |
| `log:Started` | 服务日志里出现关键字（只看本次运行新增内容） |
| JSON | 自定义超时，例如 `{"type":"tcp","host":"127.0.0.1","port":8080,"timeoutSecs":180}` |

`tcp` / `log` 默认超时 60 秒。ARM 或慢机器上启动慢，可改成 JSON 把超时调大。

### 6.5 环境快照

生成 manifest 时会抓取 `PATH`、`HOME`、`USER`、`SHELL`、`LANG`、`JAVA_HOME`、`NVM_DIR`、
`CARGO_HOME`、`GOROOT` 等路径/工具类变量（不包含代理凭据/密钥）。

- 安装/切换 Node、Java 后 PATH 变了：执行 `nexusctl boot sync` 刷新。
- 系统级指定 `--run-as <用户>` 时，manifest 会把 `USER`/`LOGNAME`/`HOME` 指向该用户，
  不会把 root 的身份带进服务；服务自己的 `--env` 仍然优先级最高。

## 7. 命令参考

### 7.0 命令总览

| 操作 | 命令 |
|---|---|
| 总览 | `nexusctl`（不写子命令）或 `status [--json]` |
| 快速加服务 | `add <项目目录> -c <命令> [--name <服务>] [--boot]` |
| 看项目 | `project list` / `project show <项目>` |
| 建/删项目 | `project add <路径> --name <项目>` / `project remove <项目>` |
| 看服务 | `service list [-p <项目>]` / `service show <项目> <服务>` |
| 建/改/删服务 | `service add` / `service set` / `service remove` |
| 启停（快捷） | `start/stop/restart <项目> [服务]` |
| 启停（完整） | `project ...` / `service start/stop/restart ...` |
| 服务日志 | `logs <项目> <服务> [-f]` / `service logs ...` |
| 启动器日志 | `logs --runner [-f]` / `boot runner-log` |
| 开机标记 | `service enable-boot/disable-boot <项目> <服务>` |
| 安装/管理启动器 | `boot install/uninstall/status` |
| 启停启动器 | `boot start/stop/restart/shutdown` |
| 开机启用/禁用 | `boot enable/disable` |
| 配置诊断 | `config path/show/validate` |

### 7.1 全局参数

```text
--config <路径>      指定配置文件
--data-dir <路径>    指定数据目录
--user              以当前用户运行（桌面场景）；默认系统级
--run-as <用户>      系统级 unit 的服务运行用户（安装时需要）
--runner <路径>      覆盖 nexus-boot-runner 路径（默认与 nexusctl 同目录；放在子命令前）
--system            兼容旧脚本；系统级已是默认，可省略
```

### 7.2 项目

```bash
nexusctl project list [--json]
nexusctl project show <项目> [--json]  # 路径 + 服务列表/状态
nexusctl project add <路径> [--name <名字>]
nexusctl project remove <名字>        # 项目下必须没有服务
nexusctl project start <项目>         # 立即启动项目下全部服务
nexusctl project stop <项目>          # 停止项目下全部服务
nexusctl project restart <项目>       # 先停后启
```

### 7.3 服务

```bash
nexusctl status [--json]                     # 总览：unit/runner/服务状态

# 快速加服务：项目名/服务名默认取目录名；runner 在跑时 --boot 会立即启动
nexusctl add <项目目录> -c <命令> [--name <服务>] [--boot] [--ready tcp:host:port]

nexusctl service list [-p <项目>] [--json]   # -p 只看某个项目
nexusctl service show <项目> <服务> [--json]  # 详情：启动命令/目录/env/状态
nexusctl logs <项目> <服务> [--lines 200] [--follow]
nexusctl service logs <项目> <服务> [--lines 200] [--follow]  # 等价写法

# 快捷启停：只给项目=整个项目；再给服务=单个服务
nexusctl start <项目> [服务]
nexusctl stop <项目> [服务]
nexusctl restart <项目> [服务]

# 必填只有 -p / -n / -c；下面方括号里的都是可选项
nexusctl service add -p <项目> -n <服务> -c <命令> \
  [--project-path <项目目录>] [--workdir <目录>] [--env K=V]... \
  [--boot] [--order N] [--ready <检查>] [--required] [--no-crash-restart]
nexusctl service set <项目> <服务> \
  [--command <命令>] [--workdir <目录>] [--env K=V]... [--clear-env] \
  [--order N] [--ready <检查>] \
  [--boot|--no-boot] [--required|--no-required] \
  [--crash-restart|--no-crash-restart]
nexusctl service remove <项目> <服务>   # 运行中的服务要先 stop
nexusctl service start|stop|restart <项目> <服务>
nexusctl service enable-boot|disable-boot <项目> <服务>
```

- `--project-path`：项目不存在时自动创建，省去一条 `project add`。
- `service add --boot` 在 runner 已运行时**会立即启动**新服务；runner 未运行时等下次启动。
- `enable-boot` 只改“下次 runner 启动时是否自动拉起”，不会立刻启动；现在启动用 `start`。
- `disable-boot` 不会停止正在运行的实例。
- 非开机服务也可以写在配置里：runner 会管理它，只是不自动拉起。

### 7.4 开机启动 / systemd

```bash
nexusctl boot install      # 生成 manifest、写 unit、enable --now（首次用）
nexusctl boot start        # 启动已安装的 unit（不重新安装）
nexusctl boot stop         # 停 runner/服务；unit 保留且仍保持开机启用
nexusctl boot restart      # 重启 runner（会重启其管理的服务）
nexusctl boot enable       # 只启用开机自启（不启动 runner）
nexusctl boot disable      # 只禁用开机自启（不停止正在运行的 runner）
nexusctl boot shutdown     # 兼容旧名：停 runner/服务（unit 保留）
nexusctl boot status [--json]  # 等价 nexusctl status（保留旧写法）
nexusctl boot sync         # 配置改了以后重新生成 manifest 并 reload
nexusctl boot logs <项目> <服务> [--lines 200] [--follow]  # 兼容旧名
nexusctl boot runner-log [--lines 200] [--follow]          # runner 自身日志
nexusctl boot unit         # 只打印将写入的 unit（检查/调试，不安装）
nexusctl boot uninstall    # 停用并删除 unit（保留配置与数据）
```

`boot install / start / enable` 的区别：

- `install`：首次安装 unit + 生成 manifest + `enable --now`（启用并启动）；
- `start`：unit 已安装，只是启动它；
- `enable`：只设置开机自启，不启动；
- `stop`：停 runner/服务，但保留 unit 和开机启用；
- `uninstall`：停用并删除 unit。

### 7.5 配置诊断

```bash
nexusctl config path       # 当前模式使用的 config/data/unit 路径
nexusctl config show       # 打印配置原文
nexusctl config validate   # 校验配置
```

默认就是系统级；桌面用户级才需要加 `--user`，例如 `nexusctl --user config path`。

## 8. 开机启动生命周期

### 8.1 系统级 unit（服务器默认）

```bash
sudo nexusctl boot install              # 不降权，以 root 运行
sudo nexusctl --run-as <用户> boot install  # 推荐：降权到非 root 用户
```

- unit 文件：`/etc/systemd/system/nexus-boot-runner.service`
- 指定 `--run-as` 时写 `User=<用户>`；不指定则不写 `User=`，systemd 默认 root。
- 开机时由 `multi-user.target` 启动，不需要 linger。
- 查看：`systemctl status nexus-boot-runner.service`
- 日志：`journalctl -u nexus-boot-runner.service -e`，或 `nexusctl logs --runner -f`

unit 已安装后再执行 `boot install`（升级）会自动从 unit 读取 `User=`，不必重复 `--run-as`。

### 8.2 用户级 unit + linger（桌面可选；服务器忽略）

```bash
nexusctl --user boot install
sudo loginctl enable-linger "$USER"
```

- 没有 linger：只在你登录后（用户管理器启动时）拉起。
- 有 linger：开机即启动用户管理器，再拉起 runner；无人登录也生效。
- 查看：`systemctl --user status nexus-boot-runner.service`
- 日志：`journalctl --user -u nexus-boot-runner.service -e`

### 8.3 修改配置后

- 用 `nexusctl service ...` 修改：自动保存 + 重新生成 manifest + 通知 runner reload。
- `service add --boot` 在 runner 运行时还会立即启动新服务。
- 直接编辑 `nexus.toml`：执行 `nexusctl boot sync`。
- `boot sync` 只更新清单，不会自动启动新加的开机服务；用 `nexusctl start <项目> <服务>`。

### 8.4 升级

1. 用新包覆盖两个二进制（`install.sh` 或手动替换）。
2. `sudo nexusctl boot install`：重写 unit；如果 unit 已在运行，会 **restart** 以加载新版本。
   `User=` 会从旧 unit 自动读取，也可以显式加 `--run-as <用户>`。
3. 注意：restart 会停止当前全部服务（含非开机服务），开机服务随后按 manifest 重新拉起。

### 8.5 停止 / 卸载

```bash
nexusctl stop <项目> <服务>
nexusctl boot shutdown       # 停 runner 和全部服务，unit 保留
nexusctl boot uninstall      # 停 runner、disable、删除 unit；配置/日志保留
```

## 9. 文件与日志

系统级（默认）：

```text
/etc/nexus/nexus.toml                         配置（唯一真相，0600）
/var/lib/nexus/boot/systemd/
├── manifest.json                             生成的启动清单
├── state.json                                runner 写入的服务状态/PID/心跳
├── runner.log                                runner 自己的日志（2MB 轮转）
├── runner.lock                               单实例锁
├── logs/<project>_<service>.log              服务 stdout/stderr（5MB 轮转）
├── commands/                                 nexusctl 投递 start/stop/restart/reload
└── results/                                  runner 回执
```

桌面用户级把 `/etc/nexus` 换成 `~/.config/nexus`、`/var/lib/nexus` 换成 `~/.nexus`。

- `nexusctl logs <项目> <服务>` 读服务日志；`--lines` 控制尾部行数，`-f/--follow` 持续跟踪。
- 日志读取有 8MB 上限（只读末尾），日志再大也不会整份进内存。
- 状态阶段：`pending` / `starting` / `ready` / `failed` / `exited` / `stopped` / `skipped` / `restarting`。
- 服务日志只包含命令的 stdout/stderr；如果服务自己另写日志（如 Tomcat 的 `logs/catalina.out`），
  需要看它自己的文件，或者用前台模式启动让输出进入 Nexus 日志。

## 10. 常见服务示例

> 以下示例都是服务器系统级（默认）。桌面用户级请把 `sudo nexusctl` 换成 `nexusctl --user`，
> 并去掉 `--run-as`（用户级不要用 sudo）。

### 10.1 Tomcat

```bash
# 以 tomcat 用户运行（按实际用户/路径替换）
sudo apt install -y openjdk-17-jre-headless
JAVA_HOME=$(dirname "$(dirname "$(readlink -f "$(command -v java)")")")

sudo chown -R tomcat:tomcat /opt/tomcat
sudo chmod +x /opt/tomcat/bin/*.sh

sudo nexusctl project add /opt/tomcat --name tomcat
sudo nexusctl service add -p tomcat -n tomcat \
  -c "/bin/sh /opt/tomcat/bin/catalina.sh run" \
  --workdir /opt/tomcat \
  --env "JAVA_HOME=$JAVA_HOME" \
  --env "HOME=/home/tomcat" \
  --env "USER=tomcat" \
  --env "LOGNAME=tomcat" \
  --boot --order 10 \
  --ready tcp:127.0.0.1:8080 --required

sudo nexusctl --run-as tomcat boot install
```

说明：

- 用 `catalina.sh run`（前台）而不是 `startup.sh`：日志会进入 Nexus 服务日志；`startup.sh` 会转后台，
  Nexus 日志里基本看不到东西。
- 端口以 `conf/server.xml` 为准（默认 8080）。
- 目录必须对运行用户可写（`logs/temp/work/webapps`），否则启动报权限错误。
- Java 要在 PATH 里或设置 `JAVA_HOME`。

### 10.2 Node / npm

```bash
sudo nexusctl project add /srv/app --name app
sudo nexusctl service add -p app -n web \
  -c "npm run start" \
  --workdir /srv/app/web \
  --env NODE_ENV=production \
  --boot --order 10 --ready tcp:127.0.0.1:3000 --required
sudo nexusctl --run-as app boot install
```

- 服务器上必须先装 Node/npm；用 nvm 时确保 runner 的 PATH 能找到 node，
  必要时给服务加 `--env "PATH=/home/app/.nvm/versions/node/vXX/bin:/usr/local/bin:/usr/bin:/bin"`。
- `npm run start` 会经 `sh -c` 执行。

### 10.3 Java jar

```bash
sudo nexusctl service add -p app -n api \
  -c "/usr/bin/java -jar /srv/app/api.jar" \
  --workdir /srv/app \
  --env "JAVA_HOME=$JAVA_HOME" \
  --boot --order 20 --ready tcp:127.0.0.1:8080 --required
```

### 10.4 Python / 静态 HTTP

```bash
sudo nexusctl service add -p app -n files \
  -c "python3 -m http.server 8000" \
  --workdir /srv/app \
  --boot --order 30 --ready tcp:127.0.0.1:8000
```

### 10.5 多服务顺序与阻断

```bash
# 数据库先起来；没就绪则跳过 api/web
nexusctl service add -p app -n db -c "..." --boot --order 10 --ready tcp:127.0.0.1:5432 --required
nexusctl service add -p app -n api -c "..." --boot --order 20 --ready tcp:127.0.0.1:8080 --required
nexusctl service add -p app -n web -c "..." --boot --order 30 --ready tcp:127.0.0.1:3000
```

## 11. 排错

| 现象 | 原因 / 处理 |
|---|---|
| `Exec format error` | 下载了错误架构的包；`uname -m`，选 x86_64 / aarch64 |
| `GLIBC_2.xx not found` | 系统 glibc 低于 2.35；升级发行版或换匹配的构建 |
| `nexusctl: command not found` | `~/.local/bin` 不在 PATH；用绝对路径或系统级安装 |
| `启动器未运行` / `runner 未运行` | `sudo nexusctl --run-as <用户> boot install`；或 `systemctl status nexus-boot-runner` |
| `linger: off` | 只有桌面用户级需要：`sudo loginctl enable-linger <用户>` |
| 服务一直 `starting` / `failed` | `nexusctl logs <项目> <服务> -f`、`nexusctl logs --runner -f`、`journalctl -u nexus-boot-runner -e`；检查命令、cwd、环境变量、Java/Node 路径、目录权限 |
| 就绪超时 | 端口/关键字不对，或启动慢；用 JSON 就绪检查把 `timeoutSecs` 调大 |
| 端口被占用 | `ss -ltnp \| grep :<端口>`；先停掉旧进程或改端口 |
| 权限错误 | 系统级 unit 用 `--run-as <用户>`；该用户要能写工作目录/日志目录 |
| 停止后服务仍显示失败 | 有进程组 survivor；看日志与 `ps`；`setsid` 逃逸的守护进程需要 systemd cgroup 兜底 |
| 服务没有输出 | 命令自己转后台了（如 `startup.sh`）；改用前台命令（如 `catalina.sh run`） |
| `systemd` 显示 degraded | 可能是其它 unit 失败（如 logrotate）；`systemctl --failed` 单独排查 |
| 配置改了没生效 | CLI 修改会自动同步；手改 TOML 后执行 `nexusctl boot sync` |
| 示例配置启动失败 | 示例里的 `/srv/app`、`npm run start` 是模板；删掉示例/改成真实值 |
| 用户级用 sudo 报错 | 用户级不要用 sudo；服务器用 `--run-as <用户>` |

## 12. 安全与权限

- 尽量让服务以**非 root** 用户运行：系统级 install 时用 `--run-as <用户>`，unit 会设置 `User=`。
- 配置文件始终 0600；数据目录（配置/state/日志/命令）统一 0700。
- systemd unit 设置 `UMask=0077`，runner 新建的 state/服务日志默认 0600。
- `nexusctl logs` 只允许读数据目录 `logs/` 下的直接文件，并拒绝符号链接，避免被日志路径引到系统文件。
- 命令、环境变量和日志可能含敏感信息；不要把密码直接写进命令行，优先用服务 `env`（配置 0600）
  或服务自己的配置文件。
- system 级安装需要 root；unit 文件 root 所有，服务进程按 `User=` 降权。省略 `--run-as` 则以 root 运行，权限更大。
- 端口是否对外开放由服务自身和防火墙决定，Nexus 不修改防火墙。

## 13. 升级、备份、多实例

- 升级：替换二进制（新包 `install.sh`）→ `sudo nexusctl boot install`。restart 会短暂停止全部服务。
- 备份：TOML 配置 + `~/.nexus`（或 `/var/lib/nexus`）即可；manifest/state 是生成物/运行时状态。
- 多实例：用 `--config`/`--data-dir` 或 `NEXUS_CONFIG`/`NEXUS_DATA_DIR` 隔离；
  **同一个数据目录只能有一个 runner**。systemd unit 名固定，不建议在同一用户/系统上装多个系统级实例。

## 14. FAQ

**需要 Node/数据库吗？** 不需要；需要的是 systemd。服务自己需要的运行时（Java/Node/Python）另行安装。

**Windows 和 Linux 共用配置吗？** 不共用；Windows 用 GUI + SQLite，Linux 用 TOML。

**服务开机自启和 `service start` 有什么区别？** `boot=true` 只在 runner 启动时自动拉起；
`service start` 是立即启动。`enable-boot` 不会立刻启动。

**能优雅停止吗？** 停止会结束整个进程组：先 TERM，很快再 KILL，不保证服务完成优雅关机。
对忽略 TERM 的进程会被强制结束；`setsid` 逃逸的进程不在进程组内，unit 停止时由 systemd cgroup 兜底。

**崩溃会自动重启吗？** 稳定运行满 10 秒后崩溃才重启，10 分钟内最多 5 次；
启动失败不会自动重试（避免“改了半截代码 → 无限重启”）。

**日志能看多少？** 服务日志单文件 5MB 轮转，runner 日志 2MB；`nexusctl logs` 默认尾部 200 行，
`-f` 持续跟踪。读取单次最多 8MB，不会整份加载大日志。

**能只看状态机数据吗？** `nexusctl status --json`、`project list --json`、`service list --json`。

**怎么立即启动？** 单个服务 `nexusctl start app web`，整个项目 `nexusctl start app`；都要求 runner 已在运行。
**怎么查看启动命令/日志？** `nexusctl service show app web` 看命令/目录/env/状态，`nexusctl logs app web -f` 看日志。
**最快加一个服务？** `sudo nexusctl add /srv/app -c "npm run start" --boot`。
**boot install、boot start、boot enable 有什么区别？** install 安装+启用+启动；start 只启动已安装 unit；enable 只设开机自启。
**系统级和用户级怎么选？** 无桌面服务器只装系统级，命令默认就是它；桌面才用 `--user`。

**怎么确认 systemd unit 写法？** `nexusctl boot unit` 只打印不安装；可配合 `systemd-analyze verify`。

## 15. 获取帮助

```bash
nexusctl --help
nexusctl project --help
nexusctl service --help
nexusctl boot --help
nexusctl service add --help
```
